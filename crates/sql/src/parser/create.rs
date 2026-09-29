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
//! CREATE statement parsing.
//!
//! `CREATE TABLE` produces a [`Statement::CreateTable`] carrying column
//! definitions and a unified list of column- and table-level constraints.
//! Constraint registration is a catalog responsibility and happens during
//! execution, never during parsing.

use super::{ParseError, Parser};
use crate::{
    ast::{
        ColumnDef, Constraint, ConstraintKind, CreateStatement, DomainConstraint, Expression,
        ForeignKeyAction, ForeignKeyMatch, FunctionArg, SelectTarget, Statement,
    },
    lexer::{Keyword, TokenKind},
};

pub fn parse_create(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Create), "CREATE")?;
    // Handle TEMPORARY/TEMP tables
    let temporary = if parser.check_keyword(Keyword::Temporary) {
        parser.advance();
        true
    } else if parser.check_keyword(Keyword::Temp) {
        parser.advance();
        true
    } else {
        false
    };
    let object = parser.peek().clone();
    match object.kind {
        TokenKind::Keyword(Keyword::Table) => parse_create_table(parser, temporary),
        TokenKind::Keyword(Keyword::Database) => {
            parser.advance();
            parse_named_create(parser, |name| CreateStatement::Database { name })
        }
        TokenKind::Keyword(Keyword::Schema) => {
            parser.advance();
            let if_not_exists = if parser.check_keyword(Keyword::If) {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Not), "NOT")?;
                parser.consume(TokenKind::Keyword(Keyword::Exists), "EXISTS")?;
                true
            } else {
                false
            };
            let name = parser.parse_qualified_identifier("object name")?;
            Ok(Statement::Create(CreateStatement::Schema {
                name,
                if_not_exists,
            }))
        }
        TokenKind::Keyword(Keyword::View) => {
            parser.advance();
            parse_create_view(parser)
        }
        TokenKind::Keyword(Keyword::Materialized) => {
            parser.advance();
            parser.consume(TokenKind::Keyword(Keyword::View), "VIEW")?;
            parse_named_create(parser, |name| CreateStatement::MaterializedView { name })
        }
        TokenKind::Keyword(Keyword::Index) => {
            parser.advance();
            let if_not_exists = parse_if_not_exists(parser)?;
            let name = parser.consume(TokenKind::Identifier, "index name")?.lexeme;
            if parser.check(TokenKind::SemiColon) || parser.check(TokenKind::Eof) {
                return Ok(Statement::Create(CreateStatement::Index { name }));
            }
            parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
            let table = parser.parse_table_name()?;
            let using = if parser.check_keyword(Keyword::Using) {
                parser.advance();
                let method = parser
                    .consume(TokenKind::Identifier, "index method")?
                    .lexeme;
                Some(method)
            } else {
                None
            };
            parser.consume(TokenKind::LParen, "(")?;
            let (column, columns, expression, operator_class) = parse_index_key_list(parser)?;
            parser.consume(TokenKind::RParen, ")")?;
            Ok(Statement::CreateIndex {
                name,
                table,
                column,
                columns,
                expression,
                unique: false,
                if_not_exists,
                using,
                operator_class,
            })
        }
        TokenKind::Keyword(Keyword::Unique) => {
            parser.advance();
            parser.consume(TokenKind::Keyword(Keyword::Index), "INDEX")?;
            let if_not_exists = parse_if_not_exists(parser)?;
            let name = parser.consume(TokenKind::Identifier, "index name")?.lexeme;
            parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
            let table = parser.parse_table_name()?;
            let using = if parser.check_keyword(Keyword::Using) {
                parser.advance();
                let method = parser
                    .consume(TokenKind::Identifier, "index method")?
                    .lexeme;
                Some(method)
            } else {
                None
            };
            parser.consume(TokenKind::LParen, "(")?;
            let (column, columns, expression, operator_class) = parse_index_key_list(parser)?;
            parser.consume(TokenKind::RParen, ")")?;
            Ok(Statement::CreateIndex {
                name,
                table,
                column,
                columns,
                expression,
                unique: true,
                if_not_exists,
                using,
                operator_class,
            })
        }
        other => parse_other_create(parser, other),
    }
}

fn parse_other_create(parser: &mut Parser<'_>, object: TokenKind) -> Result<Statement, ParseError> {
    match object {
        TokenKind::Keyword(Keyword::Sequence) => {
            parser.advance();
            parse_named_create(parser, |name| CreateStatement::Sequence { name })
        }
        TokenKind::Keyword(Keyword::Function) => {
            parser.advance();
            parse_create_function(parser)
        }
        TokenKind::Keyword(Keyword::Procedure) => {
            parser.advance();
            parse_named_create(parser, |name| CreateStatement::Procedure { name })
        }
        TokenKind::Keyword(Keyword::Trigger) => {
            parser.advance();
            parse_named_create(parser, |name| CreateStatement::Trigger { name })
        }
        TokenKind::Keyword(Keyword::Type) => {
            parser.advance();
            parse_create_type(parser)
        }
        TokenKind::Keyword(Keyword::Domain) => {
            parser.advance();
            parse_create_domain(parser)
        }
        TokenKind::Keyword(Keyword::Role) => {
            parser.advance();
            parse_create_role(parser, false)
        }
        TokenKind::Keyword(Keyword::User) => {
            parser.advance();
            parse_create_role(parser, true)
        }
        TokenKind::Keyword(Keyword::Extension) => {
            parser.advance();
            parse_named_create(parser, |name| CreateStatement::Extension { name })
        }
        _ => Err(ParseError::Unsupported {
            message: "unsupported CREATE object".to_string(),
            detail: Some(parser.peek().lexeme.clone()),
        }),
    }
}

fn parse_create_role(
    parser: &mut Parser<'_>,
    login_default: bool,
) -> Result<Statement, ParseError> {
    let name = parser.parse_qualified_identifier("role name")?;
    let mut login = login_default;
    let mut password = None;
    while !parser.check(TokenKind::SemiColon) && !parser.check(TokenKind::Eof) {
        let option = parser.peek().lexeme.to_ascii_lowercase();
        parser.advance();
        match option.as_str() {
            "login" => login = true,
            "nologin" => login = false,
            "password" => {
                let value = parser.peek().clone();
                parser.advance();
                password = Some(value.lexeme.trim_matches('\'').to_string());
            }
            _ => {}
        }
    }
    Ok(Statement::Create(CreateStatement::Role {
        name,
        login,
        password,
    }))
}

/// Parses the optional `IF NOT EXISTS` clause shared by CREATE variants.
fn parse_if_not_exists(parser: &mut Parser<'_>) -> Result<bool, ParseError> {
    if parser.check_keyword(Keyword::If) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Not), "NOT")?;
        parser.consume(TokenKind::Keyword(Keyword::Exists), "EXISTS")?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn parse_named_create<F>(parser: &mut Parser<'_>, f: F) -> Result<Statement, ParseError>
where
    F: FnOnce(String) -> CreateStatement,
{
    let name = parser.parse_qualified_identifier("object name")?;
    Ok(Statement::Create(f(name)))
}

/// Parses `CREATE TYPE name AS ENUM ('a', 'b', ...)` or
/// `CREATE TYPE name AS (col1 type1, col2 type2, ...)`.
fn parse_create_type(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    let name = parser.parse_qualified_identifier("type name")?;
    let (labels, attributes) = if parser.check_keyword(Keyword::As) {
        parser.advance();
        // Distinguish `AS ENUM (...)` from `AS (col type, ...)`.
        if parser.check_keyword(Keyword::Enum) {
            // ENUM form: AS ENUM ('label1', 'label2', ...)
            parser.advance();
            parser.consume(TokenKind::LParen, "(")?;
            let mut labels = Vec::new();
            loop {
                let label = parser.consume(TokenKind::StringLiteral, "enum label")?;
                labels.push(label.lexeme);
                if parser.check(TokenKind::Comma) {
                    parser.advance();
                } else {
                    break;
                }
            }
            parser.consume(TokenKind::RParen, ")")?;
            (Some(labels), None)
        } else {
            // Composite form: AS (col1 type1, col2 type2, ...)
            parser.consume(TokenKind::LParen, "(")?;
            let mut attributes = Vec::new();
            loop {
                let col_name = parser.consume(TokenKind::Identifier, "column name")?.lexeme;
                let col_type = parser.parse_type_name("column type")?;
                attributes.push((col_name, col_type));
                if parser.check(TokenKind::Comma) {
                    parser.advance();
                } else {
                    break;
                }
            }
            parser.consume(TokenKind::RParen, ")")?;
            (None, Some(attributes))
        }
    } else {
        (None, None)
    };
    Ok(Statement::Create(CreateStatement::Type {
        name,
        labels,
        attributes,
    }))
}

/// Parses `CREATE DOMAIN name AS type [CONSTRAINT name CHECK (...)]`.
fn parse_create_domain(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    let name = parser.parse_qualified_identifier("domain name")?;
    parser.consume(TokenKind::Keyword(Keyword::As), "AS")?;
    let base_type = parser.parse_type_name("domain base type")?;
    let mut constraints = Vec::new();
    // Parse optional constraints: CONSTRAINT name CHECK (expr) or just CHECK (expr).
    while parser.check_keyword(Keyword::Constraint) || parser.check_keyword(Keyword::Check) {
        let constraint_name = if parser.check_keyword(Keyword::Constraint) {
            parser.advance();
            let n = parser.consume(TokenKind::Identifier, "constraint name")?;
            parser.consume(TokenKind::Keyword(Keyword::Check), "CHECK")?;
            Some(n.lexeme)
        } else {
            parser.advance();
            None
        };
        parser.consume(TokenKind::LParen, "(")?;
        let check_expr = parser.parse_expression()?;
        parser.consume(TokenKind::RParen, ")")?;
        // Store the CHECK expression as a string for simplicity.
        let check = format!("{check_expr:?}");
        constraints.push(DomainConstraint {
            name: constraint_name,
            check,
        });
    }
    Ok(Statement::Create(CreateStatement::Domain {
        name,
        base_type,
        constraints,
    }))
}

/// Parses `CREATE FUNCTION name(args) RETURNS type LANGUAGE lang AS $$ body $$`.
fn parse_create_function(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    let name = parser.parse_qualified_identifier("function name")?;
    parser.consume(TokenKind::LParen, "(")?;
    let mut args = Vec::new();
    if !parser.check(TokenKind::RParen) {
        loop {
            // PostgreSQL allows both `name type` and bare `type` argument
            // declarations (`(integer)` vs `(n integer)`). Try the named form
            // first and fall back to a bare type on failure.
            let saved_pos = parser.pos;
            let named = if matches!(parser.peek().kind, TokenKind::Identifier) {
                let arg_name = parser.consume(TokenKind::Identifier, "argument name")?;
                match parser.parse_type_name("argument type") {
                    Ok(arg_type) => Some(FunctionArg {
                        name: arg_name.lexeme,
                        data_type: arg_type,
                    }),
                    Err(_) => {
                        parser.pos = saved_pos;
                        None
                    }
                }
            } else {
                None
            };
            let arg = match named {
                Some(arg) => arg,
                None => FunctionArg {
                    name: String::new(),
                    data_type: parser.parse_type_name("argument type")?,
                },
            };
            args.push(arg);
            if parser.check(TokenKind::Comma) {
                parser.advance();
            } else {
                break;
            }
        }
    }
    parser.consume(TokenKind::RParen, ")")?;
    parser.consume(TokenKind::Keyword(Keyword::Returns), "RETURNS")?;
    let returns = parser.parse_type_name("return type")?;
    parser.consume(TokenKind::Keyword(Keyword::Language), "LANGUAGE")?;
    let language = parser
        .consume(TokenKind::Identifier, "language name")?
        .lexeme;
    parser.consume(TokenKind::Keyword(Keyword::As), "AS")?;
    // The function body is dollar-quoted (`$$ body $$`) or a single-quoted
    // string. Either way the lexer hands us one StringLiteral whose lexeme is
    // the raw body text, so `$1`-style parameter references inside the body
    // are preserved verbatim and never lexed as parameter markers.
    let body = parser
        .consume(TokenKind::StringLiteral, "function body")?
        .lexeme;
    Ok(Statement::Create(CreateStatement::Function {
        name,
        args,
        returns,
        language,
        body,
    }))
}

/// Parses `CREATE [OR REPLACE] VIEW name [(cols)] AS <select>`.
fn parse_create_view(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    // `CREATE` and `VIEW` have already been consumed by `parse_create`.
    let or_replace = if parser.check(TokenKind::Keyword(Keyword::Or)) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Replace), "REPLACE")?;
        true
    } else {
        false
    };
    let name = parser.parse_qualified_identifier("view name")?;
    let mut columns = Vec::new();
    if parser.check(TokenKind::LParen) {
        parser.advance();
        loop {
            columns.push(
                parser
                    .consume(TokenKind::Identifier, "view column name")?
                    .lexeme,
            );
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
        parser.consume(TokenKind::RParen, ")")?;
    }
    parser.consume(TokenKind::Keyword(Keyword::As), "AS")?;
    let query = super::select::parse_select(parser)?;
    if columns.is_empty() {
        columns = infer_view_columns(&query);
    }
    Ok(Statement::CreateView {
        name,
        columns,
        query: Box::new(query),
        or_replace,
    })
}

/// PostgreSQL exposes view output columns even when CREATE VIEW omits an
/// explicit column list.  Keep those names in the catalog at parse time so
/// every catalog projection can describe the view consistently.
fn infer_view_columns(statement: &Statement) -> Vec<String> {
    let Statement::Select { targets, .. } = statement else {
        return Vec::new();
    };
    targets
        .iter()
        .filter_map(|target| match target {
            SelectTarget::Expr { expr, alias } => alias.clone().or_else(|| match expr {
                Expression::ColumnRef(name) => name.rsplit('.').next().map(str::to_string),
                _ => None,
            }),
            SelectTarget::Aliased { alias, .. } => Some(alias.clone()),
            SelectTarget::Function(name) | SelectTarget::FunctionCall { name, .. } => {
                Some(name.clone())
            }
            SelectTarget::WindowFunction { name, .. } => Some(name.clone()),
            SelectTarget::All | SelectTarget::QualifiedStar { .. } => None,
        })
        .collect()
}

/// Parsed `CREATE INDEX ... ON t (...)` key list: first column, full ordered
/// column list, index expression (expression indexes), operator class.
type IndexKeyList = (String, Vec<String>, Option<Expression>, Option<String>);
/// Parses the `CREATE INDEX ... ON t (...)` key list: one or more plain
/// columns (`(a)`, `(a, b)`) or a single parenthesised expression.
/// An expression cannot be combined with other keys; operator classes attach
/// to the single-column form they follow.
fn parse_index_key_list(parser: &mut Parser<'_>) -> Result<IndexKeyList, ParseError> {
    let (first, expression, operator_class) = parse_index_column_def(parser)?;
    if expression.is_some() {
        // Expression index: no further keys allowed. The comma check here
        // (rather than in the caller) produces the actionable error instead
        // of a generic "expected )" at the comma.
        if parser.check(TokenKind::Comma) {
            return Err(ParseError::Unsupported {
                message: "an expression index key cannot be combined with other columns"
                    .to_string(),
                detail: None,
            });
        }
        return Ok((first, Vec::new(), expression, operator_class));
    }
    let mut columns = vec![first.clone()];
    while parser.check(TokenKind::Comma) {
        parser.advance();
        if operator_class.is_some() {
            return Err(ParseError::Unsupported {
                message: "an operator class applies to a single-column index only".to_string(),
                detail: None,
            });
        }
        let (next, next_expression, next_operator_class) = parse_index_column_def(parser)?;
        if next_expression.is_some() {
            return Err(ParseError::Unsupported {
                message: "an expression index key cannot be combined with other columns"
                    .to_string(),
                detail: None,
            });
        }
        if next_operator_class.is_some() {
            return Err(ParseError::Unsupported {
                message: "an operator class applies to a single-column index only".to_string(),
                detail: None,
            });
        }
        columns.push(next);
    }
    Ok((first, columns, None, operator_class))
}

/// Parses an index column/expression definition inside `CREATE INDEX (...)`
/// which may be a plain identifier or a parenthesised expression like
/// `((payload ->> 'active')::INTEGER)`.
///
/// PostgreSQL also allows an optional operator class after the column name
/// or expression, e.g. `payload jsonb_path_ops`. The operator class is
/// metadata that tells the index how to build keys — it is NOT a second
/// indexed column. We return it separately so the caller can preserve it
/// in the index definition without confusing it with the indexed expression.
fn parse_index_column_def(
    parser: &mut Parser<'_>,
) -> Result<(String, Option<Expression>, Option<String>), ParseError> {
    if parser.check(TokenKind::LParen) {
        // Expression index: preserve the parsed expression as an AST so the
        // executor can bind/evaluate it against the relation (it must never
        // be resolved through a stringified representation). The raw source
        // text of the expression is kept only as the catalog display name.
        parser.advance();
        let start = parser.pos;
        let expr = parser.parse_expression()?;
        let end = parser.pos;
        parser.consume(TokenKind::RParen, ")")?;
        let raw = parser.tokens[start..end]
            .iter()
            .map(|token| token.lexeme.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        // After a parenthesised expression, PostgreSQL does not allow an
        // operator class (the syntax is ambiguous), so we return None here.
        Ok((raw, Some(expr), None))
    } else {
        let id = parser.consume(TokenKind::Identifier, "column name")?.lexeme;
        // Check for an optional operator class. In PostgreSQL, the operator
        // class follows the column name and is a bare identifier like
        // `jsonb_path_ops`. We accept any identifier here; validation of
        // whether the operator class is valid for the index method happens
        // at execution time.
        let operator_class = if parser.peek().kind == TokenKind::Identifier {
            let class = parser.peek().lexeme.clone();
            // Only treat as operator class if it looks like one (contains
            // an underscore or is a known class). This avoids accidentally
            // consuming a trailing keyword as an operator class.
            if class.contains('_') {
                parser.advance();
                Some(class)
            } else {
                None
            }
        } else {
            None
        };
        Ok((id, None, operator_class))
    }
}

fn parse_create_table(parser: &mut Parser<'_>, temporary: bool) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Table), "TABLE")?;
    let if_not_exists = if parser.check_keyword(Keyword::If) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Not), "NOT")?;
        parser.consume(TokenKind::Keyword(Keyword::Exists), "EXISTS")?;
        true
    } else {
        false
    };
    let name = parser.parse_qualified_identifier("table name")?;
    parser.consume(TokenKind::LParen, "(")?;
    let mut columns: Vec<ColumnDef> = Vec::new();
    let mut constraints: Vec<Constraint> = Vec::new();
    loop {
        if parser.check(TokenKind::RParen) {
            break;
        }
        if is_table_constraint_start(parser) {
            if let Some(constraint) = parse_table_constraint(parser)? {
                constraints.push(constraint);
            }
        } else {
            let (column, column_constraints) = parse_column_def(parser)?;
            columns.push(column);
            constraints.extend(column_constraints);
        }
        if parser.check(TokenKind::Comma) {
            parser.advance();
        } else {
            break;
        }
    }
    parser.consume(TokenKind::RParen, ")")?;
    parse_table_storage_clauses(parser)?;
    let mut seen = std::collections::HashSet::new();
    for col in &columns {
        if !seen.insert(col.name.clone()) {
            return Err(ParseError::DuplicateColumn(col.name.clone()));
        }
    }
    Ok(Statement::CreateTable {
        name,
        if_not_exists,
        temporary,
        columns,
        constraints,
    })
}

/// Consume PostgreSQL table storage clauses that do not alter PLOMID's
/// logical table model.  These are part of normal DDL emitted by database
/// tools (for example `WITH (oids=false)` and `TABLESPACE pg_default`).
/// Keeping them in the parser, rather than treating the remainder as an
/// opaque client-specific command, preserves the normal CREATE TABLE shape
/// while making the supported logical definition available to the catalog.
fn parse_table_storage_clauses(parser: &mut Parser<'_>) -> Result<(), ParseError> {
    while !parser.check(TokenKind::SemiColon) && !parser.check(TokenKind::Eof) {
        if parser.check_keyword(Keyword::With) {
            parser.advance();
            skip_balanced_clause(parser, "table storage parameters")?;
        } else if parser.check_identifier("tablespace") {
            parser.advance();
            parser.parse_qualified_identifier("tablespace name")?;
        } else if parser.check_identifier("on") || parser.check_keyword(Keyword::On) {
            parser.advance();
            // ON COMMIT {PRESERVE ROWS|DELETE ROWS|DROP} has no effect on
            // PLOMID's current transaction/storage model, but is valid DDL.
            while !parser.check(TokenKind::SemiColon) && !parser.check(TokenKind::Eof) {
                if parser.check_identifier("rows")
                    || parser.check_identifier("drop")
                    || parser.check_identifier("preserve")
                    || parser.check_identifier("delete")
                    || parser.check_keyword(Keyword::Commit)
                    || parser.check_keyword(Keyword::Drop)
                {
                    parser.advance();
                } else {
                    break;
                }
            }
        } else {
            let token = parser.peek().clone();
            return Err(ParseError::unexpected(&token, "; or end of input"));
        }
    }
    Ok(())
}

fn skip_balanced_clause(parser: &mut Parser<'_>, expected: &str) -> Result<(), ParseError> {
    if !parser.check(TokenKind::LParen) {
        return Err(ParseError::unexpected(parser.peek(), expected));
    }
    let mut depth = 0usize;
    while !parser.check(TokenKind::Eof) && !parser.check(TokenKind::SemiColon) {
        match parser.peek().kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth = depth.saturating_sub(1);
                parser.advance();
                if depth == 0 {
                    return Ok(());
                }
                continue;
            }
            _ => {}
        }
        parser.advance();
    }
    Err(ParseError::unexpected(parser.peek(), expected))
}

fn is_table_constraint_start(parser: &Parser<'_>) -> bool {
    match parser.peek().kind {
        TokenKind::Keyword(
            Keyword::Primary
            | Keyword::Unique
            | Keyword::Check
            | Keyword::Foreign
            | Keyword::Constraint,
        ) => true,
        TokenKind::Identifier => {
            let lower = parser.peek().lexeme.to_ascii_lowercase();
            matches!(lower.as_str(), "constraint" | "foreign")
        }
        _ => false,
    }
}

fn parse_table_constraint(parser: &mut Parser<'_>) -> Result<Option<Constraint>, ParseError> {
    let mut name = None;
    if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Constraint))
        || matches!(parser.peek().kind, TokenKind::Identifier)
            && parser.peek().lexeme.eq_ignore_ascii_case("constraint")
    {
        parser.advance();
        name = Some(
            parser
                .consume(TokenKind::Identifier, "constraint name")?
                .lexeme,
        );
    }
    let mut constraint = match parser.peek().kind {
        TokenKind::Keyword(Keyword::Unique) => {
            parser.advance();
            let columns = parse_constraint_column_list(parser)?;
            Constraint::new(ConstraintKind::Unique, columns)
        }
        TokenKind::Keyword(Keyword::Primary) => {
            parser.advance();
            parser.consume(TokenKind::Keyword(Keyword::Key), "KEY")?;
            let columns = parse_constraint_column_list(parser)?;
            Constraint::new(ConstraintKind::PrimaryKey, columns)
        }
        TokenKind::Keyword(Keyword::Check) => {
            parser.advance();
            parser.consume(TokenKind::LParen, "(")?;
            let expr = parser.parse_expression()?;
            parser.consume(TokenKind::RParen, ")")?;
            // Derive the constraint's column list from the expression so
            // table-level CHECK constraints participate in row validation.
            let mut columns = Vec::new();
            expr.column_refs(&mut columns);
            Constraint::check(expr, columns)
        }
        TokenKind::Keyword(Keyword::Foreign) | TokenKind::Identifier
            if parser.peek().lexeme.eq_ignore_ascii_case("foreign") =>
        {
            let constraint = parse_foreign_key_clause(parser)?;
            let mut constraint = constraint;
            constraint.name = name;
            return Ok(Some(constraint));
        }
        _ => {
            return Err(ParseError::unexpected(
                parser.peek(),
                "table constraint (UNIQUE, PRIMARY KEY, CHECK, FOREIGN KEY)",
            ))
        }
    };
    constraint.name = name;
    Ok(Some(constraint))
}

fn parse_constraint_column_list(parser: &mut Parser<'_>) -> Result<Vec<String>, ParseError> {
    parser.consume(TokenKind::LParen, "(")?;
    let mut columns = Vec::new();
    loop {
        columns.push(parser.consume(TokenKind::Identifier, "column name")?.lexeme);
        if !parser.check(TokenKind::Comma) {
            break;
        }
        parser.advance();
    }
    parser.consume(TokenKind::RParen, ")")?;
    Ok(columns)
}

fn parse_foreign_key_clause(parser: &mut Parser<'_>) -> Result<Constraint, ParseError> {
    // FOREIGN KEY (cols) REFERENCES ref_table [(ref_cols)] [MATCH ...]
    //   [ON DELETE action] [ON UPDATE action].
    parser.advance(); // FOREIGN
    parser.consume(TokenKind::Keyword(Keyword::Key), "KEY")?;
    let columns = parse_constraint_column_list(parser)?;
    // REFERENCES ref_table [(ref_cols)]
    let peek = parser.peek().clone();
    if peek.kind == TokenKind::Keyword(Keyword::References)
        || matches!(peek.kind, TokenKind::Identifier)
            && peek.lexeme.eq_ignore_ascii_case("references")
    {
        parser.advance();
    } else {
        return Err(ParseError::unexpected(&peek, "REFERENCES"));
    }
    let ref_table = parser.parse_qualified_identifier("referenced table name")?;
    let mut ref_columns = Vec::new();
    if parser.check(TokenKind::LParen) {
        ref_columns = parse_constraint_column_list(parser)?;
    }
    let mut match_type = ForeignKeyMatch::Simple;
    let mut on_delete = ForeignKeyAction::NoAction;
    let mut on_update = ForeignKeyAction::NoAction;
    loop {
        if parser.check_identifier("match") {
            parser.advance();
            let kind = parser.peek().clone();
            if kind.lexeme.eq_ignore_ascii_case("simple") {
                match_type = ForeignKeyMatch::Simple;
                parser.advance();
            } else if kind.lexeme.eq_ignore_ascii_case("full") {
                match_type = ForeignKeyMatch::Full;
                parser.advance();
            } else if kind.lexeme.eq_ignore_ascii_case("partial") {
                match_type = ForeignKeyMatch::Partial;
                parser.advance();
            } else {
                return Err(ParseError::unexpected(&kind, "SIMPLE, FULL, or PARTIAL"));
            }
        } else if parser.check_keyword(Keyword::On) {
            parser.advance();
            let what = parser.peek().clone();
            let is_delete = what.kind == TokenKind::Keyword(Keyword::Delete)
                || matches!(what.kind, TokenKind::Identifier)
                    && what.lexeme.eq_ignore_ascii_case("delete");
            let is_update = what.kind == TokenKind::Keyword(Keyword::Update)
                || matches!(what.kind, TokenKind::Identifier)
                    && what.lexeme.eq_ignore_ascii_case("update");
            if !is_delete && !is_update {
                return Err(ParseError::unexpected(&what, "DELETE or UPDATE"));
            }
            parser.advance();
            let action = parse_fk_action(parser)?;
            if is_delete {
                on_delete = action;
            } else {
                on_update = action;
            }
        } else {
            break;
        }
    }
    Ok(Constraint {
        name: None,
        kind: ConstraintKind::ForeignKey {
            ref_table,
            ref_columns,
            on_delete,
            on_update,
            match_type,
        },
        columns,
        expr: None,
    })
}

fn parse_fk_action(parser: &mut Parser<'_>) -> Result<ForeignKeyAction, ParseError> {
    let peek = parser.peek().clone();
    // NO ACTION | RESTRICT | CASCADE | SET NULL | SET DEFAULT
    if peek.kind == TokenKind::Keyword(Keyword::Not)
        || matches!(peek.kind, TokenKind::Identifier) && peek.lexeme.eq_ignore_ascii_case("no")
    {
        parser.advance();
        let action = parser.peek().clone();
        if action.lexeme.eq_ignore_ascii_case("action") {
            parser.advance();
            return Ok(ForeignKeyAction::NoAction);
        }
        return Err(ParseError::unexpected(&action, "ACTION"));
    }
    if peek.kind == TokenKind::Keyword(Keyword::Restrict)
        || matches!(peek.kind, TokenKind::Identifier)
            && peek.lexeme.eq_ignore_ascii_case("restrict")
    {
        parser.advance();
        return Ok(ForeignKeyAction::Restrict);
    }
    if peek.kind == TokenKind::Keyword(Keyword::Cascade)
        || matches!(peek.kind, TokenKind::Identifier) && peek.lexeme.eq_ignore_ascii_case("cascade")
    {
        parser.advance();
        return Ok(ForeignKeyAction::Cascade);
    }
    if peek.kind == TokenKind::Keyword(Keyword::Set)
        || matches!(peek.kind, TokenKind::Identifier) && peek.lexeme.eq_ignore_ascii_case("set")
    {
        parser.advance();
        let next = parser.peek().clone();
        if next.kind == TokenKind::Keyword(Keyword::Null)
            || matches!(next.kind, TokenKind::Identifier)
                && next.lexeme.eq_ignore_ascii_case("null")
        {
            parser.advance();
            return Ok(ForeignKeyAction::SetNull);
        }
        if next.kind == TokenKind::Keyword(Keyword::Default)
            || matches!(next.kind, TokenKind::Identifier)
                && next.lexeme.eq_ignore_ascii_case("default")
        {
            parser.advance();
            return Ok(ForeignKeyAction::SetDefault);
        }
        return Err(ParseError::unexpected(&next, "NULL or DEFAULT"));
    }
    Err(ParseError::unexpected(
        &peek,
        "NO ACTION, RESTRICT, CASCADE, SET NULL, or SET DEFAULT",
    ))
}

fn parse_column_def(parser: &mut Parser<'_>) -> Result<(ColumnDef, Vec<Constraint>), ParseError> {
    // `value` is a lexer keyword but PostgreSQL allows it as a column name
    // (e.g. `CREATE TABLE nullable (id INTEGER, value INTEGER)`), so accept
    // the soft keyword here as well as ordinary identifiers.
    let col_name = match parser.peek().kind {
        TokenKind::Identifier | TokenKind::Keyword(Keyword::Value) => parser.peek().clone(),
        _ => return Err(ParseError::unexpected(parser.peek(), "column name")),
    };
    parser.advance();
    let mut col_type = super::types::parse_column_type(parser)?;
    let mut constraints: Vec<Constraint> = Vec::new();
    loop {
        match parser.peek().kind {
            TokenKind::Identifier if parser.peek().lexeme.eq_ignore_ascii_case("generated") => {
                // GENERATED {ALWAYS|BY DEFAULT} AS IDENTITY is represented by
                // the same live sequence-backed model as SERIAL.  GENERATED
                // ALWAYS AS (expr) STORED declares a stored generated column:
                // its value is computed from `expr` on every INSERT/UPDATE and
                // is preserved as an `Expression` in a GeneratedAlways
                // constraint (distinct from DEFAULT and identity).
                parser.advance();
                if parser.check_identifier("always") {
                    parser.advance();
                } else if parser.check_keyword(Keyword::By) {
                    parser.advance();
                    if parser.check_identifier("default") || parser.check_keyword(Keyword::Default)
                    {
                        parser.advance();
                    }
                }
                parser.consume(TokenKind::Keyword(Keyword::As), "AS")?;
                if parser.check_identifier("identity") {
                    parser.advance();
                    if parser.check(TokenKind::LParen) {
                        skip_balanced_clause(parser, "identity options")?;
                    }
                    col_type.serial = true;
                } else if parser.check(TokenKind::LParen) {
                    parser.advance();
                    let generation_expr = parser.parse_expression()?;
                    parser.consume(TokenKind::RParen, ")")?;
                    if !parser.check_identifier("stored") {
                        return Err(ParseError::unexpected(parser.peek(), "STORED"));
                    }
                    parser.advance();
                    constraints.push(Constraint {
                        name: None,
                        kind: ConstraintKind::GeneratedAlways,
                        columns: vec![col_name.lexeme.clone()],
                        expr: Some(generation_expr),
                    });
                } else {
                    return Err(ParseError::unexpected(
                        parser.peek(),
                        "IDENTITY or (generation expression)",
                    ));
                }
            }
            TokenKind::Keyword(Keyword::Not) => {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Null), "NULL")?;
                constraints.push(Constraint::new(
                    ConstraintKind::NotNull,
                    vec![col_name.lexeme.clone()],
                ));
            }
            TokenKind::Keyword(Keyword::Null) => {
                // Explicit `NULL` allows NULL; nullable is the default in V1.
                parser.advance();
            }
            TokenKind::Keyword(Keyword::Primary) => {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Key), "KEY")?;
                constraints.push(Constraint::new(
                    ConstraintKind::PrimaryKey,
                    vec![col_name.lexeme.clone()],
                ));
            }
            TokenKind::Keyword(Keyword::Unique) => {
                parser.advance();
                constraints.push(Constraint::new(
                    ConstraintKind::Unique,
                    vec![col_name.lexeme.clone()],
                ));
            }
            TokenKind::Keyword(Keyword::Default) => {
                parser.advance();
                let expr = parser.parse_expression()?;
                let mut constraint = Constraint {
                    name: None,
                    kind: ConstraintKind::Default,
                    columns: vec![col_name.lexeme.clone()],
                    expr: Some(expr),
                };
                constraint.name = None;
                constraints.push(constraint);
            }
            TokenKind::Keyword(Keyword::Check) => {
                parser.advance();
                parser.consume(TokenKind::LParen, "(")?;
                let expr = parser.parse_expression()?;
                parser.consume(TokenKind::RParen, ")")?;
                constraints.push(Constraint::check(expr, vec![col_name.lexeme.clone()]));
            }
            _ => break,
        }
    }
    Ok((
        ColumnDef {
            name: col_name.lexeme,
            col_type,
            constraints: Vec::new(),
        },
        constraints,
    ))
}
