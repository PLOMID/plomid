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
use super::{ParseError, Parser};
use crate::{
    ast::{ColumnDef, Constraint, ConstraintKind, Statement},
    lexer::{Keyword, TokenKind},
};

fn is_keyword_identifier(parser: &Parser<'_>, word: &str) -> bool {
    matches!(
        parser.peek().kind,
        TokenKind::Identifier | TokenKind::Keyword(_)
    ) && parser.peek().lexeme.eq_ignore_ascii_case(word)
}

fn consume_keyword_identifier(
    parser: &mut Parser<'_>,
    word: &str,
    expected: &str,
) -> Result<(), ParseError> {
    if is_keyword_identifier(parser, word) {
        parser.advance();
        Ok(())
    } else {
        let tok = parser.peek().clone();
        Err(ParseError::unexpected(&tok, expected))
    }
}

/// Parses the optional `CASCADE` / `RESTRICT` clause after a DROP name.
/// PostgreSQL defaults to RESTRICT when neither is given.
fn parse_cascade_restrict(parser: &mut Parser<'_>) -> bool {
    if parser.check_keyword(Keyword::Cascade) || parser.check_identifier("CASCADE") {
        parser.advance();
        true
    } else if parser.check_keyword(Keyword::Restrict) || parser.check_identifier("RESTRICT") {
        parser.advance();
        false
    } else {
        false
    }
}

pub fn parse_alter(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Alter), "ALTER")?;
    let alter_kind = parser.peek().clone();
    match alter_kind.kind {
        TokenKind::Keyword(Keyword::Table) => parse_alter_table(parser),
        _ => Err(ParseError::Unsupported {
            message: "unsupported ALTER object".to_string(),
            detail: Some(alter_kind.lexeme),
        }),
    }
}

fn parse_alter_table(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Table), "TABLE")?;
    let table = parser.parse_table_name()?;
    let action = parser.peek().clone();
    match action.kind {
        TokenKind::Keyword(Keyword::Rename) => {
            parser.advance();
            if is_keyword_identifier(parser, "column") {
                parser.advance();
                let old_name = parser
                    .consume(TokenKind::Identifier, "old column name")?
                    .lexeme;
                parser.consume(TokenKind::Keyword(Keyword::To), "TO")?;
                let new_name = parser
                    .consume(TokenKind::Identifier, "new column name")?
                    .lexeme;
                Ok(Statement::AlterTableRenameColumn {
                    table,
                    old_name,
                    new_name,
                })
            } else {
                parser.consume(TokenKind::Keyword(Keyword::To), "TO")?;
                let new_name = parser
                    .consume(TokenKind::Identifier, "new table name")?
                    .lexeme;
                Ok(Statement::AlterTableRename { table, new_name })
            }
        }
        TokenKind::Keyword(Keyword::Add) => {
            parser.advance();
            let has_column_keyword = is_keyword_identifier(parser, "column");
            if has_column_keyword {
                parser.advance();
            }
            let if_not_exists = if parser.check_keyword(Keyword::If) {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Not), "NOT")?;
                parser.consume(TokenKind::Keyword(Keyword::Exists), "EXISTS")?;
                true
            } else {
                false
            };
            let looks_like_constraint = !has_column_keyword
                && !if_not_exists
                && (is_keyword_identifier(parser, "constraint")
                    || matches!(
                        parser.peek().kind,
                        TokenKind::Keyword(Keyword::Unique)
                            | TokenKind::Keyword(Keyword::Primary)
                            | TokenKind::Keyword(Keyword::Check)
                    ));
            if looks_like_constraint {
                let constraint = parse_added_constraint(parser)?;
                Ok(Statement::AlterTableAddConstraint { table, constraint })
            } else {
                let column = parse_column_def(parser)?;
                Ok(Statement::AlterTableAddColumn {
                    table,
                    column,
                    if_not_exists,
                })
            }
        }
        TokenKind::Keyword(Keyword::Drop) => {
            parser.advance();
            consume_keyword_identifier(parser, "column", "COLUMN")?;
            let column = parser.consume(TokenKind::Identifier, "column name")?.lexeme;
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::AlterTableDropColumn {
                table,
                column,
                cascade,
            })
        }
        _ => Err(ParseError::unexpected(
            &action,
            "ALTER TABLE action (RENAME, ADD COLUMN, DROP COLUMN)",
        )),
    }
}

fn parse_added_constraint(parser: &mut Parser<'_>) -> Result<Constraint, ParseError> {
    let mut name = None;
    if is_keyword_identifier(parser, "constraint") {
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
            Constraint::new(ConstraintKind::Unique, parse_constraint_columns(parser)?)
        }
        TokenKind::Keyword(Keyword::Primary) => {
            parser.advance();
            parser.consume(TokenKind::Keyword(Keyword::Key), "KEY")?;
            Constraint::new(
                ConstraintKind::PrimaryKey,
                parse_constraint_columns(parser)?,
            )
        }
        TokenKind::Keyword(Keyword::Check) => {
            parser.advance();
            parser.consume(TokenKind::LParen, "(")?;
            let expr = parser.parse_expression()?;
            parser.consume(TokenKind::RParen, ")")?;
            Constraint::check(expr, Vec::new())
        }
        _ => {
            return Err(ParseError::unexpected(
                parser.peek(),
                "constraint (UNIQUE, PRIMARY KEY, CHECK)",
            ))
        }
    };
    constraint.name = name;
    Ok(constraint)
}

fn parse_constraint_columns(parser: &mut Parser<'_>) -> Result<Vec<String>, ParseError> {
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

fn parse_column_def(parser: &mut Parser<'_>) -> Result<ColumnDef, ParseError> {
    let col_name = parser.consume(TokenKind::Identifier, "column name")?;
    let col_type = super::types::parse_column_type(parser)?;
    // PostgreSQL STORAGE clause: { PLAIN | EXTERNAL | EXTENDED | MAIN | DEFAULT }
    // PLOMID does not implement per-column storage strategies, but accepts
    // and silently ignores the clause for client compatibility (e.g. DBeaver
    // emits `STORAGE EXTENDED` for BYTEA-like columns).
    if parser.check(TokenKind::Keyword(Keyword::Storage)) {
        parser.advance();
        // Consume the storage strategy keyword (PLAIN, EXTERNAL, EXTENDED, MAIN).
        match parser.peek().kind {
            TokenKind::Keyword(Keyword::Extended)
            | TokenKind::Keyword(Keyword::Plain)
            | TokenKind::Keyword(Keyword::External)
            | TokenKind::Keyword(Keyword::Main)
            | TokenKind::Identifier => {
                parser.advance();
            }
            _ => {
                return Err(ParseError::unexpected(
                    parser.peek(),
                    "storage strategy (PLAIN, EXTERNAL, EXTENDED, MAIN)",
                ));
            }
        }
    }
    let mut constraints: Vec<Constraint> = Vec::new();
    loop {
        match parser.peek().kind {
            TokenKind::Keyword(Keyword::Not) => {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Null), "NULL")?;
                constraints.push(Constraint::new(
                    ConstraintKind::NotNull,
                    vec![col_name.lexeme.clone()],
                ));
            }
            TokenKind::Keyword(Keyword::Null) => {
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
    Ok(ColumnDef {
        name: col_name.lexeme,
        col_type,
        constraints,
    })
}
