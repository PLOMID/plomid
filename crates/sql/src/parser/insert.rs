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
//! INSERT statement parsing, including multi-row VALUES lists,
//! DEFAULT VALUES, RETURNING, and ON CONFLICT clause shape validation.

use super::{select, ParseError, Parser};
use crate::{
    ast::{CopyDirection, InsertSource, InsertValue, OnConflict, OnConflictTarget, Statement},
    lexer::{Keyword, TokenKind},
    value_pg_type, Value,
};

pub fn parse_insert(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Insert), "INSERT")?;
    parser.consume(TokenKind::Keyword(Keyword::Into), "INTO")?;
    let table = parser.parse_table_name()?;

    let mut columns: Option<Vec<String>> = None;
    if parser.check(TokenKind::LParen) {
        parser.advance();
        let mut cols = Vec::new();
        loop {
            let column = match parser.peek().kind {
                TokenKind::Identifier => {
                    parser.advance();
                    parser.tokens[parser.pos - 1].lexeme.clone()
                }
                TokenKind::Keyword(Keyword::Value) => {
                    parser.advance();
                    parser.tokens[parser.pos - 1].lexeme.clone()
                }
                _ => {
                    return Err(ParseError::unexpected(parser.peek(), "column name"));
                }
            };
            if cols.iter().any(|c: &String| c == &column) {
                return Err(ParseError::DuplicateColumn(column));
            }
            cols.push(column);
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
        parser.consume(TokenKind::RParen, ")")?;
        columns = Some(cols);
    }

    let source = if parser.check_keyword(Keyword::Default) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Values), "VALUES")?;
        InsertSource::DefaultValues
    } else if parser.check_keyword(Keyword::Select) {
        let query = select::parse_select(parser)?;
        InsertSource::Select(Box::new(query))
    } else {
        parser.consume(TokenKind::Keyword(Keyword::Values), "VALUES")?;
        let mut rows = Vec::new();
        loop {
            let mut values = Vec::new();
            parser.consume(TokenKind::LParen, "(")?;
            loop {
                values.push(parse_insert_value(parser)?);
                if parser.check(TokenKind::Comma) {
                    parser.advance();
                } else {
                    break;
                }
            }
            parser.consume(TokenKind::RParen, ")")?;
            rows.push(values);
            if parser.check(TokenKind::Comma) {
                parser.advance();
            } else {
                break;
            }
        }
        InsertSource::Values(rows)
    };

    let on_conflict = if parser.check_keyword(Keyword::On) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Conflict), "CONFLICT")?;
        // Optional arbiter target: `ON CONFLICT (col, ...)`. Absent when
        // directly followed by `DO`.
        let target = if parser.check(TokenKind::LParen) {
            parser.advance();
            let mut cols = Vec::new();
            loop {
                let column = match parser.peek().kind {
                    TokenKind::Identifier | TokenKind::Keyword(Keyword::Value) => {
                        parser.advance();
                        parser.tokens[parser.pos - 1].lexeme.clone()
                    }
                    _ => return Err(ParseError::unexpected(parser.peek(), "column name")),
                };
                cols.push(column);
                if !parser.check(TokenKind::Comma) {
                    break;
                }
                parser.advance();
            }
            parser.consume(TokenKind::RParen, ")")?;
            OnConflictTarget::Columns(cols)
        } else {
            OnConflictTarget::NoTarget
        };
        parser.consume(TokenKind::Keyword(Keyword::Do), "DO")?;
        if parser.check_keyword(Keyword::Nothing) {
            parser.advance();
            Some(OnConflict::DoNothing { target })
        } else if parser.check_keyword(Keyword::Update) {
            parser.advance();
            let assignments = super::update::parse_update_set_clause(parser)?;
            let where_expr = if parser.check_keyword(Keyword::Where) {
                parser.advance();
                Some(parser.parse_expression()?)
            } else {
                None
            };
            Some(OnConflict::DoUpdate {
                target,
                assignments,
                where_expr,
            })
        } else {
            return Err(ParseError::unexpected(
                parser.peek(),
                "NOTHING or UPDATE after DO",
            ));
        }
    } else {
        None
    };

    let returning = if parser.check_keyword(Keyword::Returning) {
        parser.advance();
        Some(select::parse_select_targets(parser)?)
    } else {
        None
    };

    Ok(Statement::Insert {
        table,
        columns,
        source,
        returning,
        on_conflict,
    })
}

/// Parses a COPY statement skeleton. The network layer owns COPY protocol
/// framing; the parser only emits the parsed statement shape.
pub fn parse_copy(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Copy), "COPY")?;
    let table = parser.parse_table_name()?;

    let mut columns: Option<Vec<String>> = None;
    if parser.check(TokenKind::LParen) {
        parser.advance();
        let mut cols = Vec::new();
        loop {
            let column = match parser.peek().kind {
                TokenKind::Identifier => {
                    parser.advance();
                    parser.tokens[parser.pos - 1].lexeme.clone()
                }
                TokenKind::Keyword(Keyword::Value) => {
                    parser.advance();
                    parser.tokens[parser.pos - 1].lexeme.clone()
                }
                _ => {
                    return Err(ParseError::unexpected(parser.peek(), "column name"));
                }
            };
            if cols.iter().any(|c: &String| c == &column) {
                return Err(ParseError::DuplicateColumn(column));
            }
            cols.push(column);
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
        parser.consume(TokenKind::RParen, ")")?;
        columns = Some(cols);
    }

    let direction = if parser.check_keyword(Keyword::From) {
        parser.advance();
        CopyDirection::From
    } else if parser.check_keyword(Keyword::To) {
        parser.advance();
        CopyDirection::To
    } else {
        return Err(ParseError::unexpected(parser.peek(), "FROM or TO"));
    };

    if !parser.check_keyword(Keyword::Stdin) && !parser.check_keyword(Keyword::Stdout) {
        return Err(ParseError::Unsupported {
            message: "only COPY FROM STDIN and COPY TO STDOUT are supported".into(),
            detail: Some("file-based COPY requires server-side filesystem access".into()),
        });
    }
    parser.advance();

    Ok(Statement::Copy {
        table,
        columns,
        direction,
    })
}

/// Parses one `INSERT ... VALUES (?, ?, ...)` entry:
/// * `DEFAULT` keyword
/// * Literal / numeric / boolean / NULL value
/// * An arbitrary scalar expression (wrapped in `InsertValue::Expression`)
pub fn parse_insert_value(parser: &mut Parser<'_>) -> Result<InsertValue, ParseError> {
    if parser.check_keyword(Keyword::Default) {
        parser.advance();
        return Ok(InsertValue::Default);
    }
    if parser.check_keyword(Keyword::Null) {
        parser.advance();
        return Ok(InsertValue::Literal(Value::Null));
    }
    if parser.check_keyword(Keyword::True) {
        parser.advance();
        return Ok(InsertValue::Literal(Value::Bool(true)));
    }
    if parser.check_keyword(Keyword::False) {
        parser.advance();
        return Ok(InsertValue::Literal(Value::Bool(false)));
    }
    if parser.check_keyword(Keyword::Array) {
        let expression = parser.parse_expression()?;
        let value = literal_expression_value(&expression)?;
        return Ok(InsertValue::Literal(value));
    }
    // Parse as a full expression so that `::type` casts (e.g.
    // `'{\"a\":1}'::jsonb`) are preserved in INSERT value contexts.
    let expression = parser.parse_expression()?;
    if let ExpressionValue::Literal(lit) = extract_simple_literal(&expression) {
        Ok(InsertValue::Literal(lit))
    } else {
        Ok(InsertValue::Expression(Box::new(expression)))
    }
}

enum ExpressionValue {
    Literal(Value),
    Complex,
}

fn extract_simple_literal(expression: &crate::ast::Expression) -> ExpressionValue {
    match expression {
        crate::ast::Expression::Literal(v) => ExpressionValue::Literal(v.clone()),
        crate::ast::Expression::Negate(inner) => {
            if let crate::ast::Expression::Literal(Value::Int8(n)) = inner.as_ref() {
                ExpressionValue::Literal(Value::Int8(-*n))
            } else if let crate::ast::Expression::Literal(Value::Int4(n)) = inner.as_ref() {
                ExpressionValue::Literal(Value::Int4(-*n))
            } else if let crate::ast::Expression::Literal(Value::Int2(n)) = inner.as_ref() {
                ExpressionValue::Literal(Value::Int2(-*n))
            } else if let crate::ast::Expression::Literal(Value::Float8(n)) = inner.as_ref() {
                ExpressionValue::Literal(Value::Float8(-*n))
            } else if let crate::ast::Expression::Literal(Value::Float4(n)) = inner.as_ref() {
                ExpressionValue::Literal(Value::Float4(-*n))
            } else {
                ExpressionValue::Complex
            }
        }
        _ => ExpressionValue::Complex,
    }
}

fn literal_expression_value(expression: &crate::ast::Expression) -> Result<Value, ParseError> {
    let crate::ast::Expression::FunctionCall { name, args, .. } = expression else {
        return Err(ParseError::Unsupported {
            message: "unsupported INSERT value".into(),
            detail: Some("array expressions in INSERT VALUES".into()),
        });
    };
    if !name.eq_ignore_ascii_case("array") {
        return Err(ParseError::Unsupported {
            message: "unsupported INSERT value".into(),
            detail: Some("non-literal expressions in INSERT VALUES".into()),
        });
    }
    let elements = args
        .iter()
        .map(|arg| match arg {
            crate::ast::Expression::Literal(value) => Ok(value.clone()),
            crate::ast::Expression::FunctionCall { .. } => literal_expression_value(arg),
            _ => Err(ParseError::Unsupported {
                message: "unsupported ARRAY value".into(),
                detail: Some("ARRAY elements must be literals".into()),
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let element_oid = elements
        .iter()
        .find_map(|value| match value {
            Value::Array { element_oid, .. } => Some(*element_oid),
            value if !value.is_null() => value_pg_type(value).map(|ty| ty.oid()),
            _ => None,
        })
        .unwrap_or(plomid_types::TypeOid::TEXT);
    Ok(Value::Array {
        element_oid,
        elements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Expression, SelectTarget};
    use crate::{Catalog, ColumnDef, ColumnType, InMemoryCatalog, Lexer, Value};

    fn make_catalog() -> InMemoryCatalog {
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "users".to_string(),
                vec![
                    ColumnDef {
                        name: "id".to_string(),
                        col_type: ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    ColumnDef {
                        name: "name".to_string(),
                        col_type: ColumnType::text(),
                        constraints: Vec::new(),
                    },
                    ColumnDef {
                        name: "age".to_string(),
                        col_type: ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        catalog
    }

    #[test]
    fn parses_multiple_value_rows() {
        let tokens = Lexer::new("INSERT INTO users (id, name) VALUES (1, 'A'), (2, 'B');")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { source, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        let InsertSource::Values(values) = source else {
            panic!("expected VALUES source")
        };
        assert_eq!(values.len(), 2);
        assert_eq!(
            values[0][1],
            InsertValue::Literal(Value::Text("A".to_string()))
        );
        assert_eq!(
            values[1][1],
            InsertValue::Literal(Value::Text("B".to_string()))
        );
    }

    #[test]
    fn parses_default_keyword_in_values() {
        let tokens = Lexer::new("INSERT INTO users (id) VALUES (DEFAULT);")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { source, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        let InsertSource::Values(values) = source else {
            panic!("expected VALUES source")
        };
        assert_eq!(values[0][0], InsertValue::Default);
    }

    #[test]
    fn parses_decimal_literal_in_values() {
        let tokens = Lexer::new("INSERT INTO users (id, name, age) VALUES (0.95, 'x', 1);")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { source, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        let InsertSource::Values(values) = source else {
            panic!("expected VALUES source")
        };
        assert!(matches!(
            values[0][0],
            InsertValue::Literal(Value::Float8(_))
        ));
    }

    #[test]
    fn parses_default_values() {
        let tokens = Lexer::new("INSERT INTO users DEFAULT VALUES;")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { source, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        assert!(matches!(source, InsertSource::DefaultValues));
    }

    #[test]
    fn rejects_duplicate_columns() {
        let tokens = Lexer::new("INSERT INTO users (id, id) VALUES (1, 2);")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let err = parser.parse_statements().unwrap_err();
        assert!(matches!(err, ParseError::DuplicateColumn(c) if c == "id"));
    }

    #[test]
    fn parses_returning_all() {
        let tokens = Lexer::new("INSERT INTO users VALUES (1, 'x', 10) RETURNING *;")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { returning, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        let Some(r) = returning else {
            panic!("expected RETURNING");
        };
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0], SelectTarget::All));
    }

    #[test]
    fn parses_returning_columns() {
        let tokens =
            Lexer::new("INSERT INTO users VALUES (1, 'x', 10) RETURNING id, name AS display;")
                .lex()
                .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { returning, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        assert!(returning.is_some());
    }

    #[test]
    fn parses_on_conflict_do_nothing() {
        let tokens = Lexer::new("INSERT INTO users VALUES (1, 'x', 10) ON CONFLICT DO NOTHING;")
            .lex()
            .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { on_conflict, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        assert!(matches!(
            on_conflict,
            Some(OnConflict::DoNothing {
                target: OnConflictTarget::NoTarget
            })
        ));
    }

    #[test]
    fn parses_on_conflict_columns_do_nothing() {
        let tokens =
            Lexer::new("INSERT INTO users VALUES (1, 'x', 10) ON CONFLICT (id) DO NOTHING;")
                .lex()
                .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { on_conflict, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        match on_conflict {
            Some(OnConflict::DoNothing { target }) => {
                assert_eq!(target, &OnConflictTarget::Columns(vec!["id".to_string()]));
            }
            other => panic!("expected DoNothing Columns(id), got {other:?}"),
        }
    }

    #[test]
    fn parses_on_conflict_do_update() {
        let tokens = Lexer::new(
            "INSERT INTO users VALUES (1, 'x', 10) ON CONFLICT (id) \
             DO UPDATE SET name = EXCLUDED.name, age = age + 1 WHERE age < 99;",
        )
        .lex()
        .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { on_conflict, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        match on_conflict {
            Some(OnConflict::DoUpdate {
                target,
                assignments,
                where_expr,
            }) => {
                assert_eq!(target, &OnConflictTarget::Columns(vec!["id".to_string()]));
                assert_eq!(assignments.len(), 2);
                assert!(
                    matches!(&assignments[0].0, Expression::ColumnRef(c) if c.as_str() == "name"),
                    "first assignment target should be column `name`"
                );
                assert!(where_expr.is_some(), "expected a WHERE clause");
            }
            other => panic!("expected DoUpdate, got {other:?}"),
        }
    }

    #[test]
    fn parses_three_rows_with_commas_in_strings() {
        let tokens = Lexer::new(
            "INSERT INTO users VALUES (1, 'hello, world', NULL), (2, 'another value', 42), (3, 'a,b,c', 5);",
        )
        .lex()
        .unwrap();
        let mut catalog = make_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Insert { source, .. } = &stmts[0] else {
            panic!("expected insert");
        };
        let InsertSource::Values(values) = source else {
            panic!("expected VALUES source")
        };
        assert_eq!(values.len(), 3);
        assert_eq!(
            values[0][1],
            InsertValue::Literal(Value::Text("hello, world".to_string()))
        );
        assert_eq!(values[0][2], InsertValue::Literal(Value::Null));
        assert_eq!(
            values[1][1],
            InsertValue::Literal(Value::Text("another value".to_string()))
        );
        assert_eq!(values[1][2], InsertValue::Literal(Value::Int4(42)));
    }

    #[test]
    fn parses_copy_from_stdin() {
        let tokens = Lexer::new("COPY users FROM STDIN;").lex().unwrap();
        let mut catalog = make_catalog();
        let mut parser = Parser::new(tokens, &mut catalog);
        let stmts = (|| {
            parser.consume(TokenKind::Keyword(Keyword::Copy), "COPY")?;
            let table = parser.parse_table_name()?;
            let direction = if parser.check_keyword(Keyword::From) {
                parser.advance();
                CopyDirection::From
            } else {
                return Err(ParseError::unexpected(parser.peek(), "FROM"));
            };
            parser.consume(TokenKind::Keyword(Keyword::Stdin), "STDIN")?;
            Ok(vec![Statement::Copy {
                table,
                columns: None,
                direction,
            }])
        })();
        let stmts = stmts.unwrap();
        let Statement::Copy { direction, .. } = &stmts[0] else {
            panic!("expected copy");
        };
        assert_eq!(*direction, CopyDirection::From);
    }
}
