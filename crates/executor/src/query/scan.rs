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
//! Physical-row helpers shared by the scan paths.
//!
//! Columnar rows arrive as one encoded value per column; these helpers decode
//! them, prune time-range predicates against zone maps, and compare scalar
//! values with SQL ordering semantics.

use crate::coerce::number_f64;
use crate::error::{SqlError, SqlResult};
use crate::util::unqualify;
use plomid_core::{ErrorKind, PlomidError};
use plomid_sql::Expression;
use plomid_sql::SelectTarget;
use plomid_sql::TableSchema;
use plomid_sql::Value;

pub(super) fn number(v: &Value) -> Option<i64> {
    match v {
        Value::Int2(v) => Some(*v as i64),
        Value::Int4(v) => Some(*v as i64),
        Value::Int8(v) => Some(*v),
        _ => None,
    }
}

pub(super) fn columnar_row_to_values(
    row: plomid_storage::Row,
    schema: &TableSchema,
    column_ids: &[plomid_core::ColumnId],
) -> SqlResult<Vec<Value>> {
    let fields = row.into_fields();
    if fields.len() != column_ids.len() {
        return Err(SqlError::Storage(PlomidError::new(
            ErrorKind::Corruption,
            "columnar row width does not match the requested columns",
        )));
    }
    let mut values = vec![Value::Null; schema.columns.len()];
    for (field, column_id) in fields.into_iter().zip(column_ids) {
        let index = column_id.get() as usize;
        if index >= values.len() {
            return Err(SqlError::Storage(PlomidError::new(
                ErrorKind::Corruption,
                "columnar column identity exceeds the SQL schema",
            )));
        }
        let value = match field {
            plomid_storage::Field::Null => Ok(Value::Null),
            plomid_storage::Field::Bytes(bytes) => Ok(Value::Bytea(bytes)),
            plomid_storage::Field::Integer(value) => {
                // Direct typed conversion: the previous code formatted the
                // integer to a string and re-parsed it through the text
                // parser (~100ns+ per value of pure waste, and the dominant
                // cost of columnar scans). Overflow behavior matches the text
                // path (a failed conversion is a corruption error either way,
                // since the generation writer validated the value at build).
                let oid = schema.columns[index].col_type.type_oid;
                if oid == plomid_types::TypeOid::INT8 {
                    Ok(Value::Int8(value))
                } else if oid == plomid_types::TypeOid::INT4 {
                    i32::try_from(value).map(Value::Int4).map_err(|_| {
                        SqlError::Storage(PlomidError::new(
                            ErrorKind::Corruption,
                            "columnar integer exceeds int4 range",
                        ))
                    })
                } else if oid == plomid_types::TypeOid::INT2 {
                    i16::try_from(value).map(Value::Int2).map_err(|_| {
                        SqlError::Storage(PlomidError::new(
                            ErrorKind::Corruption,
                            "columnar integer exceeds int2 range",
                        ))
                    })
                } else {
                    plomid_types::text::parse_value_oid(
                        &value.to_string(),
                        schema.columns[index].col_type.type_oid,
                    )
                    .map_err(|error| {
                        SqlError::Storage(PlomidError::new(ErrorKind::Corruption, error))
                    })
                }
            }
            plomid_storage::Field::String(value) => {
                plomid_types::text::parse_value_oid(&value, schema.columns[index].col_type.type_oid)
                    .map_err(|error| {
                        SqlError::Storage(PlomidError::new(ErrorKind::Corruption, error))
                    })
            }
        }?;
        values[index] = value;
    }
    Ok(values)
}

pub(super) fn normalize_time_pruning_expression(
    expr: &Expression,
    schema: &TableSchema,
) -> Expression {
    use Expression as E;
    fn bound(expr: &Expression, oid: Option<plomid_types::TypeOid>) -> Expression {
        match (expr, oid) {
            (E::Literal(Value::Text(text) | Value::Unknown(text)), Some(oid)) => {
                plomid_types::text::parse_value_oid(text, oid)
                    .map(Expression::Literal)
                    .unwrap_or_else(|_| expr.clone())
            }
            _ => expr.clone(),
        }
    }
    fn temporal_oid(schema: &TableSchema, expr: &Expression) -> Option<plomid_types::TypeOid> {
        let E::ColumnRef(name) = expr else {
            return None;
        };
        let index = schema.column_index(unqualify(name)).ok()?;
        let oid = schema.columns[index].col_type.type_oid;
        matches!(
            oid,
            plomid_types::TypeOid::DATE
                | plomid_types::TypeOid::TIME
                | plomid_types::TypeOid::TIMETZ
                | plomid_types::TypeOid::TIMESTAMP
                | plomid_types::TypeOid::TIMESTAMPTZ
        )
        .then_some(oid)
    }
    fn pair(
        left: &Expression,
        right: &Expression,
        schema: &TableSchema,
    ) -> (Expression, Expression) {
        (
            bound(left, temporal_oid(schema, right)),
            bound(right, temporal_oid(schema, left)),
        )
    }
    match expr {
        E::Equal(left, right) => {
            let (left, right) = pair(left, right, schema);
            E::Equal(Box::new(left), Box::new(right))
        }
        E::Less(left, right) => {
            let (left, right) = pair(left, right, schema);
            E::Less(Box::new(left), Box::new(right))
        }
        E::LessOrEqual(left, right) => {
            let (left, right) = pair(left, right, schema);
            E::LessOrEqual(Box::new(left), Box::new(right))
        }
        E::Greater(left, right) => {
            let (left, right) = pair(left, right, schema);
            E::Greater(Box::new(left), Box::new(right))
        }
        E::GreaterOrEqual(left, right) => {
            let (left, right) = pair(left, right, schema);
            E::GreaterOrEqual(Box::new(left), Box::new(right))
        }
        E::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let oid = temporal_oid(schema, expr);
            E::Between {
                expr: expr.clone(),
                low: Box::new(bound(low, oid)),
                high: Box::new(bound(high, oid)),
                negated: *negated,
            }
        }
        E::And(left, right) => E::And(
            Box::new(normalize_time_pruning_expression(left, schema)),
            Box::new(normalize_time_pruning_expression(right, schema)),
        ),
        E::Or(left, right) => E::Or(
            Box::new(normalize_time_pruning_expression(left, schema)),
            Box::new(normalize_time_pruning_expression(right, schema)),
        ),
        _ => expr.clone(),
    }
}

/// The stored columns a columnar scan should read, in the positional column
/// identities the columnar reader uses, or `None` when the columnar path must
/// not serve this statement.
///
/// Refusal is required for projections the columnar reader cannot produce
/// (session functions, window functions): asking it for their inputs would
/// return a row that does not have the requested shape. Which stored columns a
/// statement reads is decided in exactly one place —
/// [`crate::projection::requested_indexes`] — the same analysis the row scan
/// uses for its decode mask.
pub(super) fn columnar_requested_columns(
    targets: &[SelectTarget],
    predicate: Option<&Expression>,
    order_by: &[plomid_sql::OrderByItem],
    schema: &TableSchema,
) -> Option<Vec<plomid_core::ColumnId>> {
    if !targets
        .iter()
        .all(crate::projection::target_is_plain_projection)
    {
        return None;
    }
    let indexes = crate::projection::requested_indexes(
        schema,
        predicate
            .into_iter()
            .chain(order_by.iter().map(|item| &item.expr)),
        targets,
    )?;
    // Columnar materialization uses zero-based positional column identities;
    // the SQL catalog's ColumnIds are global metadata identities and must not
    // be passed to the pruner.
    Some(
        indexes
            .into_iter()
            .map(|index| plomid_core::ColumnId::new(index as u64))
            .collect(),
    )
}

pub(crate) fn value_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if a.is_null() || b.is_null() {
        return None;
    }
    if let (Some(left), Some(right)) = (number_f64(a), number_f64(b)) {
        return left.partial_cmp(&right);
    }
    match (a.sort_key(), b.sort_key()) {
        (Some(ka), Some(kb)) => Some(ka.cmp(&kb)),
        _ => Some(a.to_sql_text().cmp(&b.to_sql_text())),
    }
}
