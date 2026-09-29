//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! Static type inference for CREATE VIEW output columns.
//!
//! This module resolves the output type of each SELECT target of a CREATE
//! VIEW query once, at creation time, so catalog relations
//! (information_schema.columns, pg_attribute) can report real column types
//! instead of fallback TEXT. It is fully static: it walks the query AST and
//! the live catalog, never executing SQL.

use super::InMemoryCatalog;
use crate::catalog::Catalog;
use crate::{ColumnType, Expression, FromClause, SelectTarget, Statement, Value};

const MAX_VIEW_DEPTH: usize = 8;

struct Scope {
    alias: String,
    cols: Vec<(String, Option<ColumnType>)>,
}

pub(super) fn infer_query_output_types(
    catalog: &InMemoryCatalog,
    query: &Statement,
) -> Vec<Option<ColumnType>> {
    infer_depth(catalog, query, 0, &mut Vec::new())
}

pub(super) fn infer_query_output_names(catalog: &InMemoryCatalog, q: &Statement) -> Vec<String> {
    let tys = infer_depth(catalog, q, 0, &mut Vec::new());
    match q {
        Statement::Select { targets, from, .. } => {
            let scopes = scopes_of(catalog, from.as_ref(), 0);
            let mut names = Vec::new();
            for t in targets {
                push_target_names(t, &scopes, &mut names);
            }
            while names.len() < tys.len() {
                names.push(format!("column{}", names.len() + 1));
            }
            names
        }
        _ => Vec::new(),
    }
}

fn infer_depth(
    catalog: &InMemoryCatalog,
    q: &Statement,
    d: usize,
    vis: &mut Vec<String>,
) -> Vec<Option<ColumnType>> {
    if d > MAX_VIEW_DEPTH {
        return Vec::new();
    }
    match q {
        Statement::Select { targets, from, .. } => {
            let scopes = scopes_of(catalog, from.as_ref(), d);
            let mut out = Vec::new();
            for t in targets {
                target_tys(catalog, t, &scopes, d, vis, &mut out);
            }
            out
        }
        Statement::SetOperation { left, .. } => infer_depth(catalog, left, d + 1, vis),
        Statement::With { body, ctes, .. } => {
            let mut scopes = scopes_of(catalog, None, d);
            for c in ctes {
                let tys = infer_depth(catalog, &c.query, d + 1, vis);
                let names = out_names(&c.query, tys.len());
                scopes.push(Scope {
                    alias: c.name.clone(),
                    cols: names.into_iter().zip(tys).collect(),
                });
            }
            match &**body {
                Statement::Select { targets, from, .. } => {
                    let mut s2 = scopes_of(catalog, from.as_ref(), d);
                    s2.extend(scopes);
                    let mut out = Vec::new();
                    for t in targets {
                        target_tys(catalog, t, &s2, d, vis, &mut out);
                    }
                    out
                }
                other => infer_depth(catalog, other, d + 1, vis),
            }
        }
        Statement::Values(rows) => {
            let w = rows.first().map(Vec::len).unwrap_or(0);
            (0..w)
                .map(|i| {
                    rows.iter().find_map(|r| r.get(i)).and_then(|e| match e {
                        Expression::Literal(v) => lit_ty(v),
                        _ => None,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}
fn scopes_of(catalog: &InMemoryCatalog, from: Option<&FromClause>, d: usize) -> Vec<Scope> {
    let mut out = Vec::new();
    from_scopes(catalog, from, d, &mut out);
    out
}

fn from_scopes(
    catalog: &InMemoryCatalog,
    from: Option<&FromClause>,
    d: usize,
    out: &mut Vec<Scope>,
) {
    let Some(f) = from else {
        return;
    };
    match f {
        FromClause::Table { name, alias } => {
            let eff = alias.clone().unwrap_or_else(|| tbl_alias(name));
            if let Ok(t) = catalog.get_table(name) {
                out.push(Scope {
                    alias: eff,
                    cols: t
                        .columns
                        .iter()
                        .map(|c| (c.name.clone(), Some(c.col_type)))
                        .collect(),
                });
                return;
            }
            if let Some(v) = catalog.get_view(name) {
                let cols = if !v.column_types.is_empty() && v.column_types.len() == v.columns.len()
                {
                    v.columns
                        .iter()
                        .zip(v.column_types.iter())
                        .map(|(n, t)| (n.clone(), *t))
                        .collect()
                } else {
                    v.columns.iter().map(|n| (n.clone(), None)).collect()
                };
                out.push(Scope { alias: eff, cols });
            }
        }
        FromClause::Join { left, right, .. } => {
            from_scopes(catalog, Some(left), d, out);
            from_scopes(catalog, Some(right), d, out);
        }
        FromClause::Subquery {
            statement,
            alias,
            column_aliases,
            ..
        } => {
            let tys = infer_depth(catalog, statement, d + 1, &mut Vec::new());
            let names = if !column_aliases.is_empty() {
                column_aliases.clone()
            } else {
                out_names(statement, tys.len())
            };
            out.push(Scope {
                alias: alias.clone(),
                cols: names.into_iter().zip(tys).collect(),
            });
        }
        FromClause::TableFunction { .. } => {}
    }
}

fn target_tys(
    catalog: &InMemoryCatalog,
    t: &SelectTarget,
    s: &[Scope],
    d: usize,
    vis: &mut Vec<String>,
    out: &mut Vec<Option<ColumnType>>,
) {
    match t {
        SelectTarget::All => {
            for sc in s {
                for (_, ty) in &sc.cols {
                    out.push(*ty);
                }
            }
        }
        SelectTarget::QualifiedStar { qualifier } => {
            if let Some(sc) = s.iter().find(|x| x.alias.eq_ignore_ascii_case(qualifier)) {
                for (_, ty) in &sc.cols {
                    out.push(*ty);
                }
            }
        }
        SelectTarget::Function(n) => out.push(sess_ty(n)),
        SelectTarget::FunctionCall { name, args } => {
            out.push(fn_ty(catalog, name, args, s, d, vis));
        }
        SelectTarget::WindowFunction { name, args, .. } => out.push(win_ty(name, args, s)),
        SelectTarget::Expr { expr, .. } => {
            if let Expression::ColumnRef(n) = expr {
                if let Some(stripped) = n.strip_suffix(".*") {
                    if let Some(sc) = s.iter().find(|x| x.alias.eq_ignore_ascii_case(stripped)) {
                        for (_, ty) in &sc.cols {
                            out.push(*ty);
                        }
                        return;
                    }
                }
            }
            out.push(expr_ty(catalog, expr, s, d, vis));
        }
        SelectTarget::Aliased { target, .. } => target_tys(catalog, target, s, d, vis, out),
    }
}

fn arith_ty(
    catalog: &InMemoryCatalog,
    l: &Expression,
    r: &Expression,
    s: &[Scope],
    d: usize,
    vis: &mut Vec<String>,
) -> Option<ColumnType> {
    let lt = expr_ty(catalog, l, s, d, vis);
    let rt = expr_ty(catalog, r, s, d, vis);
    match (lt, rt) {
        (Some(a), Some(b)) => {
            if is_float(a.type_oid)
                || is_float(b.type_oid)
                || a.type_oid == plomid_types::TypeOid::NUMERIC
                || b.type_oid == plomid_types::TypeOid::NUMERIC
            {
                Some(numeric())
            } else if is_int(a.type_oid) && is_int(b.type_oid) {
                Some(bigint())
            } else {
                None
            }
        }
        (Some(k), None) | (None, Some(k)) => {
            if is_float(k.type_oid) || k.type_oid == plomid_types::TypeOid::NUMERIC {
                Some(numeric())
            } else {
                None
            }
        }
        (None, None) => None,
    }
}

fn bitwise_ty(
    catalog: &InMemoryCatalog,
    l: &Expression,
    r: &Expression,
    s: &[Scope],
    d: usize,
    vis: &mut Vec<String>,
) -> Option<ColumnType> {
    let lt = expr_ty(catalog, l, s, d, vis);
    let rt = expr_ty(catalog, r, s, d, vis);
    match (lt, rt) {
        (Some(a), Some(b)) if a.type_oid == b.type_oid && is_int(a.type_oid) => Some(a),
        (Some(a), _) if is_int(a.type_oid) => Some(a),
        (_, Some(b)) if is_int(b.type_oid) => Some(b),
        _ => None,
    }
}

fn fn_ty(
    catalog: &InMemoryCatalog,
    name: &str,
    args: &[Expression],
    s: &[Scope],
    d: usize,
    vis: &mut Vec<String>,
) -> Option<ColumnType> {
    if let Some(t) = sess_ty(name) {
        return Some(t);
    }
    match name.to_ascii_lowercase().as_str() {
        "count" => Some(bigint()),
        "avg" => Some(numeric()),
        "sum" => match args.first() {
            None | Some(Expression::Star) => None,
            Some(a) => match expr_ty(catalog, a, s, d, vis) {
                Some(t) => Some(match t.type_oid {
                    plomid_types::TypeOid::INT2 | plomid_types::TypeOid::INT4 => bigint(),
                    plomid_types::TypeOid::INT8 => numeric(),
                    _ => t,
                }),
                None => Some(numeric()),
            },
        },
        "min" | "max" => match args.first() {
            None | Some(Expression::Star) => None,
            Some(a) => expr_ty(catalog, a, s, d, vis),
        },
        "string_agg" => Some(text_ty()),
        "array_agg" => None,
        "json_agg" | "jsonb_agg" | "json_object_agg" | "jsonb_object_agg" | "json_arrayagg"
        | "json_objectagg" => Some(jsonb_ty()),
        "row_number" | "rank" | "dense_rank" => Some(bigint()),
        "lag" | "lead" => args.first().and_then(|a| expr_ty(catalog, a, s, d, vis)),
        "coalesce" => {
            for a in args {
                if let Some(t) = expr_ty(catalog, a, s, d, vis) {
                    return Some(t);
                }
            }
            None
        }
        "nullif" => args.first().and_then(|a| expr_ty(catalog, a, s, d, vis)),
        "length" | "char_length" | "character_length" | "octet_length" => Some(int4()),
        "lower" | "upper" | "trim" | "btrim" | "ltrim" | "rtrim" | "substring" | "substr"
        | "replace" | "concat" | "concat_ws" => Some(text_ty()),
        "extract" | "date_part" => Some(float8_ty()),
        _ => None,
    }
}

fn win_ty(name: &str, args: &[Expression], s: &[Scope]) -> Option<ColumnType> {
    match name.to_ascii_lowercase().as_str() {
        "row_number" | "rank" | "dense_rank" => Some(bigint()),
        "lag" | "lead" => match args.first() {
            Some(Expression::ColumnRef(c)) => resolve_col(s, c),
            _ => None,
        },
        _ => None,
    }
}

fn expr_ty(
    catalog: &InMemoryCatalog,
    e: &Expression,
    s: &[Scope],
    d: usize,
    vis: &mut Vec<String>,
) -> Option<ColumnType> {
    match e {
        Expression::ColumnRef(n) => {
            if is_sess(n) {
                return sess_ty(n);
            }
            resolve_col(s, n)
        }
        Expression::Literal(v) => lit_ty(v),
        Expression::Star => None,
        Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b)
        | Expression::Modulo(a, b) => arith_ty(catalog, a, b, s, d, vis),
        Expression::Power(_, _) => Some(numeric()),
        Expression::Concat(_, _) => Some(text_ty()),
        Expression::Equal(_, _)
        | Expression::NotEqual(_, _)
        | Expression::Less(_, _)
        | Expression::LessOrEqual(_, _)
        | Expression::Greater(_, _)
        | Expression::GreaterOrEqual(_, _)
        | Expression::And(_, _)
        | Expression::Or(_, _)
        | Expression::Not(_)
        | Expression::IsNull(_)
        | Expression::IsNotNull(_)
        | Expression::IsDistinctFrom(_, _)
        | Expression::Exists(_)
        | Expression::IsBoolean { .. }
        | Expression::IsJson { .. } => Some(bool_ty()),
        Expression::In { .. } | Expression::Between { .. } | Expression::Like { .. } => {
            Some(bool_ty())
        }
        Expression::Negate(x) => expr_ty(catalog, x, s, d, vis),
        Expression::Cast { type_name, .. } | Expression::TypeCast { type_name, .. } => {
            ColumnType::from_type_name(type_name)
        }
        Expression::Case { whens, default, .. } => {
            for (_, v) in whens {
                if let Some(t) = expr_ty(catalog, v, s, d, vis) {
                    return Some(t);
                }
            }
            if let Some(dd) = default {
                return expr_ty(catalog, dd, s, d, vis);
            }
            None
        }
        Expression::Coalesce(args) => {
            for a in args {
                if let Some(t) = expr_ty(catalog, a, s, d, vis) {
                    return Some(t);
                }
            }
            None
        }
        Expression::NullIf(a, _) => expr_ty(catalog, a, s, d, vis),
        Expression::FunctionCall { name, args, .. } => fn_ty(catalog, name, args, s, d, vis),
        Expression::WindowFunction { name, args, .. } => win_ty(name, args, s),
        Expression::Extract { .. } => Some(float8_ty()),
        Expression::QuantifiedComparison { .. } => Some(bool_ty()),
        Expression::ScalarSubquery(q) => infer_depth(catalog, q, d + 1, vis)
            .into_iter()
            .next()
            .flatten(),
        Expression::DateLiteral(_) => Some(ColumnType::new(
            plomid_types::TypeOid::DATE,
            plomid_types::NO_TYPEMOD,
        )),
        Expression::TimestampLiteral(_) | Expression::TypedTimestampLiteral { .. } => Some(
            ColumnType::new(plomid_types::TypeOid::TIMESTAMP, plomid_types::NO_TYPEMOD),
        ),
        Expression::TimestamptzLiteral(_) | Expression::TypedTimestamptzLiteral { .. } => Some(
            ColumnType::new(plomid_types::TypeOid::TIMESTAMPTZ, plomid_types::NO_TYPEMOD),
        ),
        Expression::TimeLiteral(_) | Expression::TypedTimeLiteral { .. } => Some(ColumnType::new(
            plomid_types::TypeOid::TIME,
            plomid_types::NO_TYPEMOD,
        )),
        Expression::JsonArrow { as_text, .. } => {
            Some(if *as_text { text_ty() } else { jsonb_ty() })
        }
        Expression::JsonSubscript { .. } => Some(jsonb_ty()),
        Expression::ArrayIndex { array, .. } => expr_ty(catalog, array, s, d, vis),
        Expression::RowField { .. } => None,
        Expression::BitAnd(a, b) | Expression::BitOr(a, b) | Expression::BitXor(a, b) => {
            bitwise_ty(catalog, a, b, s, d, vis)
        }
        Expression::ShiftLeft(a, b) | Expression::ShiftRight(a, b) => {
            bitwise_ty(catalog, a, b, s, d, vis)
        }
    }
}

fn out_names(q: &Statement, count: usize) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Statement::Select { targets, .. } = q {
        for t in targets {
            push_names(t, &mut names);
        }
    }
    while names.len() < count {
        names.push(format!("column{}", names.len() + 1));
    }
    names.truncate(count);
    names
}

fn push_names(t: &SelectTarget, out: &mut Vec<String>) {
    match t {
        SelectTarget::All | SelectTarget::QualifiedStar { .. } => {}
        SelectTarget::Function(n) => out.push(n.clone()),
        SelectTarget::FunctionCall { name, .. } => out.push(name.clone()),
        SelectTarget::WindowFunction { name, .. } => out.push(name.to_ascii_lowercase()),
        SelectTarget::Expr { expr, alias } => {
            if let Some(a) = alias {
                out.push(a.clone());
            } else if let Expression::ColumnRef(n) = expr {
                out.push(n.rsplit('.').next().unwrap_or(n).to_string());
            } else {
                out.push("?column?".to_string());
            }
        }
        SelectTarget::Aliased { alias, .. } => out.push(alias.clone()),
    }
}

fn push_target_names(t: &SelectTarget, s: &[Scope], out: &mut Vec<String>) {
    match t {
        SelectTarget::All => {
            for sc in s {
                for (n, _) in &sc.cols {
                    out.push(n.clone());
                }
            }
        }
        SelectTarget::QualifiedStar { qualifier } => {
            if let Some(sc) = s.iter().find(|x| x.alias.eq_ignore_ascii_case(qualifier)) {
                for (n, _) in &sc.cols {
                    out.push(n.clone());
                }
            }
        }
        SelectTarget::Function(n) => out.push(n.clone()),
        SelectTarget::FunctionCall { name, .. } => out.push(name.clone()),
        SelectTarget::WindowFunction { name, .. } => out.push(name.to_ascii_lowercase()),
        SelectTarget::Expr { expr, alias } => {
            if let Some(a) = alias {
                out.push(a.clone());
                return;
            }
            match expr {
                Expression::ColumnRef(n) => out.push(n.rsplit('.').next().unwrap_or(n).to_string()),
                _ => out.push("?column?".to_string()),
            }
        }
        SelectTarget::Aliased { alias, .. } => out.push(alias.clone()),
    }
}

fn bigint() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::INT8, plomid_types::NO_TYPEMOD)
}
fn int4() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::INT4, plomid_types::NO_TYPEMOD)
}
fn numeric() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::NUMERIC, plomid_types::NO_TYPEMOD)
}
fn text_ty() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::TEXT, plomid_types::NO_TYPEMOD)
}
fn bool_ty() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::BOOL, plomid_types::NO_TYPEMOD)
}
fn float8_ty() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::FLOAT8, plomid_types::NO_TYPEMOD)
}
fn jsonb_ty() -> ColumnType {
    ColumnType::new(plomid_types::TypeOid::JSONB, plomid_types::NO_TYPEMOD)
}
fn is_int(o: plomid_types::TypeOid) -> bool {
    matches!(
        o,
        plomid_types::TypeOid::INT2 | plomid_types::TypeOid::INT4 | plomid_types::TypeOid::INT8
    )
}
fn is_float(o: plomid_types::TypeOid) -> bool {
    matches!(
        o,
        plomid_types::TypeOid::FLOAT4 | plomid_types::TypeOid::FLOAT8
    )
}
fn unq(n: &str) -> &str {
    n.rsplit('.').next().unwrap_or(n)
}
fn is_sess(n: &str) -> bool {
    matches!(
        unq(n).to_ascii_lowercase().as_str(),
        "version"
            | "current_database"
            | "current_schema"
            | "current_user"
            | "session_user"
            | "current_schemas"
            | "pg_backend_pid"
            | "current_date"
            | "current_time"
            | "current_timestamp"
            | "now"
    )
}
fn sess_ty(n: &str) -> Option<ColumnType> {
    match unq(n).to_ascii_lowercase().as_str() {
        "current_schemas" => Some(ColumnType::new(
            plomid_types::TypeOid::NAME_ARRAY,
            plomid_types::NO_TYPEMOD,
        )),
        "version" | "current_database" | "current_schema" | "current_user" | "session_user"
        | "pg_backend_pid" | "current_date" | "current_time" | "current_timestamp" | "now" => {
            Some(text_ty())
        }
        _ => None,
    }
}
fn lit_ty(v: &Value) -> Option<ColumnType> {
    match v {
        Value::Null => None,
        // Untyped string literals carry no type of their own; the surrounding
        // expression (e.g. COALESCE(col, 'x')) decides.
        Value::Unknown(_) | Value::Cstring(_) => None,
        other => {
            crate::value_pg_type(other).map(|t| ColumnType::new(t.oid(), plomid_types::NO_TYPEMOD))
        }
    }
}
fn tbl_alias(n: &str) -> String {
    n.rsplit('.').next().unwrap_or(n).to_string()
}
fn resolve_col(scopes: &[Scope], name: &str) -> Option<ColumnType> {
    match name.rsplit_once('.') {
        Some((q, c)) => scopes
            .iter()
            .find(|s| s.alias.eq_ignore_ascii_case(q))
            .and_then(|s| {
                s.cols
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(c))
                    .and_then(|(_, t)| *t)
            }),
        None => {
            let mut f: Option<ColumnType> = None;
            for s in scopes {
                if let Some((_, t)) = s.cols.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
                    if f.is_none() {
                        f = *t;
                    }
                }
            }
            f
        }
    }
}
