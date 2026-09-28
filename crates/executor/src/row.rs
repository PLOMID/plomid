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
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::{Catalog, ColumnRule, InMemoryCatalog, TableSchema, Value};
use plomid_txn::StorageEngineTransaction;

use crate::coerce::number_f64;
use crate::encoding::decode_row;
use crate::error::{SqlError, SqlResult};
use crate::util::unqualify;

/// Extract a best-effort `i64` from a numeric runtime value.
///
/// Used only for `min_integer` check-constraint enforcement below; returns
/// `None` for non-numeric values so those columns skip the integer check.
/// This is deliberately a read-only inspection helper: it never mutates the
/// row and never performs type coercion (coercion lives in
/// `coerce_value_for_column` / `apply_defaults_and_validate`).
fn number_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int2(v) => Some(i64::from(*v)),
        Value::Int4(v) => Some(i64::from(*v)),
        Value::Int8(v) => Some(*v),
        Value::Float4(v) => Some(i64::from(*v as i32)),
        Value::Float8(v) => Some(*v as i64),
        Value::Numeric(n) => n.clone().to_i64(),
        _ => None,
    }
}

/// Map an explicit `INSERT` value list onto full table-column order.
///
/// With an explicit column list, omitted positions stay `NULL` and are marked
/// unprovided so `apply_defaults_and_validate` fills their `DEFAULT`s; without
/// a list every position counts as provided (PostgreSQL positional semantics).
/// Callers must still run `apply_defaults_and_validate` afterwards: that pass
/// performs per-column coercion (explicit values) plus DEFAULT substitution
/// (omitted columns), NOT NULL checks, and the final `validate_row` pass.
pub fn resolve_insert_values(
    schema: &TableSchema,
    rules: &[ColumnRule],
    columns: Option<Vec<String>>,
    values: Vec<Value>,
) -> SqlResult<Vec<Value>> {
    let col_count = schema.columns.len();
    if let Some(col_names) = columns {
        if col_names.len() != values.len() {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                "column count does not match value count",
            )));
        }
        let mut row = vec![Value::Null; col_count];
        let mut provided = vec![false; col_count];
        for (i, col_name) in col_names.iter().enumerate() {
            let idx = schema.column_index(unqualify(col_name))?;
            row[idx] = values[i].clone();
            provided[idx] = true;
        }
        apply_defaults_and_validate(schema, rules, &mut row, Some(&provided))?;
        Ok(row)
    } else {
        let mut row = values;
        let provided = vec![true; row.len()];
        apply_defaults_and_validate(schema, rules, &mut row, Some(&provided))?;
        Ok(row)
    }
}

/// Validate + normalize one INSERT row against the table schema.
///
/// This is the single funnel for all INSERT paths (explicit column lists,
/// positional VALUES, `DEFAULT VALUES`, COPY). Per column it:
/// 1. coerces explicit values to the declared column type + typmod
///    (`coerce_value_for_column`),
/// 2. substitutes `DEFAULT` for omitted columns and coerces the default
///    through that *same* path (the bug fixed here: literal `'active'`
///    parses as TEXT and must become VARCHAR(30) before validation),
/// 3. enforces NOT NULL / check constraints,
/// then recomputes generated columns and runs the final `validate_row` pass.
/// Callers distinguish "provided" from "omitted" via the `provided` bitmap;
/// `None`/all-true means every cell is explicit (positional semantics).
pub fn apply_defaults_and_validate(
    schema: &TableSchema,
    rules: &[ColumnRule],
    row: &mut [Value],
    provided: Option<&[bool]>,
) -> SqlResult<()> {
    if row.len() != schema.columns.len() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!(
                "column count mismatch: expected {}, got {}",
                schema.columns.len(),
                row.len()
            ),
        )));
    }
    for (index, value) in row.iter_mut().enumerate() {
        let rule = rules.get(index).cloned().unwrap_or_default();
        if rule.generated_expr.is_some() {
            // Generated columns are computed from the final row below; any
            // supplied value is ignored (PostgreSQL rejects explicit writes
            // to generated columns, so there is nothing meaningful to keep).
            continue;
        }
        // Coerce the supplied value first. Explicit INSERT values (e.g.
        // `'active'` parsed as TEXT) are normalized here to the declared
        // column type (e.g. VARCHAR(30)) with typmod enforcement.
        coerce_value_for_column(schema, index, value)?;
        // Fill omitted columns from their DEFAULT. In PostgreSQL the default
        // expression is evaluated and then *assigned* to the column, so it
        // must travel through the exact same coercion path as an explicit
        // value. Without this, a literal default such as `'active'` (TEXT)
        // would fail validation against a VARCHAR column with:
        //   column "status" expects varchar but received active.
        if matches!(value, Value::Null) && !provided.is_some_and(|flags| flags[index]) {
            if let Some(default) = rule.default_value {
                // Literal DEFAULT (parsed from e.g. `DEFAULT 'active'` as
                // TEXT, `DEFAULT 100` as INT, `DEFAULT TRUE` as BOOL).
                *value = default;
                // Re-coerce so the default lands in the declared column type
                // (TEXT -> VARCHAR, INT -> NUMERIC(p,s), etc.) with length /
                // precision / scale checks applied.
                coerce_value_for_column(schema, index, value)?;
            } else if let Some(expr) = rule.default_expr {
                // Non-literal DEFAULT (e.g. CURRENT_TIMESTAMP): evaluate in
                // the statement execution context with an empty row.
                let _ctx = crate::context::StatementContext::enter_if_none();
                *value = crate::query::evaluate_expression(&[], schema, &expr)?;
                // Same rule as above: the evaluated default is an untyped
                // runtime value until coerced (e.g. TIMESTAMP value into a
                // TIMESTAMP(p) column so temporal precision/typmod is
                // respected). Reuses the normal column coercion path.
                coerce_value_for_column(schema, index, value)?;
            }
        }
        if rule.not_null && matches!(value, Value::Null) {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                format!(
                    "null value in column \"{}\" violates not-null constraint",
                    schema.columns[index].name
                ),
            )));
        }
        if let Some(minimum) = rule.min_integer {
            let below = match value {
                Value::Numeric(n) => n.clone().to_f64() < minimum as f64,
                Value::Float4(n) => f64::from(*n) < minimum as f64,
                Value::Float8(n) => *n < minimum as f64,
                _ => number_i64(value).is_some_and(|actual| actual < minimum),
            };
            if below {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Conflict,
                    format!(
                        "value in column \"{}\" violates check constraint",
                        schema.columns[index].name
                    ),
                )));
            }
        }
    }
    apply_generated_columns(schema, rules, row)?;
    schema.validate_row(row)?;
    Ok(())
}

/// Computes every `GENERATED ALWAYS AS (expr) STORED` column from the current
/// row using the normal expression evaluator (`query::evaluate_expression`),
/// then coerces and not-null-validates the computed cells. Called after the
/// ordinary column values (and defaults) have been finalized, so generation
/// expressions observe the row's final content.
pub fn apply_generated_columns(
    schema: &TableSchema,
    rules: &[ColumnRule],
    row: &mut [Value],
) -> SqlResult<()> {
    for (index, rule) in rules.iter().enumerate() {
        let Some(expr) = rule.generated_expr.as_ref() else {
            continue;
        };
        let mut value = crate::query::evaluate_expression(row, schema, expr)?;
        coerce_value_for_column(schema, index, &mut value)?;
        if rule.not_null && matches!(value, Value::Null) {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                format!(
                    "null value in generated column \"{}\" violates not-null constraint",
                    schema.columns[index].name
                ),
            )));
        }
        row[index] = value;
    }
    Ok(())
}

pub fn coerce_value_for_column(
    schema: &TableSchema,
    index: usize,
    value: &mut Value,
) -> SqlResult<()> {
    if matches!(value, Value::Null) {
        return Ok(());
    }
    let column_oid = schema.columns[index].col_type.type_oid;
    let column_pg = plomid_types::PgType::by_oid(column_oid);
    let value_pg = plomid_sql::value_pg_type(value);
    if value_pg != column_pg && value_pg.is_some() {
        // PostgreSQL allows assigning a float literal to an integer column
        // (the literal's value is truncated). Do the same here before falling
        // back to text-roundtripping.
        if let Some(num) = number_f64(value) {
            if let Some(target) = column_pg {
                if matches!(
                    target,
                    plomid_types::PgType::Int2
                        | plomid_types::PgType::Int4
                        | plomid_types::PgType::Int8
                ) {
                    *value = numeric_to_integer(value, num, target)?;
                    return Ok(());
                }
            }
        }
        let text = value.to_sql_text();
        *value = plomid_types::text::parse_value_oid(&text, column_oid).map_err(|e| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!("column \"{}\": {e}", schema.columns[index].name),
            ))
        })?;
    }
    // Apply the column's declared type modifier for temporal types so stored
    // values honor the declared fractional-second precision (e.g. a TIME(0) /
    // TIMESTAMP(3) column normalizes input to that precision, exactly as
    // PostgreSQL does on insert). This applies regardless of whether the value
    // was already a temporal runtime value or was text-parsed just above.
    if let Some(pg) = column_pg {
        let typmod = schema.columns[index].col_type.typmod;
        if typmod != plomid_types::typmod::NO_TYPEMOD
            && matches!(
                pg,
                plomid_types::PgType::Time
                    | plomid_types::PgType::TimeTz
                    | plomid_types::PgType::Timestamp
                    | plomid_types::PgType::Timestamptz
            )
        {
            *value = plomid_types::apply_typmod(value.clone(), typmod, pg).map_err(|e| {
                SqlError::Storage(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    format!("column \"{}\": {e}", schema.columns[index].name),
                ))
            })?;
        }
    }
    Ok(())
}

fn numeric_to_integer(value: &Value, num: f64, target: plomid_types::PgType) -> SqlResult<Value> {
    let truncated = num.trunc();
    if !truncated.is_finite() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("value {value:?} is out of range for integer type"),
        )));
    }
    let out_of_range: fn(f64) -> bool = match target {
        plomid_types::PgType::Int2 => |t: f64| t < f64::from(i16::MIN) || t > f64::from(i16::MAX),
        plomid_types::PgType::Int4 => |t: f64| t < f64::from(i32::MIN) || t > f64::from(i32::MAX),
        _ => |t: f64| t < i64::MIN as f64 || t > i64::MAX as f64,
    };
    if out_of_range(truncated) {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::InvalidArgument,
            format!("value {value:?} is out of range for type {target}"),
        )));
    }
    let as_i64 = truncated as i64;
    Ok(match target {
        plomid_types::PgType::Int2 => Value::Int2(as_i64 as i16),
        plomid_types::PgType::Int4 => Value::Int4(as_i64 as i32),
        _ => Value::Int8(as_i64),
    })
}

pub fn validate_updated_row(
    schema: &TableSchema,
    rules: &[ColumnRule],
    row: &[Value],
) -> SqlResult<()> {
    schema.validate_row(row)?;
    for (index, rule) in rules.iter().enumerate() {
        if rule.not_null && matches!(row.get(index), Some(Value::Null)) {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                format!(
                    "null value in column \"{}\" violates not-null constraint",
                    schema.columns[index].name
                ),
            )));
        }
        if let Some(minimum) = rule.min_integer {
            if let Some(actual) = row.get(index).and_then(number_i64) {
                if actual < minimum {
                    return Err(SqlError::Storage(PlomidError::new(
                        ErrorKind::Conflict,
                        format!(
                            "value in column \"{}\" violates check constraint",
                            schema.columns[index].name
                        ),
                    )));
                }
            }
        }
    }
    Ok(())
}

pub fn enforce_unique_values_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    rules: &[ColumnRule],
    row: &[Value],
    ignore_key: Option<&[u8]>,
) -> SqlResult<()> {
    // A constrained column backed by an authoritative unique index is checked
    // by `stage_index_puts` with an exact index probe on the proposed value.
    // Only the remaining columns need the scan, so a table whose constraints
    // are all index-backed never walks the table at all.
    let uncovered = rules
        .iter()
        .enumerate()
        .filter(|(index, rule)| {
            (rule.unique || rule.primary_key)
                && schema.columns.get(*index).is_some_and(|column| {
                    crate::index::constraint_index_for_column(catalog, table, &column.name)
                        .is_none()
                })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if uncovered.is_empty() {
        return Ok(());
    }
    let entries = txn.scan(
        Some(format!("{table}:").as_bytes()),
        Some(format!("{table}:\u{10FFFF}").as_bytes()),
    )?;
    for (key, bytes) in entries {
        if ignore_key.is_some_and(|ignored| ignored == key.as_slice()) {
            continue;
        }
        let existing = decode_row(&bytes)?;
        for index in &uncovered {
            if existing.get(*index) == row.get(*index)
                && !matches!(row.get(*index), Some(Value::Null))
            {
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Conflict,
                    format!(
                        "duplicate key value violates unique constraint on \"{}\"",
                        schema.columns[*index].name
                    ),
                )));
            }
        }
    }
    Ok(())
}

pub fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        _ => {
            if let (Some(a), Some(b)) = (number_f64(left), number_f64(right)) {
                return a == b;
            }
            let sk = left.sort_key().zip(right.sort_key());
            match sk {
                Some((a, b)) if a == b => true,
                _ => left.to_sql_text() == right.to_sql_text(),
            }
        }
    }
}

/// Enforces FOREIGN KEY constraints for a proposed row before it is written.
///
/// For each FK on `schema`: extract local column values; under MATCH SIMPLE,
/// a NULL in any local column skips the check; otherwise scan the referenced
/// table for a row whose referenced columns all equal the local values.
/// Returns SQLSTATE 23503 (via message keyword `foreign`) when unmatched.
pub fn enforce_foreign_keys_txn<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    row: &[Value],
) -> SqlResult<()> {
    for constraint in schema.foreign_keys() {
        let (ref_table, ref_columns, _on_delete, _on_update, match_type) = match &constraint.kind {
            plomid_sql::ConstraintKind::ForeignKey {
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                match_type,
            } => (ref_table, ref_columns, on_delete, on_update, match_type),
            _ => continue,
        };
        // Resolve local column indices.
        let mut local_indices = Vec::with_capacity(constraint.columns.len());
        for name in &constraint.columns {
            let idx = schema.column_index(crate::util::unqualify(name))?;
            local_indices.push(idx);
        }
        let local_values: Vec<&Value> = local_indices.iter().map(|&i| &row[i]).collect();
        // MATCH SIMPLE: any NULL local value skips this FK check.
        let is_simple = matches!(match_type, plomid_sql::ForeignKeyMatch::Simple);
        if is_simple && local_values.iter().any(|v| v.is_null()) {
            continue;
        }
        // MATCH FULL: all-NULL also passes; mixed NULL/NOT NULL fails.
        if !is_simple {
            let nulls = local_values.iter().filter(|v| v.is_null()).count();
            if nulls == local_values.len() {
                continue;
            }
            if nulls > 0 {
                let cname = constraint
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{table}_fk"));
                return Err(SqlError::Storage(PlomidError::new(
                    ErrorKind::Conflict,
                    format!(
                        "insert or update on table \"{table}\" violates foreign key constraint \"{cname}\""
                    ),
                )));
            }
        }
        if ref_columns.len() != local_indices.len() {
            continue;
        }
        let ref_schema = catalog.get_table(ref_table)?;
        // Resolve the referenced table to its catalog-stored (possibly
        // schema-qualified) name so the storage scan prefix matches.
        let resolved_ref = catalog.resolve_table_name(ref_table)?;
        let mut ref_indices = Vec::with_capacity(ref_columns.len());
        for name in ref_columns {
            let idx = ref_schema.column_index(crate::util::unqualify(name))?;
            ref_indices.push(idx);
        }
        let prefix = format!("{resolved_ref}:");
        let end = format!("{resolved_ref}:\u{10FFFF}");
        let mut found = false;
        for (_key, bytes) in txn
            .scan(Some(prefix.as_bytes()), Some(end.as_bytes()))
            .map_err(SqlError::Storage)?
        {
            let existing = decode_row(&bytes)?;
            let matches = local_indices
                .iter()
                .zip(ref_indices.iter())
                .all(|(&li, &ri)| values_equal(&row[li], &existing[ri]));
            if matches {
                found = true;
                break;
            }
        }
        if !found {
            let cname = constraint
                .name
                .clone()
                .unwrap_or_else(|| format!("{table}_fk"));
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                format!(
                    "insert or update on table \"{table}\" violates foreign key constraint \"{cname}\""
                ),
            )));
        }
    }
    Ok(())
}

/// Hashes a canonical equality signature for `row`.
///
/// The signature is *implied* by [`values_equal`]: any two rows it calls equal
/// hash the same, so a `HashMap` keyed by this value can bucket duplicate
/// candidates without ever missing one. It is deliberately not equivalent to
/// [`values_equal`], which merges values through three branches — numeric
/// comparison, `sort_key` equality, and a textual fallback — that do not form a
/// transitive relation and therefore cannot be represented by a single key.
///
/// The two branches reachable for values of one column type are covered
/// exactly: numeric-typed values are canonicalized through their `f64` value
/// (so `Int8(1)`, `Float8(1.0)` and `Numeric(1.00)` agree) and every other value
/// through its SQL text. The textual fallback can additionally merge values of
/// *different* type families in the same position (for example `Bool(true)` and
/// `Text("true")`, or `Date(10)` and `Int4(10)` through their shared
/// `sort_key`). Callers must therefore treat a signature match as a filter and
/// confirm it with [`values_equal`], and must treat a signature *miss* as
/// "no duplicate here" only for uniformly typed positions — which is why
/// [`crate::distinct::apply_distinct`] documents that alongside its use.
#[must_use]
pub fn equality_signature(row: &[Value]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.len().hash(&mut hasher);
    for value in row {
        match value {
            Value::Null => 0_u8.hash(&mut hasher),
            other => match number_f64(other) {
                Some(number) => {
                    1_u8.hash(&mut hasher);
                    // `-0.0` and `0.0` compare equal, so they must hash alike.
                    // `NaN` compares unequal to everything including itself, so
                    // sharing a signature is harmless: the confirming
                    // comparison rejects it and both rows are kept.
                    let canonical = if number == 0.0 { 0.0 } else { number };
                    canonical.to_bits().hash(&mut hasher);
                }
                None => {
                    2_u8.hash(&mut hasher);
                    other.to_sql_text().hash(&mut hasher);
                }
            },
        }
    }
    hasher.finish()
}
