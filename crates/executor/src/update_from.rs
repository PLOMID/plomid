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
//! `UPDATE ... FROM` support.
//!
//! The FROM clause of an UPDATE is a *source of rows* for SET / WHERE /
//! RETURNING expression evaluation — never a write target. This module
//! materializes the FROM relation through the shared SELECT/join machinery
//! (no separate FROM executor is introduced) and binds FROM column
//! references into per-row values using the same trick
//! [`crate::dml::substitute_excluded`] uses for `EXCLUDED.*` in ON CONFLICT.
//!
//! This module also owns the reusable indexed-join candidate-discovery
//! primitive used by both `UPDATE ... FROM` and `DELETE ... USING`. That
//! primitive performs join-atom classification and index-backed candidate
//! discovery only; mutation, locking policy, MVCC lifecycle, and row
//! mutation remain in the DML layer.

use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{
    Catalog, Expression, FromClause, QueryResult, SelectTarget, Statement, TableSchema, Value,
};
use plomid_txn::{StorageEngine, StorageEngineTransaction};

use crate::decode_row;
use crate::error::{SqlError, SqlResult};
use crate::row::values_equal;

/// A materialized `UPDATE ... FROM` relation.
pub(crate) struct UpdateFromSource {
    /// Every qualifier that may legally prefix a column of this relation: the
    /// explicit alias, the relation's trailing name, and the full
    /// (schema-qualified) name as written. A reference qualified by one of
    /// these resolves against the materialized source row.
    pub qualifiers: Vec<String>,
    /// Output column names of the materialized relation.
    pub columns: Vec<String>,
    /// Materialized rows, aligned with `columns`.
    pub rows: Vec<Vec<Value>>,
}

/// Every qualifier that may legally prefix a column reference to `from`:
/// the explicit alias (which hides the relation name, matching SQL scoping),
/// the relation's trailing name segment, and the relation's full name as
/// written. Composite (join) relations contribute every member relation's
/// qualifiers.
fn from_relation_qualifiers(from: &FromClause) -> Vec<String> {
    let mut out = Vec::new();
    collect_relation_qualifiers(from, &mut out);
    out
}

fn collect_relation_qualifiers(from: &FromClause, out: &mut Vec<String>) {
    match from {
        FromClause::Table { name, alias } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
            } else {
                if let Some(trailing) = name.rsplit('.').next() {
                    out.push(trailing.to_string());
                }
            }
            out.push(name.clone());
        }
        FromClause::TableFunction { name, alias, .. } => {
            out.push(alias.clone().unwrap_or_else(|| name.clone()));
            out.push(name.clone());
        }
        FromClause::Subquery { alias, .. } => out.push(alias.clone()),
        FromClause::Join { left, right, .. } => {
            collect_relation_qualifiers(left, out);
            collect_relation_qualifiers(right, out);
        }
    }
}

/// Materializes an `UPDATE ... FROM` relation into rows by executing
/// `SELECT * FROM <relation>` through the shared SELECT/join machinery.
pub(crate) fn materialize_from<E: StorageEngine>(
    engine: &mut E,
    catalog: &plomid_sql::InMemoryCatalog,
    clause: &FromClause,
    current_database: &str,
    current_user: &str,
) -> SqlResult<UpdateFromSource> {
    let stmt = Statement::Select {
        targets: vec![SelectTarget::All],
        distinct: false,
        distinct_on: None,
        from: Some(clause.clone()),
        where_expr: None,
        group_by: None,
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };
    match crate::join::execute_statement(
        engine,
        catalog,
        &stmt,
        current_database,
        current_user,
        None,
        0,
    )? {
        QueryResult::Rows { columns, rows, .. } => Ok(UpdateFromSource {
            qualifiers: from_relation_qualifiers(clause),
            columns,
            rows,
        }),
        _ => Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Internal,
            "UPDATE FROM relation did not produce rows",
        ))),
    }
}

// ---------------------------------------------------------------------------
// Shared classification primitives
// ---------------------------------------------------------------------------

/// A prunable source-side equality predicate over a plain FROM relation:
/// `<source-qualified column> = <constant>` (or reversed).
///
/// Only qualified references classify: a bare name binds target-first in the
/// binder (`bind_from_refs`), so `id = 5` restricts the TARGET, not the
/// source — the classifier must agree with the binder, never guess.
#[derive(Clone)]
pub(crate) struct SourcePrune {
    /// Source column position in the materialized row layout.
    pub col_pos: usize,
    /// Column name for index lookup.
    pub column: String,
    /// The constant side of the predicate.
    pub const_expr: Expression,
}

/// A constant expression for probe/prune purposes: no column references at
/// all (literals, session functions, literal arithmetic) and no subqueries.
/// Such an expression evaluates identically for every row, so its value can
/// be computed once. Module-level (extracted from `join_equality_atom`) so
/// both the target-probe classifier and the source-prune classifier share it.
pub(crate) fn is_constant_expr(expr: &Expression) -> bool {
    match expr {
        // Subquery scopes are evaluated by the nested executor and may be
        // correlated; never treat them as constants.
        Expression::Exists(_)
        | Expression::ScalarSubquery(_)
        | Expression::QuantifiedComparison { .. } => false,
        Expression::In { subquery, .. } => subquery.is_none(),
        _ => {
            let mut refs = Vec::new();
            expr.column_refs(&mut refs);
            refs.is_empty()
        }
    }
}

/// Extracts prunable source-side equality predicates from a WHERE conjunction.
/// Conjunct-only (same as target discovery): an OR arm could match rows the
/// pruned materialization never visits, so disjunctions are not descended.
/// Column position uses the schema's column order = `SELECT *` layout.
pub(crate) fn source_prune_predicates(
    where_expr: Option<&Expression>,
    clause: &FromClause,
    catalog: &plomid_sql::InMemoryCatalog,
) -> Vec<SourcePrune> {
    let FromClause::Table { name, alias } = clause else {
        return Vec::new();
    };
    let Ok(schema) = catalog.get_table(name) else {
        return Vec::new();
    };
    // Qualifiers the binder accepts for this relation: alias first (which
    // hides the relation name), then resolved trailing/full name.
    let quals = source_qualifiers(catalog, name, alias.as_deref());
    let mut out = Vec::new();
    if let Some(e) = where_expr {
        classifiable_source_prunes(e, &quals, schema, &mut out);
    }
    out
}

/// Qualifiers the binder accepts for a single plain table relation.
pub(crate) fn source_qualifiers(
    catalog: &plomid_sql::InMemoryCatalog,
    name: &str,
    alias: Option<&str>,
) -> Vec<String> {
    let mut quals = Vec::new();
    if let Some(a) = alias {
        quals.push(a.to_string());
    }
    let resolved = catalog
        .resolve_table_name(name)
        .unwrap_or_else(|_| name.to_string());
    for q in [
        resolved.rsplit('.').next().unwrap_or(&resolved).to_string(),
        resolved.clone(),
    ] {
        if !quals.iter().any(|k| k.eq_ignore_ascii_case(&q)) {
            quals.push(q);
        }
    }
    quals
}

/// Walk conjunction-only WHERE and collect every provably prunable source-side
/// equality atom.
fn classifiable_source_prunes(
    expr: &Expression,
    quals: &[String],
    schema: &TableSchema,
    out: &mut Vec<SourcePrune>,
) {
    match expr {
        Expression::Equal(..) => {
            if let Some(p) = classify_source_prune(expr, quals, schema) {
                out.push(p);
            }
        }
        Expression::And(a, b) => {
            classifiable_source_prunes(a, quals, schema, out);
            classifiable_source_prunes(b, quals, schema, out);
        }
        _ => {}
    }
}

/// Classify one predicate as a prunable source-side equality, if any.
fn classify_source_prune(
    expr: &Expression,
    quals: &[String],
    schema: &TableSchema,
) -> Option<SourcePrune> {
    let Expression::Equal(left, right) = expr else {
        return None;
    };
    for (col_expr, const_expr) in [
        (left.as_ref(), right.as_ref()),
        (right.as_ref(), left.as_ref()),
    ] {
        let Expression::ColumnRef(raw) = col_expr else {
            continue;
        };
        if crate::catalog_fn::is_session_function(raw) {
            continue;
        }
        let Some((qualifier, column)) = raw.rsplit_once('.') else {
            continue; // bare name: binds target-first, never prunes source
        };
        if !quals.iter().any(|k| k.eq_ignore_ascii_case(qualifier)) {
            continue; // unknown qualifier: binder will error, keep its error
        }
        let Some(pos) = schema
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(column))
        else {
            continue; // unknown column: binder will error, keep its error
        };
        if !is_constant_expr(const_expr) {
            continue;
        }
        return Some(SourcePrune {
            col_pos: pos,
            column: schema.columns[pos].name.clone(),
            const_expr: const_expr.clone(),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Shared indexed-join helpers
// ---------------------------------------------------------------------------

/// An equality atom usable for indexed candidate discovery in a joined DML
/// statement's WHERE conjunction.
#[derive(Debug, Clone)]
pub(crate) enum JoinAtom {
    /// `target_column = <source-only expression>`: the source side evaluates
    /// (per source row) to the probe key.
    Probe {
        source_expr: Expression,
        target_column: String,
    },
    /// `target_column = <constant expression>`: a target-side restriction
    /// independent of the source rows. If the target column is indexed this
    /// alone discovers the candidate row set (O(log target)).
    TargetConstant {
        target_column: String,
        const_expr: Expression,
    },
}

/// Whether an evaluated probe-key `Value` can be served by the canonical
/// index encoding without missing an equality the nested loop would find.
///
/// SQL integer equality is integer-coercing (`values_equal` compares
/// numerics), and `index_value_bytes` encodes Int2/Int4/Int8 under one
/// canonical integer tag — so any integer key probes any integer column
/// exactly. Text works the same way (tag + length-prefixed bytes; the
/// nested loop's text equality is byte equality of the same string).
/// Every other family (floats, numerics, temporals, bools, JSON) encodes
/// through the generic text path and can disagree with `values_equal`'s
/// coercion rules across variants, so it is refused — those statements keep
/// the exact nested-loop path.
pub(crate) fn probe_key_is_index_exact(column: &plomid_sql::ColumnDef, key: &Value) -> bool {
    use plomid_types::PgType;
    let Some(pg) = plomid_types::PgType::by_oid(column.col_type.type_oid) else {
        return false;
    };
    match key {
        Value::Int2(_) | Value::Int4(_) | Value::Int8(_) => {
            matches!(pg, PgType::Int2 | PgType::Int4 | PgType::Int8)
        }
        Value::Text(_) => matches!(
            pg,
            PgType::Text | PgType::VarChar | PgType::Name | PgType::Cstring
        ),
        _ => false,
    }
}

/// Canonical index-key encoding for a scalar probe key.
pub(crate) fn index_key_bytes(key: &Value) -> Vec<u8> {
    crate::index::index_value_bytes(key)
}

/// Leading B+Tree prefix for one index and one encoded key.
pub(crate) fn index_probe_prefix(index_name: &str, key: &Value) -> Vec<u8> {
    crate::index::index_value_prefix(index_name, Some(key))
}

// ---------------------------------------------------------------------------
// Join-atom classification (shared by target-side and source-side paths)
// ---------------------------------------------------------------------------

/// Extracts every provably index-safe equality atom from a joined DML
/// statement's WHERE clause.
///
/// The source-side provenance is determined from the materialized source
/// relation (`from`), so this is safe to call for both `UPDATE ... FROM` and
/// `DELETE ... USING`. The resolution mirrors `bind_from_refs` exactly: a
/// reference is source-bound only when the binder will bind it to the source
/// row.
pub(crate) fn join_equality_atoms(
    where_expr: Option<&Expression>,
    from: Option<&UpdateFromSource>,
    target_quals: &[String],
    schema: &TableSchema,
) -> Vec<JoinAtom> {
    // The source side must not reference the target row. Walks *every* column
    // reference (complete traversal, no descending into subqueries — those
    // refuse instead) and classifies each the way `bind_from_refs` will bind
    // it for the current source row: source-bound (constant here),
    // target-bound, or unresolvable (error). Any non-source-bound reference
    // refuses the optimization.
    fn source_only(expr: &Expression, from: &UpdateFromSource, schema: &TableSchema) -> bool {
        let mut refs = Vec::new();
        expr.column_refs(&mut refs);
        refs.iter().all(|name| {
            if crate::catalog_fn::is_session_function(name) {
                return true;
            }
            match name.rsplit_once('.') {
                Some((qualifier, column)) => {
                    // Source-qualified: the binder binds it -> constant.
                    if from
                        .qualifiers
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(qualifier))
                    {
                        return from.columns.iter().any(|fc| {
                            fc.eq_ignore_ascii_case(&format!("{qualifier}.{column}"))
                                || fc.eq_ignore_ascii_case(column)
                        });
                    }
                    // Target-qualified: references the target row -> refuse.
                    // Unknown qualifier: binder would error -> refuse.
                    false
                }
                None => {
                    // Bare name: a target column when the target schema has
                    // it (the binder's bare-name precedence is target-first,
                    // so this is exact even when the source has the same
                    // name); otherwise it must be a source column.
                    let is_target = schema
                        .columns
                        .iter()
                        .any(|c| c.name.eq_ignore_ascii_case(name));
                    if is_target {
                        return false;
                    }
                    from.columns.iter().any(|fc| fc.eq_ignore_ascii_case(name))
                }
            }
        })
    }
    // Classifies one side of an `=` as a target column, if it is one.
    // Resolution mirrors `bind_from_refs` exactly: a qualified reference
    // belongs to the target when its qualifier is a target qualifier and not
    // also a source qualifier (the binder checks source qualifiers first, so
    // a shared qualifier binds to the source); a bare reference is a target
    // column whenever the target schema has it — the binder's bare-name
    // precedence is target-first, so this is exact, including when the same
    // name also exists in the source.
    fn target_col(
        expr: &Expression,
        from: &UpdateFromSource,
        target_quals: &[String],
        schema: &TableSchema,
    ) -> Option<String> {
        let Expression::ColumnRef(name) = expr else {
            return None;
        };
        if crate::catalog_fn::is_session_function(name) {
            return None;
        }
        match name.rsplit_once('.') {
            Some((qualifier, column)) => {
                if from
                    .qualifiers
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(qualifier))
                {
                    return None;
                }
                if !target_quals
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(qualifier))
                {
                    return None;
                }
                schema.column_index(column).ok().map(|_| column.to_string())
            }
            None => {
                let is_target = schema
                    .columns
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(name));
                is_target.then(|| name.clone())
            }
        }
    }
    // Classifies one `Equal` atom. Either orientation is accepted. A
    // constant operand is classified as `TargetConstant` *before* the
    // source-only check — `source_only` is vacuously true for a
    // reference-free expression, and the constant is strictly more
    // selective, so it must win.
    fn classify_equal(
        left: &Expression,
        right: &Expression,
        from: &UpdateFromSource,
        target_quals: &[String],
        schema: &TableSchema,
    ) -> Option<JoinAtom> {
        if let Some(col) = target_col(left, from, target_quals, schema) {
            if is_constant_expr(right) {
                return Some(JoinAtom::TargetConstant {
                    target_column: col,
                    const_expr: right.clone(),
                });
            }
            if source_only(right, from, schema) {
                return Some(JoinAtom::Probe {
                    source_expr: right.clone(),
                    target_column: col,
                });
            }
            return None;
        }
        if let Some(col) = target_col(right, from, target_quals, schema) {
            if is_constant_expr(left) {
                return Some(JoinAtom::TargetConstant {
                    target_column: col,
                    const_expr: left.clone(),
                });
            }
            if source_only(left, from, schema) {
                return Some(JoinAtom::Probe {
                    source_expr: left.clone(),
                    target_column: col,
                });
            }
        }
        None
    }
    // Conjunction-only collection. A nested `OR` refuses the whole
    // optimization (its other arm could match rows the probe never visits),
    // so disjunctions are simply not descended — the caller falls back to
    // the nested loop when no atom is found.
    fn collect(
        expr: &Expression,
        from: &UpdateFromSource,
        target_quals: &[String],
        schema: &TableSchema,
        out: &mut Vec<JoinAtom>,
    ) {
        match expr {
            Expression::Equal(left, right) => {
                if let Some(atom) = classify_equal(left, right, from, target_quals, schema) {
                    out.push(atom);
                }
            }
            Expression::And(a, b) => {
                collect(a, from, target_quals, schema, out);
                collect(b, from, target_quals, schema, out);
            }
            _ => {}
        }
    }
    let Some(from) = from else {
        return Vec::new();
    };
    let Some(where_expr) = where_expr else {
        return Vec::new();
    };
    let mut atoms = Vec::new();
    collect(where_expr, from, target_quals, schema, &mut atoms);
    atoms
}

// ---------------------------------------------------------------------------
// Reusable indexed-join candidate discovery primitive
// ---------------------------------------------------------------------------

/// A candidate target row discovered by the indexed-join primitive, plus the
/// materialized source-row indices that produced it.
///
/// The caller's predicate recheck pairs the row only with these source rows
/// (`None` = every source row — constant-only discovery). The probe key was
/// evaluated from exactly one source row per probe, so a source row with a
/// different key can never satisfy the equality conjunct; provenance only
/// removes pairings that provably cannot match, never rows that might. The
/// full WHERE (including residual conjuncts) is still re-checked. An *empty*
/// `Some` set is meaningful: no source row's key can equal this row's indexed
/// column value, so the row cannot match any pairing.
#[derive(Clone, Debug)]
pub(crate) struct IndexedCandidate {
    /// Target row key (ascending across candidates — deadlock-safe lock order).
    pub row_key: Vec<u8>,
    /// Current committed row bytes (MVCC-visible at discovery time).
    pub row_bytes: Vec<u8>,
    /// `None` = all source rows participate in the recheck (constant-only
    /// discovery); `Some(v)` = exactly these source rows (ascending,
    /// deduplicated; possibly empty — see above).
    pub producing: Option<Vec<usize>>,
}

/// Whether at least one atom actually contributed candidate rows.
#[derive(Clone, Debug)]
pub(crate) struct IndexedDiscoveryOutcome {
    /// Candidate rows (ascending row-key order, deduplicated producers).
    pub candidates: Vec<IndexedCandidate>,
    /// Whether an index probe was actually used. `true` means candidate set
    /// is authoritative (probe is a WHERE conjunct); `false` means fallback.
    pub index_used: bool,
}

/// Index-backed candidate discovery for `UPDATE ... FROM` / `DELETE ... USING`.
///
/// Replaces the O(target × source) nested loop with index probes when the
/// WHERE conjunction holds a usable atom and the target column is served by
/// the existing authoritative single-column index:
///
/// * `TargetConstant` atom (`t.id = 1`): the constant is evaluated once and
///   probed — O(log target), independent of source cardinality. This is the
///   same decision the plain `UPDATE ... WHERE pk = literal` path makes
///   (`indexed_dml_prefix`), extended to evaluate the constant instead of
///   requiring a literal.
/// * `Probe` atom (`t.id = s.id`): source keys are evaluated once per source
///   row and grouped by their canonical encoding; the target index is probed
///   once per DISTINCT key — O(distinct source keys · log target).
/// * Both present, same column: every candidate's value on that column is
///   the constant key (the index is authoritative), so provenance is a
///   single map lookup — no per-source probing at all.
/// * Both present, different columns: the constant fixes the candidate set;
///   the probe intersects it (both atoms are WHERE conjuncts) and records
///   the producing source rows.
///
/// Reuses exactly the access path the plain UPDATE/DELETE statements use:
///
/// ```text
/// key expression (constant: once; join: per source row — evaluated before
///                any target row is touched)
///   → probe_key_is_index_exact family guard (else per-statement fallback)
///   → canonical index encoding (crate::index::index_value_bytes)
///   → B+Tree prefix scan (crate::index::index_value_prefix)
///   → candidate row keys → txn.get (MVCC-visibility filter)
/// ```
///
/// The full original WHERE is still bound and evaluated per candidate by the
/// caller, so the predicate — not the probe — remains authoritative. Rows are
/// returned deduplicated in ascending row-key order, which makes the
/// caller's `lock_rows` acquisition deadlock-free (canonical ascending
/// order, the same convention the existing paths rely on).
///
/// Returns `IndexedDiscoveryOutcome::index_used == false` when the statement
/// must keep the sequential-scan path: no usable atom, no single-column index,
/// a NULL probe key (NULL never equals anything — but a residual/OR arm could
/// match, so refuse rather than guess), or a non-exact probe-key family.
///
/// Each discovered row also carries its *producing* source-row indices so the
/// caller's predicate recheck pairs the row only with source rows that can
/// possibly match it — removing the residual O(matched × source) pairing the
/// nested loop left behind. See [`IndexedCandidate`].
pub(crate) fn indexed_join_candidates<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &plomid_sql::InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    from: &UpdateFromSource,
    where_expr: Option<&Expression>,
    target_quals: &[String],
) -> SqlResult<IndexedDiscoveryOutcome> {
    let atoms = join_equality_atoms(where_expr, Some(from), target_quals, schema);
    // No usable atom (OR shapes, non-constant non-join operands, unresolvable
    // references): the statement keeps the exact nested-loop path, which also
    // surfaces any binder error the classifier declined to guess at.
    if atoms.is_empty() {
        return Ok(IndexedDiscoveryOutcome {
            candidates: Vec::new(),
            index_used: false,
        });
    }
    // A probe key that cannot be served by the canonical index encoding
    // refuses the whole optimization (per-statement fallback), never
    // silently skips rows.
    let family_exact = |target_column: &str, key: &Value| -> SqlResult<bool> {
        let col_index = schema.column_index(target_column)?;
        Ok(probe_key_is_index_exact(&schema.columns[col_index], key))
    };
    // Candidate row keys in ascending order (BTreeMap iteration), each with
    // `None` (constant-only discovery: every source row participates in the
    // caller's predicate recheck) or the exact producing source-row indices.
    let mut candidates: std::collections::BTreeMap<Vec<u8>, Option<Vec<usize>>> =
        std::collections::BTreeMap::new();
    // Whether at least one atom actually probed an index. An empty candidate
    // set is only authoritative when a probe ran (the atom is a conjunct, so
    // nothing can match); when no atom could be applied (no usable index),
    // the statement must fall back to the nested loop instead.
    let mut index_used = false;
    // 1. Target-constant atom: O(log target), source-independent. Wins over
    //    the join probe whenever its column is indexed.
    let mut constant_used = false;
    let mut constant_column: Option<String> = None;
    let mut constant_encoded: Vec<u8> = Vec::new();
    for atom in &atoms {
        let JoinAtom::TargetConstant {
            target_column,
            const_expr,
        } = atom
        else {
            continue;
        };
        let Some(index) = crate::index::constraint_index_for_column(catalog, table, target_column)
        else {
            continue;
        };
        // A constant evaluates identically on any row; an empty row avoids
        // accidental column lookups.
        let empty_row: Vec<Value> = Vec::new();
        let key = match crate::query::evaluate_expression(&empty_row, schema, const_expr) {
            Ok(key) => key,
            // Evaluation error: fall back; the nested loop surfaces the same
            // error through the identical evaluator.
            Err(_) => {
                return Ok(IndexedDiscoveryOutcome {
                    candidates: Vec::new(),
                    index_used: false,
                })
            }
        };
        if matches!(key, Value::Null) || !family_exact(target_column, &key)? {
            return Ok(IndexedDiscoveryOutcome {
                candidates: Vec::new(),
                index_used: false,
            });
        }
        let prefix = index_probe_prefix(&index.name, &key);
        for (_, row_key) in txn.scan(Some(&prefix), Some(&crate::index::prefix_end(&prefix)))? {
            candidates.insert(row_key, None);
        }
        constant_used = true;
        constant_column = Some(target_column.clone());
        constant_encoded = index_key_bytes(&key);
        index_used = true;
        break;
    }
    // 2. Join probe. Source keys are evaluated once per source row and
    //    grouped by canonical encoding, so duplicate keys share one probe.
    //    With no constant atom the probes discover the candidate set; with a
    //    constant atom on the SAME column, every candidate's value on that
    //    column is already the constant key (the index is authoritative), so
    //    provenance is a single map lookup — probing per source row would
    //    return exactly the candidate set for the equal-encoding keys and
    //    nothing for the rest, which the lookup reproduces exactly. With a
    //    constant atom on a DIFFERENT column, the probe intersects the
    //    constant's candidate set (both atoms are WHERE conjuncts).
    for atom in &atoms {
        let JoinAtom::Probe {
            source_expr,
            target_column,
        } = atom
        else {
            continue;
        };
        let Some(index) = crate::index::constraint_index_for_column(catalog, table, target_column)
        else {
            continue;
        };
        let mut by_key: std::collections::BTreeMap<Vec<u8>, (Value, Vec<usize>)> =
            std::collections::BTreeMap::new();
        for (src_index, from_row) in from.rows.iter().enumerate() {
            let key = match crate::query::evaluate_expression(from_row, schema, source_expr) {
                Ok(key) => key,
                Err(_) => {
                    return Ok(IndexedDiscoveryOutcome {
                        candidates: Vec::new(),
                        index_used: false,
                    })
                }
            };
            if matches!(key, Value::Null) || !family_exact(target_column, &key)? {
                return Ok(IndexedDiscoveryOutcome {
                    candidates: Vec::new(),
                    index_used: false,
                });
            }
            by_key
                .entry(index_key_bytes(&key))
                .or_insert_with(|| (key, Vec::new()))
                .1
                .push(src_index);
        }
        if constant_used && constant_column.as_deref() == Some(target_column.as_str()) {
            // Same column: one lookup replaces source-count probes. Empty
            // bucket = no source row can equal the constant key = candidates
            // cannot match any pairing (the recheck must skip them).
            let producers = by_key
                .get(&constant_encoded)
                .map(|(_, srcs)| srcs.clone())
                .unwrap_or_default();
            for slot in candidates.values_mut() {
                *slot = Some(producers.clone());
            }
        } else {
            for (repr, srcs) in by_key.values() {
                let prefix = index_probe_prefix(&index.name, &repr);
                for (_, row_key) in
                    txn.scan(Some(&prefix), Some(&crate::index::prefix_end(&prefix)))?
                {
                    if constant_used {
                        // Intersect: keep only rows the constant atom already
                        // selected; this source row becomes a producer.
                        if let Some(slot) = candidates.get_mut(&row_key) {
                            slot.get_or_insert_with(Vec::new).extend_from_slice(srcs);
                        }
                    } else {
                        candidates
                            .entry(row_key)
                            .or_insert_with(|| Some(Vec::new()))
                            .get_or_insert_with(Vec::new)
                            .extend_from_slice(srcs);
                    }
                }
            }
        }
        index_used = true;
        break;
    }
    if candidates.is_empty() {
        // A probe that ran and found nothing is final (the atom is a
        // conjunct of the WHERE). No applied probe means the discovery
        // cannot answer the predicate — the nested loop must.
        return if index_used {
            Ok(IndexedDiscoveryOutcome {
                candidates: Vec::new(),
                index_used: true,
            })
        } else {
            Ok(IndexedDiscoveryOutcome {
                candidates: Vec::new(),
                index_used: false,
            })
        };
    }
    let mut rows = Vec::with_capacity(candidates.len());
    for (key, mut producing) in candidates {
        // Producers ascend so the caller's first-qualifying-FROM-row pick
        // keeps materialized order; dedup covers duplicate source keys.
        if let Some(ref mut v) = producing {
            v.sort_unstable();
            v.dedup();
        }
        if let Some(bytes) = txn.get(&key)? {
            rows.push(IndexedCandidate {
                row_key: key,
                row_bytes: bytes,
                producing,
            });
        }
    }
    Ok(IndexedDiscoveryOutcome {
        candidates: rows,
        index_used,
    })
}

/// Materializes a FROM/USING relation, pruning the source scan through its
/// own durable index when the WHERE holds a provably safe source-side
/// equality (`src.col = constant`).
///
/// This is source-side indexed-join discovery applied to the source relation
/// itself. It shares the same classification helpers and index primitives as
/// the target-side path above.
///
/// Correctness:
/// * the index is authoritative and prefix-served (B+Tree scan of the exact
///   encoded key = the nested loop's `values_equal` for integer/text),
/// * equal keys map to equal entry prefixes → entries sort by row key → the
///   pruned rows keep the full scan's relative order (first-match semantics),
/// * the constant expression is evaluated once through the source schema and
///   re-checked per row after decode, so evaluator semantics are exact.
///
/// Falls back to the full shared-SELECT materialization when no predicate is
/// prunable, the column has no usable single-column index, the constant is
/// NULL (SQL equality), the key family is not index-exact, or evaluation
/// fails (the binder/nested path surfaces the same error).
pub(crate) fn materialize_from_pruned<E: StorageEngine>(
    engine: &mut E,
    catalog: &plomid_sql::InMemoryCatalog,
    clause: &FromClause,
    where_expr: Option<&Expression>,
    current_database: &str,
    current_user: &str,
) -> SqlResult<UpdateFromSource> {
    let FromClause::Table { name, alias: _ } = clause else {
        return materialize_from(engine, catalog, clause, current_database, current_user);
    };
    let prunes = source_prune_predicates(where_expr, clause, catalog);
    let mut chosen: Option<(plomid_sql::IndexDefinition, SourcePrune, Value)> = None;
    for prune in &prunes {
        // Any plain single-column index on the column serves the probe —
        // unique or not. The prefix scan returns every entry under the key,
        // and equal values sort by row key within the prefix, so the
        // materialized order (and first-match semantics) are identical to
        // the full scan. Expression indexes are excluded: their stored value
        // is the expression result, not the column value.
        let resolved = catalog
            .resolve_table_name(name)
            .unwrap_or_else(|_| name.to_string());
        let index = catalog
            .all_indexes_for_table(&resolved)
            .into_iter()
            .find(|ix| {
                ix.expression.is_none()
                    && crate::index::index_columns(ix) == [prune.column.as_str()]
            });
        let Some(index) = index else {
            continue;
        };
        let Ok(src_schema) = catalog.get_table(name) else {
            continue;
        };
        let empty: Vec<Value> = Vec::new();
        let Ok(key) = crate::query::evaluate_expression(&empty, &src_schema, &prune.const_expr)
        else {
            return materialize_from(engine, catalog, clause, current_database, current_user);
        };
        if matches!(key, Value::Null) {
            continue; // NULL never equals anything; full scan is correct
        }
        if !probe_key_is_index_exact(&src_schema.columns[prune.col_pos], &key) {
            continue;
        }
        chosen = Some((index, prune.clone(), key));
        break;
    }
    let Some((index, prune, key)) = chosen else {
        return materialize_from(engine, catalog, clause, current_database, current_user);
    };
    // Pruned path: prefix-scan the source's own B+Tree for the encoded key.
    let prefix = index_probe_prefix(&index.name, &key);
    let end = crate::index::prefix_end(&prefix);
    let entries = engine.scan(Some(&prefix), Some(&end))?;
    let src_schema = catalog.get_table(name)?.clone();
    let columns: Vec<String> = src_schema.columns.iter().map(|c| c.name.clone()).collect();
    let mut rows = Vec::with_capacity(entries.len());
    for (_, row_key) in entries {
        let Some(bytes) = engine.get(&row_key)? else {
            continue; // index entry without a visible row: skip (MVCC-safe)
        };
        let row = decode_row(&bytes)?;
        // Re-check the constant through `values_equal` (the nested loop's
        // predicate equality): exact coercion semantics, no second evaluator.
        let actual = row.get(prune.col_pos).cloned().ok_or_else(|| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::Corruption,
                "row has fewer columns than catalog",
            ))
        })?;
        if !values_equal(&actual, &key) {
            continue;
        }
        rows.push(row);
    }
    if tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf") {
        tracing::debug!(
            target: "plomid::perf",
            event = "from_materialize_pruned",
            table = %name,
            index = %index.name,
            source_rows_fetched = rows.len(),
        );
    }
    Ok(UpdateFromSource {
        qualifiers: from_relation_qualifiers(clause),
        columns,
        rows,
    })
}

/// Materializes a `DELETE ... USING` relation inside an explicit transaction
/// (no engine access is available there). Plain table relations are read
/// directly through the transaction's own scan API — the same key-range scan
/// plus row decoding the target table uses — so uncommitted changes made
/// earlier in the same transaction are visible. Subqueries, joins, and table
/// functions would need the shared SELECT/join machinery, which requires
/// engine access and is therefore not supported inside explicit transactions.
pub(crate) fn materialize_using_in_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &plomid_sql::InMemoryCatalog,
    clause: &FromClause,
) -> SqlResult<UpdateFromSource> {
    let FromClause::Table { name, alias: _ } = clause else {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Unsupported,
            "subqueries, joins, and table functions in DELETE ... USING inside \
             explicit transactions are not yet supported",
        )));
    };
    let schema = catalog.get_table(name)?;
    let entries = txn.scan(
        Some(format!("{name}:").as_bytes()),
        Some(format!("{name}:\u{10FFFF}").as_bytes()),
    )?;
    let mut rows = Vec::with_capacity(entries.len());
    for (_, bytes) in entries {
        rows.push(crate::encoding::decode_row(&bytes)?);
    }
    Ok(UpdateFromSource {
        qualifiers: from_relation_qualifiers(clause),
        columns: schema.columns.iter().map(|c| c.name.clone()).collect(),
        rows,
    })
}

/// Bind all expressions inside `UPDATE ... RETURNING` targets against the
/// current source row (same resolution rules as SET/WHERE).
pub(crate) fn bind_returning_targets(
    targets: &[SelectTarget],
    from: Option<&UpdateFromSource>,
    target_qualifiers: &[String],
    from_row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Vec<SelectTarget>> {
    targets
        .iter()
        .map(|t| match t {
            SelectTarget::Expr { expr, alias } => Ok(SelectTarget::Expr {
                expr: bind_from_refs(expr, from, target_qualifiers, from_row, schema)?,
                alias: alias.clone(),
            }),
            other => Ok(other.clone()),
        })
        .collect()
}

/// Binds `UPDATE ... FROM` / `DELETE ... USING` column references in an
/// expression to the values of the current source row.
///
/// Resolution rules (PostgreSQL scoping for UPDATE/DELETE):
/// * `q.col` where `q` is a legal qualifier of the source relation (its
///   alias, its trailing name, or its full name as written) -> the source
///   row's `col` value; when the source has no such column the reference is
///   an error;
/// * `q.col` where `q` qualifies the *target* relation -> left for the
///   evaluator, which unqualifies it against the target schema;
/// * a bare `col` that is *not* a target-table column but matches a source
///   column -> the source row's value;
/// * any other qualifier is an **error**. A reference must never silently
///   fall back to the target row: doing so turned a joined predicate into a
///   tautology and made `UPDATE ... FROM` / `DELETE ... USING` rewrite or
///   empty whole tables.
///
/// Subqueries are copied without descending - their column references belong
/// to a nested scope.
pub(crate) fn bind_from_refs(
    expr: &Expression,
    from: Option<&UpdateFromSource>,
    target_qualifiers: &[String],
    from_row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Expression> {
    let Some(from) = from else {
        // No source relation: identity copy (fast path for plain UPDATE /
        // DELETE, whose qualified references are ordinary target references).
        return Ok(expr.clone());
    };
    macro_rules! bind2 {
        ($V:ident, $a:expr, $b:expr) => {
            Expression::$V(
                Box::new(bind_from_refs(
                    &**$a,
                    Some(from),
                    target_qualifiers,
                    from_row,
                    schema,
                )?),
                Box::new(bind_from_refs(
                    &**$b,
                    Some(from),
                    target_qualifiers,
                    from_row,
                    schema,
                )?),
            )
        };
    }
    macro_rules! bind1 {
        ($V:ident, $a:expr) => {
            Expression::$V(Box::new(bind_from_refs(
                &**$a,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?))
        };
    }
    match expr {
        Expression::ColumnRef(name) => {
            // Split at the LAST dot: `schema.table.column` qualifies on
            // `schema.table`, `table.column` on `table`.
            let (qualifier, column) = match name.rsplit_once('.') {
                Some((q, c)) => (Some(q), c),
                None => (None, name.as_str()),
            };
            if let Some(qualifier) = qualifier {
                // 1. Source-qualified reference: bind to the source row.
                if from
                    .qualifiers
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(qualifier))
                {
                    let qualified = format!("{qualifier}.{column}");
                    let position = from
                        .columns
                        .iter()
                        .position(|fc| fc.eq_ignore_ascii_case(&qualified))
                        .or_else(|| {
                            from.columns
                                .iter()
                                .position(|fc| fc.eq_ignore_ascii_case(column))
                        });
                    return match position {
                        Some(i) => Ok(Expression::Literal(from_row[i].clone())),
                        None => Err(undefined_column(
                            name,
                            "the source relation has no such column",
                        )),
                    };
                }
                // 2. Target-qualified reference: the ordinary evaluator
                //    already unqualifies it against the target schema.
                if target_qualifiers
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(qualifier))
                {
                    return Ok(expr.clone());
                }
                // 3. Unknown qualifier: never fall back to the target row.
                return Err(undefined_column(
                    name,
                    &format!(
                        "qualifier \"{qualifier}\" names neither the target relation nor a \
                         source relation"
                    ),
                ));
            }
            // Bare reference: target columns take precedence, matching the
            // ordinary UPDATE/DELETE resolution rules; otherwise a source
            // column of that name supplies the value.
            let is_target_column = schema
                .columns
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(name));
            if !is_target_column {
                if let Some(i) = from
                    .columns
                    .iter()
                    .position(|fc| fc.eq_ignore_ascii_case(name))
                {
                    return Ok(Expression::Literal(from_row[i].clone()));
                }
            }
            Ok(expr.clone())
        }
        Expression::Literal(v) => Ok(Expression::Literal(v.clone())),
        Expression::Star => Ok(Expression::Star),
        Expression::Equal(a, b) => Ok(bind2!(Equal, a, b)),
        Expression::NotEqual(a, b) => Ok(bind2!(NotEqual, a, b)),
        Expression::Less(a, b) => Ok(bind2!(Less, a, b)),
        Expression::LessOrEqual(a, b) => Ok(bind2!(LessOrEqual, a, b)),
        Expression::Greater(a, b) => Ok(bind2!(Greater, a, b)),
        Expression::GreaterOrEqual(a, b) => Ok(bind2!(GreaterOrEqual, a, b)),
        Expression::And(a, b) => Ok(bind2!(And, a, b)),
        Expression::Or(a, b) => Ok(bind2!(Or, a, b)),
        Expression::Add(a, b) => Ok(bind2!(Add, a, b)),
        Expression::Subtract(a, b) => Ok(bind2!(Subtract, a, b)),
        Expression::Multiply(a, b) => Ok(bind2!(Multiply, a, b)),
        Expression::Divide(a, b) => Ok(bind2!(Divide, a, b)),
        Expression::Modulo(a, b) => Ok(bind2!(Modulo, a, b)),
        Expression::Concat(a, b) => Ok(bind2!(Concat, a, b)),
        Expression::Power(a, b) => Ok(bind2!(Power, a, b)),
        Expression::BitAnd(a, b) => Ok(bind2!(BitAnd, a, b)),
        Expression::BitOr(a, b) => Ok(bind2!(BitOr, a, b)),
        Expression::BitXor(a, b) => Ok(bind2!(BitXor, a, b)),
        Expression::ShiftLeft(a, b) => Ok(bind2!(ShiftLeft, a, b)),
        Expression::ShiftRight(a, b) => Ok(bind2!(ShiftRight, a, b)),
        Expression::IsDistinctFrom(a, b) => Ok(bind2!(IsDistinctFrom, a, b)),
        Expression::NullIf(a, b) => Ok(bind2!(NullIf, a, b)),
        Expression::IsNull(inner) => Ok(bind1!(IsNull, inner)),
        Expression::IsNotNull(inner) => Ok(bind1!(IsNotNull, inner)),
        Expression::Not(inner) => Ok(bind1!(Not, inner)),
        Expression::Negate(inner) => Ok(bind1!(Negate, inner)),
        Expression::In {
            expr,
            list,
            subquery,
            negated,
        } => Ok(Expression::In {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            list: list
                .iter()
                .map(|e| bind_from_refs(e, Some(from), target_qualifiers, from_row, schema))
                .collect::<SqlResult<Vec<_>>>()?,
            subquery: subquery.as_ref().map(|s| (**s).clone()).map(Box::new),
            negated: *negated,
        }),
        Expression::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(Expression::Between {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            low: Box::new(bind_from_refs(
                low,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            high: Box::new(bind_from_refs(
                high,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            negated: *negated,
        }),
        Expression::Like {
            expr,
            pattern,
            escape,
            negated,
        } => Ok(Expression::Like {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            pattern: Box::new(bind_from_refs(
                pattern,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            escape: *escape,
            negated: *negated,
        }),
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            let operand = match operand.as_deref() {
                Some(o) => Some(Box::new(bind_from_refs(
                    o,
                    Some(from),
                    target_qualifiers,
                    from_row,
                    schema,
                )?)),
                None => None,
            };
            let mut new_whens = Vec::with_capacity(whens.len());
            for (c, v) in whens {
                new_whens.push((
                    bind_from_refs(c, Some(from), target_qualifiers, from_row, schema)?,
                    bind_from_refs(v, Some(from), target_qualifiers, from_row, schema)?,
                ));
            }
            let default = match default.as_deref() {
                Some(d) => Some(Box::new(bind_from_refs(
                    d,
                    Some(from),
                    target_qualifiers,
                    from_row,
                    schema,
                )?)),
                None => None,
            };
            Ok(Expression::Case {
                operand,
                whens: new_whens,
                default,
            })
        }
        Expression::Coalesce(args) => Ok(Expression::Coalesce(
            args.iter()
                .map(|a| bind_from_refs(a, Some(from), target_qualifiers, from_row, schema))
                .collect::<SqlResult<Vec<_>>>()?,
        )),
        Expression::Exists(stmt) => Ok(Expression::Exists(Box::new((**stmt).clone()))),
        Expression::ScalarSubquery(stmt) => {
            Ok(Expression::ScalarSubquery(Box::new((**stmt).clone())))
        }
        Expression::IsJson {
            expr,
            kind,
            negated,
        } => Ok(Expression::IsJson {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            kind: *kind,
            negated: *negated,
        }),
        Expression::IsBoolean {
            expr,
            kind,
            negated,
        } => Ok(Expression::IsBoolean {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            kind: *kind,
            negated: *negated,
        }),
        Expression::QuantifiedComparison {
            left,
            operator,
            quantifier,
            subquery,
        } => Ok(Expression::QuantifiedComparison {
            left: Box::new(bind_from_refs(
                left,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            operator: *operator,
            quantifier: *quantifier,
            subquery: subquery.clone(),
        }),
        Expression::WindowFunction { name, args, over } => Ok(Expression::WindowFunction {
            name: name.clone(),
            args: args
                .iter()
                .map(|a| bind_from_refs(a, Some(from), target_qualifiers, from_row, schema))
                .collect::<SqlResult<Vec<_>>>()?,
            over: over.clone(),
        }),
        Expression::Cast { expr, type_name } => Ok(Expression::Cast {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            type_name: type_name.clone(),
        }),
        Expression::Extract { field, expr } => Ok(Expression::Extract {
            field: field.clone(),
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
        }),
        Expression::RowField { expr, field } => Ok(Expression::RowField {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            field: field.clone(),
        }),
        Expression::TypeCast { expr, type_name } => Ok(Expression::TypeCast {
            expr: Box::new(bind_from_refs(
                expr,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            type_name: type_name.clone(),
        }),
        Expression::FunctionCall {
            name,
            args,
            distinct,
            filter,
            order_by,
            returning,
            null_handling,
            unique_keys,
        } => {
            let filter = match filter.as_deref() {
                Some(f) => Some(Box::new(bind_from_refs(
                    f,
                    Some(from),
                    target_qualifiers,
                    from_row,
                    schema,
                )?)),
                None => None,
            };
            Ok(Expression::FunctionCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| bind_from_refs(a, Some(from), target_qualifiers, from_row, schema))
                    .collect::<SqlResult<Vec<_>>>()?,
                distinct: *distinct,
                filter,
                order_by: order_by.clone(),
                returning: returning.clone(),
                null_handling: *null_handling,
                unique_keys: *unique_keys,
            })
        }
        Expression::JsonArrow {
            left,
            right,
            as_text,
        } => Ok(Expression::JsonArrow {
            left: Box::new(bind_from_refs(
                left,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            right: Box::new(bind_from_refs(
                right,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            as_text: *as_text,
        }),
        Expression::ArrayIndex { array, index } => Ok(Expression::ArrayIndex {
            array: Box::new(bind_from_refs(
                array,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            index: Box::new(bind_from_refs(
                index,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
        }),
        Expression::JsonSubscript { array, index } => Ok(Expression::JsonSubscript {
            array: Box::new(bind_from_refs(
                array,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
            index: Box::new(bind_from_refs(
                index,
                Some(from),
                target_qualifiers,
                from_row,
                schema,
            )?),
        }),
        Expression::DateLiteral(s) => Ok(Expression::DateLiteral(s.clone())),
        Expression::TimestampLiteral(s) => Ok(Expression::TimestampLiteral(s.clone())),
        Expression::TimestamptzLiteral(s) => Ok(Expression::TimestamptzLiteral(s.clone())),
        Expression::TimeLiteral(s) => Ok(Expression::TimeLiteral(s.clone())),
        Expression::TypedTimestampLiteral { text, precision } => {
            Ok(Expression::TypedTimestampLiteral {
                text: text.clone(),
                precision: *precision,
            })
        }
        Expression::TypedTimestamptzLiteral { text, precision } => {
            Ok(Expression::TypedTimestamptzLiteral {
                text: text.clone(),
                precision: *precision,
            })
        }
        Expression::TypedTimeLiteral { text, precision } => Ok(Expression::TypedTimeLiteral {
            text: text.clone(),
            precision: *precision,
        }),
    }
}

/// An unresolvable column reference in a joined DML statement. Raised instead
/// of silently reinterpreting the reference as a target-table column.
fn undefined_column(name: &str, reason: &str) -> SqlError {
    SqlError::Storage(PlomidError::with_detail(
        ErrorKind::NotFound,
        format!("column \"{name}\" does not exist"),
        reason.to_string(),
    ))
}

/// The qualifiers that legally prefix a column reference to the target
/// relation: the explicit alias when one was given (which hides the relation
/// name), otherwise the trailing name; the full name as written is accepted
/// too.
pub(crate) fn target_qualifiers(table: &str, alias: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(alias) = alias {
        out.push(alias.to_string());
    } else if let Some(trailing) = table.rsplit('.').next() {
        out.push(trailing.to_string());
    }
    out.push(table.to_string());
    out
}
