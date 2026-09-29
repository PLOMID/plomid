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
//! `EXPLAIN` (plain and JSON) and the table statistics it reports.
//!
//! Row counts and widths are estimates derived from the catalog and the
//! current snapshot; the output stays stable enough for tests to assert on.

use crate::error::SqlResult;
use plomid_core::PlomidError;
use plomid_sql::Catalog;
use plomid_sql::ColumnType;
use plomid_sql::FromClause;
use plomid_sql::InMemoryCatalog;
use plomid_sql::QueryResult;
use plomid_sql::SetOpKind;
use plomid_sql::Statement;
use plomid_sql::TableSchema;
use plomid_sql::Value;
use plomid_txn::StorageEngine;

/// Renders a text execution-plan summary for `EXPLAIN [ANALYZE] <statement>`.
/// Returns a single-column `QUERY PLAN` result like PostgreSQL's psql output.
pub fn explain_statement<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    statement: &Statement,
    analyze: bool,
    format: Option<String>,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    let mut lines = Vec::new();
    let plan_root = describe_statement(statement);

    // Get actual row count from table if this is a simple table scan
    let (estimated_rows, estimated_width) = get_table_stats(engine, catalog, statement);

    // Calculate approximate cost
    let total_cost = if estimated_rows > 0 {
        1.0 + (estimated_rows as f64 * 0.01)
    } else {
        0.0
    };

    // Handle JSON format
    if format.as_deref() == Some("json") {
        return explain_json(
            engine,
            catalog,
            statement,
            analyze,
            current_database,
            current_user,
        );
    }

    lines.push(format!(
        "{plan_root} (cost={:.2}..{:.2} rows={} width={})",
        total_cost, total_cost, estimated_rows, estimated_width
    ));

    if analyze {
        match crate::join::execute_statement(
            engine,
            catalog,
            statement,
            current_database,
            current_user,
            None,
            0,
        ) {
            Ok(result) => {
                let rows = match &result {
                    QueryResult::Rows { rows, .. } => rows.len(),
                    QueryResult::Inserted(n) => *n as usize,
                    QueryResult::Updated(n) => *n as usize,
                    QueryResult::Deleted(n) => *n as usize,
                    _ => 0,
                };
                lines.push(format!(
                    "  -> Actual Rows: {}, Execution Time: 0.00ms",
                    rows
                ));
            }
            Err(e) => lines.push(format!("  ERROR: {e}")),
        }
    }
    Ok(QueryResult::Rows {
        columns: vec!["QUERY PLAN".to_string()],
        column_types: vec![Some(ColumnType::text())],
        rows: lines
            .into_iter()
            .map(|line| vec![Value::Text(line)])
            .collect(),
    })
}

/// Get table statistics for row count and width estimation
fn get_table_stats<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    statement: &Statement,
) -> (usize, usize) {
    let table_name = extract_table_name(statement);
    match table_name {
        Some(ref name) => {
            let qualified = catalog.resolve_table_namespace(name);
            match catalog.get_table(&qualified) {
                Ok(schema) => {
                    let width = estimate_row_width(schema);
                    match count_table_rows(engine, &qualified) {
                        Ok(count) => (count, width),
                        Err(_) => (0, width),
                    }
                }
                Err(_) => (0, 0),
            }
        }
        None => (0, 0), // No table (e.g., EXPLAIN SELECT 1)
    }
}

/// Generate JSON EXPLAIN output (manual JSON generation to avoid serde_json dependency)
fn explain_json<E: StorageEngine>(
    engine: &mut E,
    catalog: &InMemoryCatalog,
    statement: &Statement,
    analyze: bool,
    current_database: &str,
    current_user: &str,
) -> SqlResult<QueryResult> {
    let plan_root = describe_statement(statement);
    let (estimated_rows, estimated_width) = get_table_stats(engine, catalog, statement);
    let total_cost = if estimated_rows > 0 {
        1.0 + (estimated_rows as f64 * 0.01)
    } else {
        0.0
    };

    let table_name = extract_table_name(statement);

    // Build JSON manually
    let mut json = String::from("{\n  \"Plan\": {\n");
    json.push_str(&format!(
        "    \"Node Type\": \"{}\",\n",
        escape_json_string(&plan_root)
    ));
    json.push_str("\"Parent Relationship\": \"Outer\",\n");
    json.push_str("\"Parallel Aware\": false,\n");
    json.push_str(&format!(
        "    \"Relation Name\": {},\n",
        if let Some(ref name) = table_name {
            format!("\"{}\"", escape_json_string(name))
        } else {
            "null".to_string()
        }
    ));
    json.push_str("\"Alias\": null,\n");
    json.push_str(&format!("    \"Startup Cost\": {:.2},\n", total_cost));
    json.push_str(&format!("    \"Total Cost\": {:.2},\n", total_cost));
    json.push_str(&format!("    \"Plan Rows\": {},\n", estimated_rows));
    json.push_str(&format!("    \"Plan Width\": {}\n", estimated_width));
    json.push_str("  }\n");

    // Add actual stats if analyzing
    if analyze {
        let start = std::time::Instant::now();
        let result = crate::join::execute_statement(
            engine,
            catalog,
            statement,
            current_database,
            current_user,
            None,
            0,
        );
        let elapsed = start.elapsed();

        let actual_rows = match result {
            Ok(qr) => match qr {
                QueryResult::Rows { rows, .. } => rows.len(),
                QueryResult::Inserted(n) => n as usize,
                QueryResult::Updated(n) => n as usize,
                QueryResult::Deleted(n) => n as usize,
                _ => 0,
            },
            Err(_) => 0,
        };

        // Insert actual stats before closing braces
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
        json.push_str(&format!(
            "  \"Actual Rows\": {},\n  \"Actual Loops\": 1,\n  \"Actual Startup Time\": {:.3},\n  \"Actual Total Time\": {:.3}\n}}\n",
            actual_rows, elapsed_ms, elapsed_ms
        ));
    } else {
        json.push('}');
        json.push('\n');
    }

    Ok(QueryResult::Rows {
        columns: vec!["QUERY PLAN".to_string()],
        column_types: vec![Some(ColumnType::text())],
        rows: vec![vec![Value::Text(json)]],
    })
}

/// Escape a string for JSON output
fn escape_json_string(s: &str) -> String {
    let mut escaped = String::new();
    for c in s.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Produces a short text description of a statement's plan root.
fn describe_statement(statement: &Statement) -> String {
    match statement {
        Statement::Select {
            from, where_expr, ..
        } => {
            let from_desc = match from {
                Some(FromClause::Table { name, .. }) => format!(" on {name}"),
                Some(FromClause::Join { left, right, .. }) => {
                    format!(" on {} JOIN {}", describe_from(left), describe_from(right))
                }
                Some(FromClause::Subquery { alias, .. }) => format!(" on subquery {alias}"),
                Some(FromClause::TableFunction { name, .. }) => format!(" on {name}"),
                None => String::new(),
            };
            if where_expr.is_some() {
                format!("Seq Scan{from_desc} (filter: WHERE)")
            } else {
                format!("Seq Scan{from_desc}")
            }
        }
        Statement::SetOperation { op, all, .. } => {
            let name = match op {
                SetOpKind::Union => "Append",
                SetOpKind::Intersect => "Hash Intersect",
                SetOpKind::Except => "Hash Except",
            };
            let all_suffix = if *all { " ALL" } else { "" };
            format!("{name}{all_suffix}")
        }
        Statement::With { ctes, .. } => format!("CTE Scan ({} CTEs)", ctes.len()),
        Statement::Insert { table, .. } => format!("Insert on {table}"),
        Statement::Update { table, .. } => format!("Update on {table}"),
        Statement::Delete { table, .. } => format!("Delete on {table}"),
        Statement::Explain { .. } => "Explain".to_string(),
        other => format!("{other:?}"),
    }
}

/// Describes one FROM clause side for plan naming.
fn describe_from(clause: &FromClause) -> String {
    match clause {
        FromClause::Table { name, .. } => name.clone(),
        FromClause::TableFunction { name, .. } => name.clone(),
        FromClause::Subquery { alias, .. } => alias.clone(),
        FromClause::Join { left, right, .. } => {
            format!("({} JOIN {})", describe_from(left), describe_from(right))
        }
    }
}

/// Estimate the average width of a row in bytes based on column types.
fn estimate_row_width(schema: &TableSchema) -> usize {
    let mut total_width = 0;
    for column in &schema.columns {
        total_width += estimate_column_width(&column.col_type);
    }
    total_width
}

/// Estimate the width of a single column based on its type.
fn estimate_column_width(col_type: &ColumnType) -> usize {
    match col_type.type_oid.raw() {
        23 => 4,    // INTEGER
        21 => 4,    // SMALLINT
        20 => 8,    // BIGINT
        700 => 8,   // REAL
        701 => 8,   // DOUBLE PRECISION
        1043 => 16, // VARCHAR
        1042 => 16, // CHAR
        25 => 32,   // TEXT
        3802 => 32, // JSON
        3808 => 32, // JSONB
        16 => 1,    // BOOLEAN
        _ => 16,
    }
}

/// Count rows in a table by scanning its storage.
fn count_table_rows<E: StorageEngine>(
    engine: &mut E,
    table_name: &str,
) -> Result<usize, PlomidError> {
    let start = format!("{table_name}:").into_bytes();
    let end = format!("{table_name}:\u{10FFFF}").into_bytes();
    let entries = engine.scan(Some(&start), Some(&end))?;
    Ok(entries.len())
}

/// Extract table name from a SELECT statement's FROM clause.
fn extract_table_name(statement: &Statement) -> Option<String> {
    match statement {
        Statement::Select { from, .. } => match from {
            Some(FromClause::Table { name, .. }) => Some(name.clone()),
            Some(FromClause::Join { left, .. }) => extract_from_clause_name(left),
            _ => None,
        },
        Statement::Insert { table, .. } => Some(table.clone()),
        Statement::Update { table, .. } => Some(table.clone()),
        Statement::Delete { table, .. } => Some(table.clone()),
        _ => None,
    }
}

/// Extract table name from a FromClause.
fn extract_from_clause_name(clause: &FromClause) -> Option<String> {
    match clause {
        FromClause::Table { name, .. } => Some(name.clone()),
        FromClause::Join { left, .. } => extract_from_clause_name(left),
        _ => None,
    }
}
