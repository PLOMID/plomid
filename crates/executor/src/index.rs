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
use plomid_sql::{Catalog, InMemoryCatalog, IndexDefinition, TableSchema, Value};
use plomid_txn::StorageEngineTransaction;

use crate::error::{SqlError, SqlResult};

/// Deterministic catalog name of the index that backs the single-column
/// PRIMARY KEY / UNIQUE constraint on `table`.`column`.
///
/// Constraint indexes reuse the ordinary index representation, so they are
/// maintained by the same `stage_index_puts`/`stage_index_deletes` paths as a
/// user-created index, are cleaned up by the same DROP TABLE/TRUNCATE paths,
/// and are persisted in the catalog exactly like any other index. The name is
/// derived from the table so that two tables may both have a primary key.
pub fn constraint_index_name(table: &str, column: &str) -> String {
    format!("{table}__{column}_key")
}

/// Deterministic catalog name of the composite index that backs the
/// multi-column `UNIQUE (a, b, ...)` / `PRIMARY KEY (a, b, ...)` constraint on
/// `table`. Derived from the table plus the ordered column list, so it is
/// stable across restart and cannot collide with a single-column constraint
/// index (`t__a_key` vs `t__a_b_key`).
pub fn composite_constraint_index_name(table: &str, columns: &[String]) -> String {
    format!("{table}__{}_key", columns.join("_"))
}

/// The ordered indexed columns of `index`.
///
/// `columns` is authoritative; `column` is the single-column mirror every
/// legacy single-column path reads. Expression indexes have no column list.
pub fn index_columns(index: &IndexDefinition) -> &[String] {
    if index.columns.is_empty() {
        std::slice::from_ref(&index.column)
    } else {
        &index.columns
    }
}

/// One unique conflict domain that a statement can move, and how to compute
/// its value from a proposed row.
///
/// `Some(position)` — the domain value is the row's stored column at that
/// position (a plain single-column index). `None` — the domain value is the
/// evaluated `index.expression` (an expression unique index). Both use the
/// exact same canonical encoding (`index_value_bytes`) as the durable index.
pub(crate) type ReservationTarget = (Option<usize>, IndexDefinition);

/// Every unique conflict domain on `table` that per-key reservations can
/// cover, or `None` when the table has a domain they cannot:
///
/// * a uniqueness rule (`UNIQUE` / `PRIMARY KEY`) with no backing single-column
///   unique index — only the scan path enforces it, and no reservation can
///   close its probe-to-publication window;
/// * a composite (multi-column) unique index — its domain is the whole tuple,
///   which a single-column reservation cannot represent;
/// * a FOREIGN KEY — the referenced table is not part of this table's
///   reservation domain.
///
/// Callers that get `None` keep the conservative table-wide write lane.
pub(crate) fn unique_reservation_targets(
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    rules: &[plomid_sql::ColumnRule],
) -> Option<Vec<ReservationTarget>> {
    let mut targets: Vec<ReservationTarget> = Vec::new();
    // Every uniqueness rule must be answered by its own backing index.
    for (position, rule) in rules.iter().enumerate() {
        if !(rule.unique || rule.primary_key) {
            continue;
        }
        let column = schema.columns.get(position)?;
        let backing = constraint_index_for_column(catalog, table, &column.name)?;
        targets.push((Some(position), backing));
    }
    // Every other single-column unique index enforces a conflict domain of
    // its own: an expression unique index, or a unique index created directly
    // by the user over a column that has no UNIQUE constraint. Constraint
    // indexes are included here so a composite one still disables the
    // reservation path (its domain is the whole tuple).
    for index in catalog.all_indexes_for_table(table) {
        if !index.unique {
            continue;
        }
        let columns = index_columns(&index);
        if columns.len() > 1 {
            return None;
        }
        if targets
            .iter()
            .any(|(_, existing)| existing.name == index.name)
        {
            continue;
        }
        let position = if index.expression.is_some() {
            None
        } else {
            match schema.column_index(&columns[0]) {
                Ok(position) => Some(position),
                // An index over a dropped column is inert: it reads no rows.
                Err(_) => continue,
            }
        };
        targets.push((position, index));
    }
    if schema.constraints.iter().any(|constraint| {
        matches!(
            constraint.kind,
            plomid_sql::ConstraintKind::ForeignKey { .. }
        )
    }) {
        return None;
    }
    Some(targets)
}

/// The reservation key a proposed row contributes to one target, or `None`
/// when the row cannot conflict (a NULL domain value never conflicts).
pub(crate) fn reservation_key_for_target(
    target: &ReservationTarget,
    row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Option<(String, Vec<u8>)>> {
    let (position, index) = target;
    let value = match position {
        Some(position) => row.get(*position).cloned(),
        None => match &index.expression {
            Some(expr) => Some(crate::query::evaluate_expression(row, schema, expr)?),
            None => None,
        },
    };
    Ok(match value {
        Some(value) if !value.is_null() => Some((index.name.clone(), index_value_bytes(&value))),
        _ => None,
    })
}

/// The reservations an UPDATE must hold for one row: exactly the domains whose
/// value the statement changed. The old value needs no reservation (the row
/// lock on the updated row covers its release); a NULL new value never
/// conflicts.
pub(crate) fn changed_reservation_keys(
    targets: &[ReservationTarget],
    old_row: &[Value],
    new_row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Vec<(String, Vec<u8>)>> {
    let mut keys = Vec::new();
    for (_, index) in targets {
        let old_values = index_values_for_row(index, old_row, schema)?;
        let new_values = index_values_for_row(index, new_row, schema)?;
        if tuples_equal(&old_values, &new_values) || !tuple_conflicts_under_unique(&new_values) {
            continue;
        }
        keys.push((index.name.clone(), index_value_bytes(&new_values[0])));
    }
    Ok(keys)
}

/// True when the assignment list can move a value in some unique index on
/// `table`. The domain is read from the *indexes* (the enforcement
/// authority), so a unique index the user created directly is covered even
/// when no column rule marks its column unique.
///
/// An expression unique index's key depends on the whole row, so any
/// assignment may move it.
pub(crate) fn assignment_moves_unique_domain(
    catalog: &InMemoryCatalog,
    table: &str,
    assigned_columns: &[String],
) -> bool {
    catalog
        .all_indexes_for_table(table)
        .into_iter()
        .any(|index| {
            if !index.unique {
                return false;
            }
            if index.expression.is_some() {
                return true;
            }
            index_columns(&index)
                .iter()
                .any(|column| assigned_columns.contains(column))
        })
}

/// Returns the unique catalog index that authoritatively answers "does a row
/// with this value already exist in `table`.`column`" and "which rows have
/// this value".
///
/// Composite constraints and expression indexes are never returned: only a
/// plain, single-column unique index can answer both questions for one column
/// value.
pub fn constraint_index_for_column(
    catalog: &InMemoryCatalog,
    table: &str,
    column: &str,
) -> Option<IndexDefinition> {
    let resolved = catalog
        .get_table(table)
        .map(|schema| schema.name.clone())
        .unwrap_or_else(|_| table.to_string());
    catalog
        .all_indexes_for_table(&resolved)
        .into_iter()
        .find(|index| {
            index.unique
                && index.expression.is_none()
                && index_columns(index) == [column.to_string()]
        })
}

/// Registers the backing index for every single-column PRIMARY KEY / UNIQUE
/// constraint of `table` that does not already have one.
///
/// Must only be called while the table is provably empty (freshly created),
/// so the new index needs no backfill. A column that already has a unique
/// index, or whose derived constraint-index name is taken by a user index, is
/// left alone: the scan-based uniqueness path still covers it.
pub fn register_constraint_indexes(
    catalog: &mut InMemoryCatalog,
    table: &str,
) -> Result<Vec<String>, plomid_core::PlomidError> {
    let schema = catalog.get_table(table)?.clone();
    let rules = catalog.column_rules(table)?;
    let mut created = Vec::new();
    // Composite constraints are registered first: their backing index is the
    // tuple authority for `(a, b)`, and `column_rules` deliberately does not
    // mark their columns individually unique.
    for constraint in &schema.constraints {
        let is_unique = matches!(
            constraint.kind,
            plomid_sql::ConstraintKind::Unique | plomid_sql::ConstraintKind::PrimaryKey
        );
        if !is_unique || constraint.columns.len() < 2 {
            continue;
        }
        let name = composite_constraint_index_name(&schema.name, &constraint.columns);
        if catalog.index(&name).is_some() {
            continue;
        }
        catalog.create_composite_unique_index(
            name.clone(),
            schema.name.clone(),
            constraint.columns.clone(),
        )?;
        created.push(name);
    }
    for (index, rule) in rules.iter().enumerate() {
        if !(rule.unique || rule.primary_key) {
            continue;
        }
        let Some(column) = schema.columns.get(index) else {
            continue;
        };
        if constraint_index_for_column(catalog, table, &column.name).is_some() {
            continue;
        }
        let name = constraint_index_name(&schema.name, &column.name);
        if catalog.index(&name).is_some() {
            continue;
        }
        // A constraint index, not a user index: it enforces the constraint
        // only, so it stays out of the derived index views.
        catalog.create_constraint_index(
            name.clone(),
            schema.name.clone(),
            column.name.clone(),
            true,
        )?;
        created.push(name);
    }
    Ok(created)
}

pub fn index_value_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Null => vec![0],
        // Equality in SQL is type-coercing for the integer family.  Store all
        // integer index keys in one canonical representation so a BIGINT
        // value inserted as Int8 is still found by an Int4 query literal.
        Value::Int2(number) => {
            let mut bytes = vec![1];
            bytes.extend_from_slice(&i64::from(*number).to_be_bytes());
            bytes
        }
        Value::Int4(number) => {
            let mut bytes = vec![1];
            bytes.extend_from_slice(&i64::from(*number).to_be_bytes());
            bytes
        }
        Value::Int8(number) => {
            let mut bytes = vec![1];
            bytes.extend_from_slice(&number.to_be_bytes());
            bytes
        }
        Value::Text(text) => {
            let mut bytes = vec![2];
            bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
        other => {
            let mut bytes = vec![3];
            let text = other.to_sql_text();
            bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
    }
}

pub fn index_value_prefix(index_name: &str, value: Option<&Value>) -> Vec<u8> {
    let mut key = format!("__plomid_index:{index_name}:").into_bytes();
    if let Some(value) = value {
        key.extend_from_slice(&index_value_bytes(value));
        key.push(0);
    }
    key
}

/// The canonical byte string of a composite index key: the per-component
/// canonical encodings (`index_value_bytes`) concatenated in column order.
///
/// Every `index_value_bytes` result is self-delimiting (1 tag byte plus a
/// fixed-width integer payload, or a 4-byte length plus payload), so the
/// concatenation is unambiguous: two different tuples can never produce the
/// same byte string, and a prefix of one tuple is never a prefix of another
/// with a different first component.
pub fn index_tuple_bytes(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in values {
        out.extend_from_slice(&index_value_bytes(value));
    }
    out
}

/// The B+Tree key prefix of a *complete* index tuple (used for exact lookup
/// and for the uniqueness probe of a proposed tuple).
pub fn index_tuple_prefix(index_name: &str, values: &[Value]) -> Vec<u8> {
    let mut key = format!("__plomid_index:{index_name}:").into_bytes();
    if !values.is_empty() {
        key.extend_from_slice(&index_tuple_bytes(values));
        key.push(0);
    }
    key
}

/// The B+Tree key prefix of a *leading* part of an index tuple (used for
/// range scans such as `WHERE first_col = literal` over a composite index).
///
/// No terminator is appended, so the prefix matches every tuple whose leading
/// components are `values`. Encoded tags are never `0xff`, so `prefix_end`
/// bounds the scan exactly.
pub fn index_leading_prefix(index_name: &str, values: &[Value]) -> Vec<u8> {
    let mut key = format!("__plomid_index:{index_name}:").into_bytes();
    key.extend_from_slice(&index_tuple_bytes(values));
    key
}

pub fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    end.push(0xff);
    end
}

/// A single-column comparison the durable index can answer directly as a
/// B+Tree key range instead of a full table scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexRangeOp {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// The integer payload of `value` under the index's canonical encoding, or
/// `None` when `value` is not an integer-family value.
///
/// [`index_value_bytes`] writes the integer family as a tag byte plus a
/// big-endian `i64`, which is the property the range bounds below depend on:
/// byte order equals numeric order, so a numeric range maps exactly onto a
/// contiguous B+Tree key range. Text and every other type use a length-prefixed
/// encoding that is *not* order-preserving, so they are deliberately rejected
/// and their callers keep the scan fallback.
fn index_integer_bytes(value: &Value) -> Option<Vec<u8>> {
    let bytes = index_value_bytes(value);
    (bytes.len() == 9 && bytes[0] == 1).then_some(bytes)
}

/// Byte bounds `[start, end)` of every index entry whose indexed value
/// satisfies `value <op> literal`.
///
/// Entry keys are `namespace + value_bytes + 0 + row_key`, so a comparison on
/// the value maps onto the key range as follows (`N` = the index namespace,
/// `E(v)` = the canonical bytes of `v`):
///
/// ```text
///  v >= lit   [ N + E(lit) + 0x00 , N + 0xff )
///  v >  lit   [ N + E(lit) + 0xff , N + 0xff )
///  v <= lit   [ N                       , N + E(lit) + 0xff )
///  v <  lit   [ N                       , N + E(lit) + 0x00 )
/// ```
///
/// Equality needs no arm of its own: the caller expresses `v = lit` as
/// `v >= lit AND v <= lit`, whose intersection is exactly the entries carrying
/// `lit`.
///
/// The bounds are exact for the value comparison, so the returned entries are a
/// superset only of what the predicate needs — never narrower than it. The
/// statement's residual `WHERE` evaluation still runs, so a bound that admits a
/// boundary row the predicate rejects is harmless.
///
/// Returns `None` for any index or literal whose encoding is not
/// order-preserving, which leaves the caller's scan path unchanged.
pub fn index_range_bounds(
    index_name: &str,
    op: IndexRangeOp,
    literal: &Value,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let encoded = index_integer_bytes(literal)?;
    let namespace = index_value_prefix(index_name, None);
    // End of the whole index namespace: every entry key is below this.
    let namespace_end = prefix_end(&namespace);
    match op {
        IndexRangeOp::GreaterOrEqual => {
            let mut start = namespace;
            start.extend_from_slice(&encoded);
            start.push(0);
            Some((start, namespace_end))
        }
        IndexRangeOp::Greater => {
            let mut start = namespace;
            start.extend_from_slice(&encoded);
            // Strictly above every entry carrying this value: those keys are
            // `N + E(lit) + 0x00 + row_key`, all below this bound.
            start.push(0xff);
            Some((start, namespace_end))
        }
        IndexRangeOp::LessOrEqual => {
            let mut end = namespace.clone();
            end.extend_from_slice(&encoded);
            end.push(0xff);
            Some((namespace, end))
        }
        IndexRangeOp::Less => {
            let mut end = namespace.clone();
            end.extend_from_slice(&encoded);
            end.push(0);
            Some((namespace, end))
        }
    }
}

/// The canonical bytes of one literal coerced to an integer, or `None`.
///
/// The index stores integer columns canonically, but a comparison literal may
/// still arrive as text (an untyped parameter or a quoted literal). Accepting
/// both keeps the range path available for parameterized statements without
/// changing what the predicate means: the text is parsed to the same numeric
/// value the index encoded, and anything unparseable simply keeps the scan
/// fallback.
pub fn integer_literal_bytes(literal: &Value) -> Option<Vec<u8>> {
    index_integer_bytes(literal).or_else(|| {
        let text = match literal {
            Value::Text(text) | Value::Unknown(text) => text.clone(),
            other => other.to_sql_text(),
        };
        text.trim()
            .parse::<i64>()
            .ok()
            .and_then(|parsed| index_integer_bytes(&Value::Int8(parsed)))
    })
}

/// Byte bounds `[start, end)` of every index entry carrying exactly `literal`
/// for a text equality predicate (`column = 'literal'`).
///
/// Text entries use a length-prefixed encoding that is deliberately *not*
/// order-preserving, so ranges stay unsupported — but equality names one
/// exact key prefix (`namespace + value_bytes + 0x00`, entries being
/// `namespace + value_bytes + 0x00 + row_key`). The literal is spelled **in
/// the column's own value representation** through the same
/// [`index_value_bytes`] function the writer uses, so the spelling always
/// agrees with the stored entries: `TEXT` columns hold `Text` spellings
/// (tag 2), `VARCHAR`/`NAME` columns hold their own spellings (tag 3), and
/// write-time type strictness keeps each column uniform. Requirements:
///
/// * plain single-column index (checked by the caller);
/// * the indexed column is `TEXT`, `VARCHAR`, or `NAME`. `BPCHAR` is excluded
///   — the scalar path resolves padded comparisons through casts, which byte
///   prefixes cannot reproduce;
/// * both bounds carry an equal text-family literal (`Text`/`VarChar`/`Name`
///   /`Unknown`; content bytes are what is compared). `NULL` never matches
///   and declines, as does every other type.
///
/// Returns `None` whenever any requirement fails; the caller keeps its
/// existing integer path or the full-scan fallback, so behavior is unchanged
/// outside the proven shape.
pub fn text_equality_bounds(
    catalog: &InMemoryCatalog,
    table: &str,
    column: &str,
    index_name: &str,
    low: &Value,
    high: &Value,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if !crate::row::values_equal(low, high) {
        return None;
    }
    let content = match low {
        Value::Text(text) | Value::VarChar(text) | Value::Name(text) | Value::Unknown(text) => text,
        _ => return None,
    };
    let schema = catalog.get_table(table).ok()?;
    let position = schema.column_index(column).ok()?;
    let spelled = match schema.columns.get(position)?.col_type.type_oid {
        plomid_types::TypeOid::TEXT => Value::Text(content.clone()),
        plomid_types::TypeOid::VARCHAR => Value::VarChar(content.clone()),
        plomid_types::TypeOid::NAME => Value::Name(content.clone()),
        _ => return None,
    };
    let start = index_tuple_prefix(index_name, std::slice::from_ref(&spelled));
    let mut stem = format!("__plomid_index:{index_name}:").into_bytes();
    stem.extend_from_slice(&index_tuple_bytes(std::slice::from_ref(&spelled)));
    let end = prefix_end(&stem);
    (start <= end).then_some((start, end))
}

/// Spells a text-family literal as the column's stored representation.
///
/// Index entries carry proposed values in the column's own variant (`Text`
/// for `TEXT` columns, `VarChar`/`Name`/`Unknown` spellings for theirs),
/// while SQL literals arrive in whatever spelling the parser produced. A
/// prefix built from the raw literal can therefore miss entries that are
/// present, which downstream callers would read as "no rows". Normalizing to
/// the column spelling keeps every spelling consistent. Non-text values pass
/// through unchanged (integers are canonically encoded either way).
pub fn spell_like_column(
    catalog: &InMemoryCatalog,
    table: &str,
    column: &str,
    value: &Value,
) -> Value {
    let content = match value {
        Value::Text(text) | Value::VarChar(text) | Value::Name(text) | Value::Unknown(text) => text,
        _ => return value.clone(),
    };
    let spelled = (|| {
        let schema = catalog.get_table(table).ok()?;
        let position = schema.column_index(column).ok()?;
        Some(match schema.columns.get(position)?.col_type.type_oid {
            plomid_types::TypeOid::TEXT => Value::Text(content.clone()),
            plomid_types::TypeOid::VARCHAR => Value::VarChar(content.clone()),
            plomid_types::TypeOid::NAME => Value::Name(content.clone()),
            _ => return None,
        })
    })();
    spelled.unwrap_or_else(|| value.clone())
}

/// Byte bounds for a comparison whose literal may be text-encoded.
///
/// Identical to [`index_range_bounds`] except that the literal is first
/// coerced through [`integer_literal_bytes`].
pub fn index_range_bounds_coerced(
    index_name: &str,
    op: IndexRangeOp,
    literal: &Value,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if let Some((start, end)) = index_range_bounds(index_name, op, literal) {
        return Some((start, end));
    }
    let parsed = integer_literal_bytes(literal)?;
    // Re-run the bound construction with the canonical integer value so the
    // encoding used for the bounds is the one the entries were written with.
    let value = Value::Int8(i64::from_be_bytes(parsed[1..9].try_into().ok()?));
    index_range_bounds(index_name, op, &value)
}

/// Single-tuple convenience over [`index_entry_key_multi`], kept for
/// single-column callers and tests.
#[allow(dead_code)]
pub fn index_entry_key(index_name: &str, value: &Value, row_key: &[u8]) -> Vec<u8> {
    index_entry_key_multi(index_name, std::slice::from_ref(value), row_key)
}

/// The B+Tree entry key of one indexed tuple: tuple prefix + internal row key.
pub fn index_entry_key_multi(index_name: &str, values: &[Value], row_key: &[u8]) -> Vec<u8> {
    let mut key = index_tuple_prefix(index_name, values);
    key.extend_from_slice(row_key);
    key
}

/// Values a row contributes to one index: the evaluated expression for an
/// expression index (one value), otherwise the stored value of every indexed
/// column in declared column order.
pub(crate) fn index_values_for_row(
    index: &IndexDefinition,
    row: &[Value],
    schema: &TableSchema,
) -> SqlResult<Vec<Value>> {
    if let Some(expr) = &index.expression {
        return Ok(vec![crate::query::evaluate_expression(row, schema, expr)?]);
    }
    let mut values = Vec::with_capacity(index_columns(index).len());
    for column in index_columns(index) {
        let position = schema.column_index(column)?;
        values.push(row.get(position).cloned().ok_or_else(|| {
            SqlError::Storage(PlomidError::new(
                ErrorKind::Corruption,
                "row has fewer columns than catalog",
            ))
        })?);
    }
    Ok(values)
}

/// Two index tuples are equal when every component is equal under the SQL
/// equality rules the index uses (`values_equal`).
pub(crate) fn tuples_equal(left: &[Value], right: &[Value]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(l, r)| crate::row::values_equal(l, r))
}

/// A tuple never conflicts when any component is NULL (SQL NULL-not-equal
/// semantics), mirroring the single-column rule.
pub(crate) fn tuple_conflicts_under_unique(values: &[Value]) -> bool {
    !values.iter().any(Value::is_null)
}

/// Probes the authoritative index for an existing entry with the proposed
/// tuple and fails with a duplicate-key conflict when one belongs to a
/// different row.
fn probe_unique_tuple<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    index: &IndexDefinition,
    values: &[Value],
    row_key: &[u8],
) -> SqlResult<()> {
    if !index.unique || !tuple_conflicts_under_unique(values) {
        return Ok(());
    }
    let prefix = index_tuple_prefix(&index.name, values);
    for (_, existing_row_key) in txn.scan(Some(&prefix), Some(&prefix_end(&prefix)))? {
        if existing_row_key != row_key {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Conflict,
                format!(
                    "duplicate key value violates unique index \"{}\"",
                    index.name
                ),
            )));
        }
    }
    Ok(())
}

/// Stages the index maintenance for an updated row.
///
/// An UPDATE only touches the indexes whose indexed value actually changed: an
/// index whose value is unchanged has byte-identical entries before and after,
/// so deleting and re-inserting them would add WAL records, storage mutations,
/// and a uniqueness probe for no state change. The uniqueness probe for a
/// changed value is preserved, so constraint enforcement is unchanged.
pub fn stage_index_update<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    old_row: &[Value],
    new_row: &[Value],
    row_key: &[u8],
) -> SqlResult<()> {
    for index in catalog.all_indexes_for_table(table) {
        let old_values = index_values_for_row(&index, old_row, schema)?;
        let new_values = index_values_for_row(&index, new_row, schema)?;
        if tuples_equal(&old_values, &new_values) {
            continue;
        }
        txn.delete(&index_entry_key_multi(&index.name, &old_values, row_key))?;
        probe_unique_tuple(txn, &index, &new_values, row_key)?;
        txn.put(
            &index_entry_key_multi(&index.name, &new_values, row_key),
            row_key,
        )?;
    }
    Ok(())
}

/// Stages the index entries for a row.
///
/// `unique_already_checked` names the indexes whose uniqueness the caller has
/// already verified for this exact row (rule-based check backed by those same
/// indexes). Their probe is skipped so a multi-row statement does not re-query
/// the index for a value it just verified against a set derived from it.
pub fn stage_index_puts<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    row: &[Value],
    row_key: &[u8],
    unique_already_checked: Option<&std::collections::BTreeSet<String>>,
) -> SqlResult<()> {
    for index in catalog.all_indexes_for_table(table) {
        let values = index_values_for_row(&index, row, schema)?;
        let verified = unique_already_checked.is_some_and(|names| names.contains(&index.name));
        if !verified {
            probe_unique_tuple(txn, &index, &values, row_key)?;
        }
        txn.put(
            &index_entry_key_multi(&index.name, &values, row_key),
            row_key,
        )?;
    }
    Ok(())
}

pub fn stage_index_deletes<'a, T: StorageEngineTransaction<'a>>(
    txn: &mut T,
    catalog: &InMemoryCatalog,
    table: &str,
    schema: &TableSchema,
    row: &[Value],
    row_key: &[u8],
) -> SqlResult<()> {
    for index in catalog.all_indexes_for_table(table) {
        let values = index_values_for_row(&index, row, schema)?;
        txn.delete(&index_entry_key_multi(&index.name, &values, row_key))?;
    }
    Ok(())
}
