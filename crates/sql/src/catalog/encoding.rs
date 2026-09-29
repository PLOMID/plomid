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
use super::{
    InMemoryCatalog, IndexDefinition, RoleDefinition, StoredDomain, StoredFunction, StoredType,
    StoredView, TableSchema,
};
use crate::{
    ast::{ColumnDef, ColumnType, Constraint, ConstraintKind, Expression},
    value_pg_type, Lexer, Parser, Value,
};
use plomid_core::{ColumnId, ErrorKind, IndexId, PlomidError, Result, SchemaId, TableId};
use plomid_types::PgType;

// Catalog-encoding constants are defined once in `plomid_core::constants`;
// the module-local names below keep the body of this file unchanged.
use plomid_core::{
    CATALOG_VERSION_SQL as CATALOG_VERSION, EXPR_ADD, EXPR_AND, EXPR_ARRAY_INDEX, EXPR_BIT_AND,
    EXPR_BIT_OR, EXPR_BIT_XOR, EXPR_CAST, EXPR_COLUMN_REF, EXPR_CONCAT, EXPR_DIVIDE, EXPR_EQUAL,
    EXPR_FUNCTION_CALL, EXPR_GREATER, EXPR_GREATER_OR_EQUAL, EXPR_IS_NOT_NULL, EXPR_IS_NULL,
    EXPR_JSON_ARROW, EXPR_LESS, EXPR_LESS_OR_EQUAL, EXPR_LITERAL, EXPR_MODULO, EXPR_MULTIPLY,
    EXPR_NEGATE, EXPR_NOT, EXPR_NOT_EQUAL, EXPR_OR, EXPR_SHIFT_LEFT, EXPR_SHIFT_RIGHT, EXPR_STAR,
    EXPR_SUBTRACT, EXPR_TYPE_CAST,
};

pub(super) fn encode(catalog: &InMemoryCatalog) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(CATALOG_VERSION);
    let mut schemas: Vec<&String> = catalog.schemas.keys().collect();
    schemas.sort_unstable();
    push_u16(&mut buf, schemas.len() as u16);
    for schema in schemas {
        push_string(&mut buf, schema);
        buf.extend_from_slice(&catalog.schemas[schema].get().to_le_bytes());
    }

    let mut table_names: Vec<&String> = catalog.tables.keys().collect();
    table_names.sort_unstable();
    push_u16(&mut buf, table_names.len() as u16);
    for name in table_names {
        let schema = &catalog.tables[name];
        push_string(&mut buf, name);
        buf.extend_from_slice(&schema.table_id.get().to_le_bytes());
        push_u16(&mut buf, schema.columns.len() as u16);
        for (column_id, col) in schema.column_ids.iter().zip(&schema.columns) {
            push_string(&mut buf, &col.name);
            buf.extend_from_slice(&column_id.get().to_le_bytes());
            buf.extend_from_slice(&col.col_type.type_oid.raw().to_le_bytes());
            buf.extend_from_slice(&col.col_type.typmod.to_le_bytes());
            // v15: the SERIAL/BIGSERIAL/IDENTITY flag. Without it a recovered
            // column loses its sequence default and INSERT stops allocating.
            buf.push(u8::from(col.col_type.serial));
        }
        push_u16(&mut buf, schema.constraints.len() as u16);
        for constraint in &schema.constraints {
            encode_constraint(&mut buf, constraint);
        }
    }

    push_u16(&mut buf, catalog.sequences.len() as u16);
    for name in catalog.sequences.keys() {
        push_string(&mut buf, name);
    }

    let mut indexes: Vec<&IndexDefinition> = catalog.indexes.values().collect();
    indexes.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    push_u16(&mut buf, indexes.len() as u16);
    for index in indexes {
        push_string(&mut buf, &index.name);
        push_string(&mut buf, &index.table);
        push_string(&mut buf, &index.column);
        // v15: the ordered indexed column list. A composite UNIQUE/PK is one
        // tuple index; persisting the list keeps that representation across
        // restart (a single-column index stores exactly one name).
        push_u16(&mut buf, index.columns.len() as u16);
        for column in &index.columns {
            push_string(&mut buf, column);
        }
        match &index.expression {
            Some(expr) => {
                buf.push(1);
                encode_expression(&mut buf, expr);
            }
            None => buf.push(0),
        }
        buf.push(u8::from(index.unique));
        buf.extend_from_slice(&index.index_id.get().to_le_bytes());
        // Encode the optional GIN operator class (e.g. `jsonb_path_ops`).
        // A 0 byte means no operator class was specified; a 1 byte is
        // followed by the length-prefixed class name.
        match &index.operator_class {
            Some(class) => {
                buf.push(1);
                push_string(&mut buf, class);
            }
            None => buf.push(0),
        }
        // v15: whether this index only backs a PRIMARY KEY / UNIQUE
        // constraint. Recovered catalogs must keep constraint indexes out of
        // the user-index views (planner, ART, columnar generations).
        buf.push(u8::from(index.constraint));
    }
    push_u16(&mut buf, catalog.roles.len() as u16);
    for role in catalog.roles.values() {
        push_string(&mut buf, &role.name);
        for flag in [
            role.superuser,
            role.inherit,
            role.create_role,
            role.create_database,
            role.can_login,
            role.replication,
            role.bypass_rls,
        ] {
            buf.push(u8::from(flag));
        }
        buf.extend_from_slice(&role.connection_limit.to_le_bytes());
        match &role.password {
            Some(password) => {
                buf.push(1);
                push_string(&mut buf, password);
            }
            None => buf.push(0),
        }
        push_u16(&mut buf, role.members.len() as u16);
        for member in &role.members {
            push_string(&mut buf, member);
        }
    }
    let mut views: Vec<&StoredView> = catalog.views.values().collect();
    views.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    push_u16(&mut buf, views.len() as u16);
    for view in views {
        push_string(&mut buf, &view.name);
        push_u16(&mut buf, view.columns.len() as u16);
        for column in &view.columns {
            push_string(&mut buf, column);
        }
        // Persist resolved output types so restarts keep metadata.
        // A 0-length sentinel means legacy view (no type info).
        push_u16(&mut buf, view.column_types.len() as u16);
        for ty in &view.column_types {
            match ty {
                Some(t) => {
                    buf.push(1);
                    buf.extend_from_slice(&t.type_oid.raw().to_le_bytes());
                    buf.extend_from_slice(&t.typmod.to_le_bytes());
                }
                None => buf.push(0),
            }
        }
        push_string(&mut buf, &view.definition);
    }

    // Types.
    let mut types: Vec<&StoredType> = catalog.types.values().collect();
    types.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    push_u16(&mut buf, types.len() as u16);
    for ty in types {
        push_string(&mut buf, &ty.name);
        push_u16(&mut buf, ty.labels.len() as u16);
        for label in &ty.labels {
            push_string(&mut buf, label);
        }
    }

    // Domains.
    let mut domains: Vec<&StoredDomain> = catalog.domains.values().collect();
    domains.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    push_u16(&mut buf, domains.len() as u16);
    for domain in domains {
        push_string(&mut buf, &domain.name);
        push_string(&mut buf, &domain.base_type);
        push_u16(&mut buf, domain.constraints.len() as u16);
        for constraint in &domain.constraints {
            match &constraint.name {
                Some(name) => {
                    buf.push(1);
                    push_string(&mut buf, name);
                }
                None => buf.push(0),
            }
            push_string(&mut buf, &constraint.check);
        }
    }

    // Functions.
    let mut functions: Vec<&StoredFunction> = catalog.functions.values().collect();
    functions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    push_u16(&mut buf, functions.len() as u16);
    for func in functions {
        push_string(&mut buf, &func.name);
        push_string(&mut buf, &func.returns);
        push_string(&mut buf, &func.language);
        push_string(&mut buf, &func.body);
        push_u16(&mut buf, func.args.len() as u16);
        for arg in &func.args {
            push_string(&mut buf, &arg.name);
            push_string(&mut buf, &arg.data_type);
        }
    }

    // Databases.
    //
    // The catalog's database name set is what makes `CREATE DATABASE` durable
    // and what gives every database its identity, so it is persisted with the
    // rest of the catalog state. Names are written in the catalog's own order,
    // which is the order that assigns database identities.
    push_u16(&mut buf, catalog.database_names.len() as u16);
    for name in &catalog.database_names {
        push_string(&mut buf, name);
    }

    buf
}

fn encode_constraint(buf: &mut Vec<u8>, constraint: &Constraint) {
    let tag = match constraint.kind {
        ConstraintKind::NotNull => 0u8,
        ConstraintKind::PrimaryKey => 1,
        ConstraintKind::Unique => 2,
        ConstraintKind::Check => 3,
        ConstraintKind::Default => 4,
        ConstraintKind::GeneratedAlways => 5,
        ConstraintKind::ForeignKey { .. } => 6,
    };
    buf.push(tag);
    match &constraint.name {
        Some(name) => {
            buf.push(1);
            push_string(buf, name);
        }
        None => buf.push(0),
    }
    match &constraint.expr {
        Some(expr) => {
            buf.push(1);
            encode_expression(buf, expr);
        }
        None => buf.push(0),
    }
    push_u16(buf, constraint.columns.len() as u16);
    for column in &constraint.columns {
        push_string(buf, column);
    }
    if let ConstraintKind::ForeignKey {
        ref_table,
        ref_columns,
        on_delete,
        on_update,
        match_type,
    } = &constraint.kind
    {
        push_string(buf, ref_table);
        push_u16(buf, ref_columns.len() as u16);
        for column in ref_columns {
            push_string(buf, column);
        }
        buf.push(foreign_key_action_tag(*on_delete));
        buf.push(foreign_key_action_tag(*on_update));
        buf.push(foreign_key_match_tag(*match_type));
    }
}

fn foreign_key_action_tag(action: crate::ast::ForeignKeyAction) -> u8 {
    match action {
        crate::ast::ForeignKeyAction::NoAction => 0,
        crate::ast::ForeignKeyAction::Restrict => 1,
        crate::ast::ForeignKeyAction::Cascade => 2,
        crate::ast::ForeignKeyAction::SetNull => 3,
        crate::ast::ForeignKeyAction::SetDefault => 4,
    }
}

fn foreign_key_match_tag(kind: crate::ast::ForeignKeyMatch) -> u8 {
    match kind {
        crate::ast::ForeignKeyMatch::Simple => 0,
        crate::ast::ForeignKeyMatch::Full => 1,
        crate::ast::ForeignKeyMatch::Partial => 2,
    }
}

fn decode_foreign_key_action(tag: u8) -> Result<crate::ast::ForeignKeyAction> {
    match tag {
        0 => Ok(crate::ast::ForeignKeyAction::NoAction),
        1 => Ok(crate::ast::ForeignKeyAction::Restrict),
        2 => Ok(crate::ast::ForeignKeyAction::Cascade),
        3 => Ok(crate::ast::ForeignKeyAction::SetNull),
        4 => Ok(crate::ast::ForeignKeyAction::SetDefault),
        _ => Err(corrupt("unknown foreign key action tag")),
    }
}

fn decode_foreign_key_match(tag: u8) -> Result<crate::ast::ForeignKeyMatch> {
    match tag {
        0 => Ok(crate::ast::ForeignKeyMatch::Simple),
        1 => Ok(crate::ast::ForeignKeyMatch::Full),
        2 => Ok(crate::ast::ForeignKeyMatch::Partial),
        _ => Err(corrupt("unknown foreign key match tag")),
    }
}

fn encode_expression(buf: &mut Vec<u8>, expr: &Expression) {
    match expr {
        Expression::ColumnRef(name) => {
            buf.push(EXPR_COLUMN_REF);
            push_string(buf, name);
        }
        Expression::Literal(value) => {
            buf.push(EXPR_LITERAL);
            encode_value(buf, value);
        }
        Expression::Star => buf.push(EXPR_STAR),
        Expression::Equal(l, r) => encode_binary(buf, EXPR_EQUAL, l, r),
        Expression::NotEqual(l, r) => encode_binary(buf, EXPR_NOT_EQUAL, l, r),
        Expression::Less(l, r) => encode_binary(buf, EXPR_LESS, l, r),
        Expression::LessOrEqual(l, r) => encode_binary(buf, EXPR_LESS_OR_EQUAL, l, r),
        Expression::Greater(l, r) => encode_binary(buf, EXPR_GREATER, l, r),
        Expression::GreaterOrEqual(l, r) => encode_binary(buf, EXPR_GREATER_OR_EQUAL, l, r),
        Expression::And(l, r) => encode_binary(buf, EXPR_AND, l, r),
        Expression::Or(l, r) => encode_binary(buf, EXPR_OR, l, r),
        Expression::Add(l, r) => encode_binary(buf, EXPR_ADD, l, r),
        Expression::Subtract(l, r) => encode_binary(buf, EXPR_SUBTRACT, l, r),
        Expression::Multiply(l, r) => encode_binary(buf, EXPR_MULTIPLY, l, r),
        Expression::Divide(l, r) => encode_binary(buf, EXPR_DIVIDE, l, r),
        Expression::Modulo(l, r) => encode_binary(buf, EXPR_MODULO, l, r),
        Expression::Concat(l, r) => encode_binary(buf, EXPR_CONCAT, l, r),
        Expression::BitAnd(l, r) => encode_binary(buf, EXPR_BIT_AND, l, r),
        Expression::BitOr(l, r) => encode_binary(buf, EXPR_BIT_OR, l, r),
        Expression::BitXor(l, r) => encode_binary(buf, EXPR_BIT_XOR, l, r),
        Expression::ShiftLeft(l, r) => encode_binary(buf, EXPR_SHIFT_LEFT, l, r),
        Expression::ShiftRight(l, r) => encode_binary(buf, EXPR_SHIFT_RIGHT, l, r),
        Expression::TypeCast { expr, type_name } => {
            buf.push(EXPR_TYPE_CAST);
            encode_expression(buf, expr);
            push_string(buf, type_name);
        }
        Expression::Cast { expr, type_name } => {
            buf.push(EXPR_CAST);
            encode_expression(buf, expr);
            push_string(buf, type_name);
        }
        Expression::JsonArrow {
            left,
            right,
            as_text,
        } => {
            buf.push(EXPR_JSON_ARROW);
            encode_expression(buf, left);
            encode_expression(buf, right);
            buf.push(u8::from(*as_text));
        }
        Expression::ArrayIndex { array, index } => {
            buf.push(EXPR_ARRAY_INDEX);
            encode_expression(buf, array);
            encode_expression(buf, index);
        }
        Expression::IsNull(operand) => {
            buf.push(EXPR_IS_NULL);
            encode_expression(buf, operand);
        }
        Expression::IsNotNull(operand) => {
            buf.push(EXPR_IS_NOT_NULL);
            encode_expression(buf, operand);
        }
        Expression::Not(operand) => {
            buf.push(EXPR_NOT);
            encode_expression(buf, operand);
        }
        Expression::Negate(operand) => {
            buf.push(EXPR_NEGATE);
            encode_expression(buf, operand);
        }
        Expression::FunctionCall { name, args, .. } => {
            buf.push(EXPR_FUNCTION_CALL);
            push_string(buf, name);
            push_u16(buf, args.len() as u16);
            for arg in args {
                encode_expression(buf, arg);
            }
        }
        // Newer expression kinds (subqueries, IN-lists, CASE, etc.) are not
        // currently used inside persisted CHECK constraints. Encode them as
        // a NULL literal so we never panic at serialization time; a future
        // schema migration can add proper tags when these forms are needed
        // in catalog metadata.
        Expression::In { .. }
        | Expression::Between { .. }
        | Expression::Like { .. }
        | Expression::Case { .. }
        | Expression::Coalesce(_)
        | Expression::NullIf(..)
        | Expression::Exists(_)
        | Expression::IsDistinctFrom(..)
        | Expression::ScalarSubquery(_)
        | Expression::WindowFunction { .. }
        | Expression::RowField { .. }
        | Expression::Power(_, _)
        | Expression::Extract { .. }
        | Expression::DateLiteral(_)
        | Expression::TimestampLiteral(_)
        | Expression::TypedTimestampLiteral { .. }
        | Expression::TimestamptzLiteral(_)
        | Expression::TypedTimestamptzLiteral { .. }
        | Expression::TimeLiteral(_)
        | Expression::TypedTimeLiteral { .. }
        | Expression::QuantifiedComparison { .. }
        | Expression::IsJson { .. }
        | Expression::IsBoolean { .. }
        | Expression::JsonSubscript { .. } => {
            buf.push(EXPR_LITERAL);
            buf.push(0); // Value::Null
        }
    }
}

fn encode_binary(buf: &mut Vec<u8>, tag: u8, left: &Expression, right: &Expression) {
    buf.push(tag);
    encode_expression(buf, left);
    encode_expression(buf, right);
}

fn encode_value(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.push(0),
        Value::Bool(b) => {
            buf.push(1);
            buf.push(u8::from(*b));
        }
        Value::Int2(v) => {
            buf.push(2);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Int4(v) => {
            buf.push(3);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Int8(v) => {
            buf.push(4);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Text(v) => {
            buf.push(5);
            push_bytes(buf, v);
        }
        Value::Float4(v) => {
            buf.push(6);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Float8(v) => {
            buf.push(7);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        other => {
            buf.push(8);
            let oid = value_pg_type(other)
                .map(|ty| ty.oid())
                .unwrap_or(plomid_types::TypeOid::TEXT);
            buf.extend_from_slice(&oid.raw().to_le_bytes());
            push_bytes(buf, &plomid_types::format_text(other));
        }
    }
}

fn push_bytes(buf: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}
pub(super) fn decode(catalog: &mut InMemoryCatalog, bytes: &[u8]) -> Result<()> {
    let mut pos = 0usize;
    if bytes.is_empty() || bytes[0] != CATALOG_VERSION {
        return Err(PlomidError::new(
            ErrorKind::Catalog,
            "catalog metadata has an incompatible engine version",
        ));
    }
    pos += 1;

    let schema_count = read_u16(bytes, &mut pos)? as usize;
    for _ in 0..schema_count {
        let name = read_string(bytes, &mut pos)?;
        let id = SchemaId::new(read_u64(bytes, &mut pos)?);
        catalog.schemas.insert(name, id);
    }

    let table_count = read_u16(bytes, &mut pos)? as usize;
    for _ in 0..table_count {
        let name = read_string(bytes, &mut pos)?;
        let table_id = TableId::new(read_u64(bytes, &mut pos)?);
        let col_count = read_u16(bytes, &mut pos)? as usize;
        let mut columns = Vec::with_capacity(col_count);
        let mut column_ids = Vec::with_capacity(col_count);
        for _ in 0..col_count {
            let col_name = read_string(bytes, &mut pos)?;
            let column_id = ColumnId::new(read_u64(bytes, &mut pos)?);
            let type_oid = plomid_types::TypeOid(read_u32(bytes, &mut pos)?);
            let typmod = read_i32(bytes, &mut pos)?;
            let serial = read_u8(bytes, &mut pos)? != 0;
            let mut col_type = ColumnType::new(type_oid, typmod);
            col_type.serial = serial;
            columns.push(ColumnDef {
                name: col_name,
                col_type,
                constraints: Vec::new(),
            });
            column_ids.push(column_id);
        }
        let constraint_count = read_u16(bytes, &mut pos)? as usize;
        let mut constraints = Vec::with_capacity(constraint_count);
        for _ in 0..constraint_count {
            constraints.push(decode_constraint(bytes, &mut pos)?);
        }
        catalog.next_table_id = catalog.next_table_id.max(table_id.get().saturating_add(1));
        catalog.next_column_id = catalog.next_column_id.max(
            column_ids
                .last()
                .map_or(0u64, |id| id.get())
                .saturating_add(1),
        );
        catalog.tables.insert(
            name.clone(),
            TableSchema {
                name,
                table_id,
                column_ids,
                columns,
                constraints,
            },
        );
    }

    let sequence_count = read_u16(bytes, &mut pos)? as usize;
    for _ in 0..sequence_count {
        let name = read_string(bytes, &mut pos)?;
        catalog.sequences.insert(name, 0);
    }

    let index_count = read_u16(bytes, &mut pos)? as usize;
    for _ in 0..index_count {
        let index_name = read_string(bytes, &mut pos)?;
        let table = read_string(bytes, &mut pos)?;
        let column = read_string(bytes, &mut pos)?;
        let column_count = read_u16(bytes, &mut pos)? as usize;
        let mut columns = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            columns.push(read_string(bytes, &mut pos)?);
        }
        let index_expression = if read_u8(bytes, &mut pos)? != 0 {
            Some(decode_expression(bytes, &mut pos)?)
        } else {
            None
        };
        let unique = read_u8(bytes, &mut pos)? != 0;
        let index_id = IndexId::new(read_u64(bytes, &mut pos)?);
        catalog.next_index_id = catalog.next_index_id.max(index_id.get().saturating_add(1));
        // Decode the optional GIN operator class. This field was added in
        // catalog version 13; catalogs from older versions are rejected by
        // the version check at the top of `decode`, so we can safely assume
        // this byte is always present here.
        let operator_class = if read_u8(bytes, &mut pos)? != 0 {
            Some(read_string(bytes, &mut pos)?)
        } else {
            None
        };
        let constraint = read_u8(bytes, &mut pos)? != 0;
        catalog.indexes.insert(
            index_name.clone(),
            IndexDefinition {
                name: index_name,
                expression: index_expression,
                table,
                column,
                columns,
                unique,
                index_id,
                operator_class,
                constraint,
            },
        );
    }
    if pos < bytes.len() {
        let role_count = read_u16(bytes, &mut pos)? as usize;
        for _ in 0..role_count {
            let name = read_string(bytes, &mut pos)?;
            let flags = (0..7)
                .map(|_| read_u8(bytes, &mut pos).map(|value| value != 0))
                .collect::<Result<Vec<_>>>()?;
            let connection_limit = read_i32(bytes, &mut pos)?;
            let password = if read_u8(bytes, &mut pos)? != 0 {
                Some(read_string(bytes, &mut pos)?)
            } else {
                None
            };
            let member_count = read_u16(bytes, &mut pos)? as usize;
            let mut members = Vec::with_capacity(member_count);
            for _ in 0..member_count {
                members.push(read_string(bytes, &mut pos)?);
            }
            catalog.roles.insert(
                name.clone(),
                RoleDefinition {
                    name,
                    superuser: flags[0],
                    inherit: flags[1],
                    create_role: flags[2],
                    create_database: flags[3],
                    can_login: flags[4],
                    replication: flags[5],
                    bypass_rls: flags[6],
                    connection_limit,
                    password,
                    members,
                },
            );
        }
    }
    if pos < bytes.len() {
        let view_count = read_u16(bytes, &mut pos)? as usize;
        for _ in 0..view_count {
            let name = read_string(bytes, &mut pos)?;
            let column_count = read_u16(bytes, &mut pos)? as usize;
            let mut columns = Vec::with_capacity(column_count);
            for _ in 0..column_count {
                columns.push(read_string(bytes, &mut pos)?);
            }
            let type_count = read_u16(bytes, &mut pos)? as usize;
            let mut column_types = Vec::with_capacity(type_count);
            for _ in 0..type_count {
                let present = read_u8(bytes, &mut pos)?;
                column_types.push(if present != 0 {
                    let type_oid = plomid_types::TypeOid(read_u32(bytes, &mut pos)?);
                    let typmod = read_i32(bytes, &mut pos)?;
                    Some(ColumnType::new(type_oid, typmod))
                } else {
                    None
                });
            }
            let definition = read_string(bytes, &mut pos)?;
            let tokens = match Lexer::new(&definition).lex() {
                Ok(tokens) => tokens,
                Err(_) => continue,
            };
            let mut statements = match Parser::new(tokens, catalog).parse_statements() {
                Ok(statements) => statements,
                Err(_) => continue,
            };
            let Some(statement) = statements.pop() else {
                continue;
            };
            catalog.views.insert(
                name.clone(),
                StoredView {
                    name,
                    columns,
                    query: Box::new(statement),
                    definition,
                    column_types,
                },
            );
        }
    }
    if pos < bytes.len() {
        let type_count = read_u16(bytes, &mut pos)? as usize;
        for _ in 0..type_count {
            let name = read_string(bytes, &mut pos)?;
            let label_count = read_u16(bytes, &mut pos)? as usize;
            let mut labels = Vec::with_capacity(label_count);
            for _ in 0..label_count {
                labels.push(read_string(bytes, &mut pos)?);
            }
            catalog.types.insert(
                name.clone(),
                StoredType {
                    name,
                    labels,
                    attributes: Vec::new(),
                },
            );
        }
    }
    if pos < bytes.len() {
        let domain_count = read_u16(bytes, &mut pos)? as usize;
        for _ in 0..domain_count {
            let name = read_string(bytes, &mut pos)?;
            let base_type = read_string(bytes, &mut pos)?;
            let constraint_count = read_u16(bytes, &mut pos)? as usize;
            let mut constraints = Vec::with_capacity(constraint_count);
            for _ in 0..constraint_count {
                let constraint_name = if read_u8(bytes, &mut pos)? != 0 {
                    Some(read_string(bytes, &mut pos)?)
                } else {
                    None
                };
                let check = read_string(bytes, &mut pos)?;
                constraints.push(super::StoredDomainConstraint {
                    name: constraint_name,
                    check,
                });
            }
            catalog.domains.insert(
                name.clone(),
                StoredDomain {
                    name,
                    base_type,
                    constraints,
                },
            );
        }
    }
    if pos < bytes.len() {
        let function_count = read_u16(bytes, &mut pos)? as usize;
        for _ in 0..function_count {
            let name = read_string(bytes, &mut pos)?;
            let returns = read_string(bytes, &mut pos)?;
            let language = read_string(bytes, &mut pos)?;
            let body = read_string(bytes, &mut pos)?;
            let arg_count = read_u16(bytes, &mut pos)? as usize;
            let mut args = Vec::with_capacity(arg_count);
            for _ in 0..arg_count {
                let arg_name = read_string(bytes, &mut pos)?;
                let arg_type = read_string(bytes, &mut pos)?;
                args.push(super::StoredFunctionArg {
                    name: arg_name,
                    data_type: arg_type,
                });
            }
            let types: Vec<String> = args.iter().map(|a| a.data_type.clone()).collect();
            let key = format!("{}[{}]", name, types.join(","));
            catalog.functions.insert(
                key,
                StoredFunction {
                    name,
                    returns,
                    language,
                    body,
                    args,
                },
            );
        }
    }

    // Databases. Absent in catalogs written before database creation was
    // durable, in which case the seeded default database remains the only
    // registered database.
    if pos < bytes.len() {
        let database_count = read_u16(bytes, &mut pos)? as usize;
        let mut names = Vec::with_capacity(database_count);
        for _ in 0..database_count {
            names.push(read_string(bytes, &mut pos)?);
        }
        if !names.is_empty() {
            catalog.database_names = names;
        }
    }
    Ok(())
}

fn decode_constraint(bytes: &[u8], pos: &mut usize) -> Result<Constraint> {
    let tag = read_u8(bytes, pos)?;
    let mut kind = match tag {
        0 => ConstraintKind::NotNull,
        1 => ConstraintKind::PrimaryKey,
        2 => ConstraintKind::Unique,
        3 => ConstraintKind::Check,
        4 => ConstraintKind::Default,
        5 => ConstraintKind::GeneratedAlways,
        6 => ConstraintKind::ForeignKey {
            ref_table: String::new(),
            ref_columns: Vec::new(),
            on_delete: crate::ast::ForeignKeyAction::NoAction,
            on_update: crate::ast::ForeignKeyAction::NoAction,
            match_type: crate::ast::ForeignKeyMatch::Simple,
        },
        _ => return Err(corrupt("unknown constraint tag")),
    };
    let name = if read_u8(bytes, pos)? != 0 {
        Some(read_string(bytes, pos)?)
    } else {
        None
    };
    let expr = if read_u8(bytes, pos)? != 0 {
        Some(decode_expression(bytes, pos)?)
    } else {
        None
    };
    let col_count = read_u16(bytes, pos)? as usize;
    let mut columns = Vec::with_capacity(col_count);
    for _ in 0..col_count {
        columns.push(read_string(bytes, pos)?);
    }
    if tag == 6 {
        let ref_table = read_string(bytes, pos)?;
        let ref_count = read_u16(bytes, pos)? as usize;
        let mut ref_columns = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            ref_columns.push(read_string(bytes, pos)?);
        }
        let on_delete = decode_foreign_key_action(read_u8(bytes, pos)?)?;
        let on_update = decode_foreign_key_action(read_u8(bytes, pos)?)?;
        let match_type = decode_foreign_key_match(read_u8(bytes, pos)?)?;
        kind = ConstraintKind::ForeignKey {
            ref_table,
            ref_columns,
            on_delete,
            on_update,
            match_type,
        };
    }
    Ok(Constraint {
        name,
        kind,
        columns,
        expr,
    })
}
fn decode_expression(bytes: &[u8], pos: &mut usize) -> Result<Expression> {
    let tag = read_u8(bytes, pos)?;
    let children = |pos: &mut usize| {
        Ok::<_, PlomidError>((
            Box::new(decode_expression(bytes, pos)?),
            Box::new(decode_expression(bytes, pos)?),
        ))
    };
    match tag {
        EXPR_COLUMN_REF => Ok(Expression::ColumnRef(read_string(bytes, pos)?)),
        EXPR_LITERAL => Ok(Expression::Literal(decode_value(bytes, pos)?)),
        EXPR_STAR => Ok(Expression::Star),
        EXPR_EQUAL => children(pos).map(|(l, r)| Expression::Equal(l, r)),
        EXPR_NOT_EQUAL => children(pos).map(|(l, r)| Expression::NotEqual(l, r)),
        EXPR_LESS => children(pos).map(|(l, r)| Expression::Less(l, r)),
        EXPR_LESS_OR_EQUAL => children(pos).map(|(l, r)| Expression::LessOrEqual(l, r)),
        EXPR_GREATER => children(pos).map(|(l, r)| Expression::Greater(l, r)),
        EXPR_GREATER_OR_EQUAL => children(pos).map(|(l, r)| Expression::GreaterOrEqual(l, r)),
        EXPR_AND => children(pos).map(|(l, r)| Expression::And(l, r)),
        EXPR_OR => children(pos).map(|(l, r)| Expression::Or(l, r)),
        EXPR_ADD => children(pos).map(|(l, r)| Expression::Add(l, r)),
        EXPR_SUBTRACT => children(pos).map(|(l, r)| Expression::Subtract(l, r)),
        EXPR_MULTIPLY => children(pos).map(|(l, r)| Expression::Multiply(l, r)),
        EXPR_DIVIDE => children(pos).map(|(l, r)| Expression::Divide(l, r)),
        EXPR_MODULO => children(pos).map(|(l, r)| Expression::Modulo(l, r)),
        EXPR_CONCAT => children(pos).map(|(l, r)| Expression::Concat(l, r)),
        EXPR_BIT_AND => children(pos).map(|(l, r)| Expression::BitAnd(l, r)),
        EXPR_BIT_OR => children(pos).map(|(l, r)| Expression::BitOr(l, r)),
        EXPR_BIT_XOR => children(pos).map(|(l, r)| Expression::BitXor(l, r)),
        EXPR_SHIFT_LEFT => children(pos).map(|(l, r)| Expression::ShiftLeft(l, r)),
        EXPR_SHIFT_RIGHT => children(pos).map(|(l, r)| Expression::ShiftRight(l, r)),
        EXPR_TYPE_CAST => {
            let expr = decode_expression(bytes, pos)?;
            let type_name = read_string(bytes, pos)?;
            Ok(Expression::TypeCast {
                expr: Box::new(expr),
                type_name,
            })
        }
        EXPR_CAST => {
            let expr = decode_expression(bytes, pos)?;
            let type_name = read_string(bytes, pos)?;
            Ok(Expression::Cast {
                expr: Box::new(expr),
                type_name,
            })
        }
        EXPR_JSON_ARROW => {
            let left = decode_expression(bytes, pos)?;
            let right = decode_expression(bytes, pos)?;
            let as_text = read_u8(bytes, pos)? != 0;
            Ok(Expression::JsonArrow {
                left: Box::new(left),
                right: Box::new(right),
                as_text,
            })
        }
        EXPR_ARRAY_INDEX => {
            let array = decode_expression(bytes, pos)?;
            let index = decode_expression(bytes, pos)?;
            Ok(Expression::ArrayIndex {
                array: Box::new(array),
                index: Box::new(index),
            })
        }
        EXPR_IS_NULL => Ok(Expression::IsNull(Box::new(decode_expression(bytes, pos)?))),
        EXPR_IS_NOT_NULL => Ok(Expression::IsNotNull(Box::new(decode_expression(
            bytes, pos,
        )?))),
        EXPR_NOT => Ok(Expression::Not(Box::new(decode_expression(bytes, pos)?))),
        EXPR_NEGATE => Ok(Expression::Negate(Box::new(decode_expression(bytes, pos)?))),
        EXPR_FUNCTION_CALL => {
            let name = read_string(bytes, pos)?;
            let arg_count = read_u16(bytes, pos)? as usize;
            let mut args = Vec::with_capacity(arg_count);
            for _ in 0..arg_count {
                args.push(decode_expression(bytes, pos)?);
            }
            Ok(Expression::FunctionCall {
                name,
                args,
                distinct: false,
                filter: None,
                order_by: Vec::new(),
                returning: None,
                null_handling: None,
                unique_keys: None,
            })
        }
        _ => Err(corrupt("unknown expression tag")),
    }
}

fn decode_value(bytes: &[u8], pos: &mut usize) -> Result<Value> {
    let tag = read_u8(bytes, pos)?;
    match tag {
        0 => Ok(Value::Null),
        1 => Ok(Value::Bool(read_u8(bytes, pos)? != 0)),
        2 => Ok(Value::Int2(i16::from_le_bytes(read_array(bytes, pos)?))),
        3 => Ok(Value::Int4(i32::from_le_bytes(read_array(bytes, pos)?))),
        4 => Ok(Value::Int8(i64::from_le_bytes(read_array(bytes, pos)?))),
        5 => {
            let len = read_u32(bytes, pos)? as usize;
            let text = read_bytes(bytes, pos, len)?;
            String::from_utf8(text.to_vec())
                .map(Value::Text)
                .map_err(|_| PlomidError::new(ErrorKind::Catalog, "catalog value is not UTF-8"))
        }
        6 => Ok(Value::Float4(f32::from_le_bytes(read_array(bytes, pos)?))),
        7 => Ok(Value::Float8(f64::from_le_bytes(read_array(bytes, pos)?))),
        8 => {
            let oid = plomid_types::TypeOid(read_u32(bytes, pos)?);
            let len = read_u32(bytes, pos)? as usize;
            let text = read_bytes(bytes, pos, len)?;
            let text = std::str::from_utf8(text)
                .map_err(|_| PlomidError::new(ErrorKind::Catalog, "catalog value is not UTF-8"))?;
            let pg_type = PgType::by_oid(oid)
                .ok_or_else(|| PlomidError::new(ErrorKind::Catalog, "unknown catalog value oid"))?;
            plomid_types::parse_text_literal(text, pg_type).map_err(|message| {
                PlomidError::new(ErrorKind::Catalog, format!("catalog value: {message}"))
            })
        }
        _ => Err(corrupt("unknown value tag")),
    }
}

fn push_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn push_string(buf: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn read_u8(bytes: &[u8], pos: &mut usize) -> Result<u8> {
    Ok(read_bytes(bytes, pos, 1)?[0])
}

fn read_u16(bytes: &[u8], pos: &mut usize) -> Result<u16> {
    Ok(u16::from_le_bytes(read_array(bytes, pos)?))
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> Result<u32> {
    Ok(u32::from_le_bytes(read_array(bytes, pos)?))
}

fn read_i32(bytes: &[u8], pos: &mut usize) -> Result<i32> {
    Ok(i32::from_le_bytes(read_array(bytes, pos)?))
}

fn read_u64(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    Ok(u64::from_le_bytes(read_array(bytes, pos)?))
}

fn read_array<const N: usize>(bytes: &[u8], pos: &mut usize) -> Result<[u8; N]> {
    let slice = read_bytes(bytes, pos, N)?;
    TryInto::<[u8; N]>::try_into(slice).map_err(|_| corrupt("catalog metadata is truncated"))
}

fn read_bytes<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8]> {
    if pos.saturating_add(len) > bytes.len() {
        return Err(corrupt("catalog metadata is truncated"));
    }
    let slice = &bytes[*pos..*pos + len];
    *pos += len;
    Ok(slice)
}

fn read_string(bytes: &[u8], pos: &mut usize) -> Result<String> {
    let len = read_u16(bytes, pos)? as usize;
    let slice = read_bytes(bytes, pos, len)?;
    String::from_utf8(slice.to_vec()).map_err(|_| {
        PlomidError::new(
            ErrorKind::Catalog,
            "catalog metadata contains invalid UTF-8",
        )
    })
}

fn corrupt(message: &str) -> PlomidError {
    PlomidError::new(ErrorKind::Catalog, message.to_string())
}
