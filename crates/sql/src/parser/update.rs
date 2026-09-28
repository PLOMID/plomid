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
//! UPDATE statement parsing with expression-based SET assignments.
//!
//! Also parses the optional trailing PostgreSQL `RETURNING` clause
//! (`UPDATE ... WHERE ... RETURNING id, col`), stored on
//! `Statement::Update::returning` and executed by `dml::execute_update[_txn]`.

use super::{ParseError, Parser};
use crate::{
    ast::{Expression, Statement},
    lexer::{Keyword, TokenKind},
};

/// Parses the `SET col = expr [, col = expr ...]` assignment list shared by
/// `UPDATE ... SET` and `INSERT ... ON CONFLICT DO UPDATE SET`. Returns one
/// `(target, value)` pair per assignment using the existing expression grammar
/// (no second expression language is introduced for ON CONFLICT).
pub(crate) fn parse_update_set_clause(
    parser: &mut Parser<'_>,
) -> Result<Vec<(Expression, Expression)>, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Set), "SET")?;
    let mut assignments: Vec<(Expression, Expression)> = Vec::new();
    loop {
        // `value` is a lexer keyword but PostgreSQL allows it as a column
        // name, so accept the soft keyword as an assignment target.
        let first_tok = parser.peek().clone();
        let ident = match first_tok.kind {
            TokenKind::Identifier | TokenKind::Keyword(Keyword::Value) => {
                let name = first_tok.lexeme.clone();
                parser.advance();
                name
            }
            _ => return Err(ParseError::unexpected(parser.peek(), "column name")),
        };
        // Allow subscripted column targets such as `payload['key']` or
        // `payload['items'][1]` used for jsonb in-place assignment.
        let mut target = Expression::ColumnRef(ident);
        while parser.check(TokenKind::LBracket) {
            parser.advance();
            let index = super::expressions::parse_expression(parser)?;
            parser.consume(TokenKind::RBracket, "]")?;
            target = Expression::ArrayIndex {
                array: Box::new(target),
                index: Box::new(index),
            };
        }
        parser.consume(TokenKind::Eq, "=")?;
        let expr = parser.parse_expression()?;
        assignments.push((target, expr));

        if !parser.check(TokenKind::Comma) {
            break;
        }
        parser.advance();
    }
    Ok(assignments)
}

/// Parses the optional target alias after `UPDATE <table>`:
/// `UPDATE products p` or `UPDATE products AS p`. PostgreSQL allows an
/// unreserved-keyword alias too, but a bare `Identifier` token is safe here
/// because every clause keyword (`SET`, `FROM`, `WHERE`, `RETURNING`)
/// lexes as `Keyword`, never `Identifier`.
pub(crate) fn parse_target_alias(parser: &mut Parser<'_>) -> Result<Option<String>, ParseError> {
    if parser.check_keyword(Keyword::As) {
        parser.advance();
        let tok = parser.peek().clone();
        if !matches!(tok.kind, TokenKind::Identifier) {
            return Err(ParseError::unexpected(parser.peek(), "alias name after AS"));
        }
        parser.advance();
        return Ok(Some(tok.lexeme.clone()));
    }
    if matches!(parser.peek().kind, TokenKind::Identifier) {
        let tok = parser.peek().clone();
        parser.advance();
        return Ok(Some(tok.lexeme.clone()));
    }
    Ok(None)
}

pub fn parse_update(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Update), "UPDATE")?;
    let table = parser.parse_table_name()?;
    let alias = parse_target_alias(parser)?;
    let assignments = parse_update_set_clause(parser)?;

    // Optional `FROM <relation>` source (PostgreSQL `UPDATE ... FROM`),
    // parsed with the same relation grammar as SELECT's FROM clause.
    let mut from = None;
    if parser.check_keyword(Keyword::From) {
        parser.advance();
        from = Some(super::select::parse_from_clause(parser)?);
    }

    let mut where_expr = None;
    if parser.check(TokenKind::Keyword(Keyword::Where)) {
        parser.advance();
        where_expr = Some(parser.parse_expression()?);
    }

    if parser.check_keyword(Keyword::Returning) {
        // Trailing `RETURNING <select targets>` (same target grammar as
        // `INSERT ... RETURNING`, including `*`). Consumes the keyword then
        // parses a SELECT target list; absence leaves `returning: None`.
        parser.advance();
        let returning = Some(super::select::parse_select_targets(parser)?);
        return Ok(Statement::Update {
            table,
            alias,
            from,
            assignments,
            where_expr,
            returning,
        });
    }

    Ok(Statement::Update {
        table,
        alias,
        from,
        assignments,
        where_expr,
        returning: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FromClause;
    use crate::{Catalog, InMemoryCatalog, Lexer};

    /// Builds a catalog with a `products(id, stock)` table so UPDATE parsing
    /// can resolve the target relation.
    fn products_catalog() -> InMemoryCatalog {
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "products".to_string(),
                vec![
                    crate::ColumnDef {
                        name: "id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    crate::ColumnDef {
                        name: "stock".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        // A source relation for `UPDATE ... FROM updates u` parsing.
        catalog
            .create_table(
                "updates".to_string(),
                vec![
                    crate::ColumnDef {
                        name: "id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    crate::ColumnDef {
                        name: "price".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        catalog
    }

    fn parse_update_sql(sql: &str) -> Statement {
        let tokens = Lexer::new(sql).lex().unwrap();
        let mut catalog = products_catalog();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        stmts[0].clone()
    }

    #[test]
    fn parses_update_target_alias_from_and_returning() {
        let sql = "UPDATE products p \
                   SET stock = p.stock + 10 \
                   FROM (SELECT 1 AS product_id) x \
                   WHERE p.id = x.product_id \
                   RETURNING p.id, p.stock";
        let Statement::Update {
            table,
            alias,
            from,
            assignments,
            where_expr,
            returning,
        } = parse_update_sql(sql)
        else {
            panic!("expected update");
        };
        assert_eq!(table, "products");
        assert_eq!(alias.as_deref(), Some("p"));
        match from {
            Some(FromClause::Subquery { alias, .. }) => assert_eq!(alias, "x"),
            other => panic!("expected subquery FROM, got {other:?}"),
        }
        assert_eq!(assignments.len(), 1);
        assert!(where_expr.is_some());
        assert_eq!(returning.map(|r| r.len()), Some(2));
    }

    #[test]
    fn parses_update_as_alias() {
        let Statement::Update { alias, .. } =
            parse_update_sql("UPDATE products AS p SET stock = p.stock + 1 WHERE p.id = 1")
        else {
            panic!("expected update");
        };
        assert_eq!(alias.as_deref(), Some("p"));
    }

    #[test]
    fn parses_update_without_alias_or_from() {
        let Statement::Update { alias, from, .. } =
            parse_update_sql("UPDATE products SET stock = stock + 10 WHERE id = 1")
        else {
            panic!("expected update");
        };
        assert!(alias.is_none());
        assert!(from.is_none());
    }

    #[test]
    fn parses_update_from_table() {
        // FROM parsing must not require the source relation in the catalog.
        let Statement::Update { from, .. } = parse_update_sql(
            "UPDATE products p SET stock = p.stock + 1 FROM updates u WHERE p.id = u.id",
        ) else {
            panic!("expected update");
        };
        match from {
            Some(FromClause::Table { name, alias }) => {
                assert_eq!(name, "updates");
                assert_eq!(alias.as_deref(), Some("u"));
            }
            other => panic!("expected table FROM, got {other:?}"),
        }
    }

    #[test]
    fn parses_arithmetic_assignment() {
        let tokens = Lexer::new("UPDATE users SET age = age + 1 WHERE id = 1;")
            .lex()
            .unwrap();
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "users".to_string(),
                vec![
                    crate::ColumnDef {
                        name: "id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    crate::ColumnDef {
                        name: "age".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Update { assignments, .. } = &stmts[0] else {
            panic!("expected update");
        };
        assert_eq!(assignments.len(), 1);
        assert!(matches!(assignments[0].1, Expression::Add(_, _)));
    }

    #[test]
    fn parses_json_subscript_assignment() {
        let tokens =
            Lexer::new("UPDATE t SET payload['name'] = '\"UPDATED\"'::jsonb WHERE id = 1;")
                .lex()
                .unwrap();
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "t".to_string(),
                vec![
                    crate::ColumnDef {
                        name: "id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    crate::ColumnDef {
                        name: "payload".to_string(),
                        col_type: crate::ColumnType::new(
                            plomid_types::TypeOid::JSONB,
                            plomid_types::NO_TYPEMOD,
                        ),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Update { assignments, .. } = &stmts[0] else {
            panic!("expected update");
        };
        assert_eq!(assignments.len(), 1);
        assert!(matches!(
            assignments[0].0,
            Expression::ArrayIndex { ref array, ref index }
                if matches!(**array, Expression::ColumnRef(ref c) if c == "payload")
                && matches!(**index, Expression::Literal(_))
        ));
    }

    #[test]
    fn parses_nested_json_subscript_assignment() {
        let tokens = Lexer::new("UPDATE t SET payload['items'][1] = '99'::jsonb WHERE id = 2;")
            .lex()
            .unwrap();
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "t".to_string(),
                vec![
                    crate::ColumnDef {
                        name: "id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    crate::ColumnDef {
                        name: "payload".to_string(),
                        col_type: crate::ColumnType::new(
                            plomid_types::TypeOid::JSONB,
                            plomid_types::NO_TYPEMOD,
                        ),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        let Statement::Update { assignments, .. } = &stmts[0] else {
            panic!("expected update");
        };
        assert_eq!(assignments.len(), 1);
        assert!(matches!(
            assignments[0].0,
            Expression::ArrayIndex { array: ref outer, .. }
                if matches!(&**outer, Expression::ArrayIndex { ref array, .. }
                    if matches!(**array, Expression::ColumnRef(ref c) if c == "payload"))
        ));
    }
}
