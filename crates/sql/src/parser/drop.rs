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
//! DROP statement parsing with optional IF EXISTS.

use super::{ParseError, Parser};
use crate::{
    ast::Statement,
    lexer::{Keyword, TokenKind},
};

pub fn parse_drop(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Drop), "DROP")?;
    let mut if_exists = false;
    let object = parser.peek().clone();
    if !matches!(
        object.kind,
        TokenKind::Keyword(
            Keyword::Table
                | Keyword::Schema
                | Keyword::Sequence
                | Keyword::Index
                | Keyword::View
                | Keyword::Type
                | Keyword::Domain
                | Keyword::Function
        )
    ) {
        return Err(ParseError::Unsupported {
            message: "unsupported DROP object".into(),
            detail: Some(object.lexeme),
        });
    }
    parser.advance();
    let object = parser.tokens[parser.pos - 1].clone();
    if parser.check_keyword(Keyword::If) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Exists), "EXISTS")?;
        if_exists = true;
    }
    match object.kind {
        TokenKind::Keyword(Keyword::Table) => {
            let name = parser.parse_qualified_identifier("table name")?;
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::DropTable {
                name,
                if_exists,
                cascade,
            })
        }
        TokenKind::Keyword(Keyword::Schema) => {
            let name = parser.parse_qualified_identifier("schema name")?;
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::DropSchema {
                name,
                if_exists,
                cascade,
            })
        }
        TokenKind::Keyword(Keyword::Sequence) => {
            let name = parser.parse_qualified_identifier("sequence name")?;
            Ok(Statement::DropSequence { name, if_exists })
        }
        TokenKind::Keyword(Keyword::Index) => {
            let name = parser.parse_qualified_identifier("index name")?;
            Ok(Statement::DropIndex { name, if_exists })
        }
        TokenKind::Keyword(Keyword::View) => {
            let name = parser.parse_qualified_identifier("view name")?;
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::DropView {
                name,
                if_exists,
                cascade,
            })
        }
        TokenKind::Keyword(Keyword::Type) => {
            let name = parser.parse_qualified_identifier("type name")?;
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::DropType {
                name,
                if_exists,
                cascade,
            })
        }
        TokenKind::Keyword(Keyword::Domain) => {
            let name = parser.parse_qualified_identifier("domain name")?;
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::DropDomain {
                name,
                if_exists,
                cascade,
            })
        }
        TokenKind::Keyword(Keyword::Function) => {
            let name = parser.parse_qualified_identifier("function name")?;
            let mut args = Vec::new();
            // Parse optional argument type list: `func(integer, text)`
            if parser.check(TokenKind::LParen) {
                parser.advance();
                if !parser.check(TokenKind::RParen) {
                    loop {
                        let arg_type = parser.parse_type_name("argument type")?;
                        args.push(arg_type);
                        if parser.check(TokenKind::Comma) {
                            parser.advance();
                        } else {
                            break;
                        }
                    }
                }
                parser.consume(TokenKind::RParen, ")")?;
            }
            let cascade = parse_cascade_restrict(parser);
            Ok(Statement::DropFunction {
                name,
                args,
                if_exists,
                cascade,
            })
        }
        _ => Err(ParseError::Unsupported {
            message: "unsupported DROP object".to_string(),
            detail: Some(object.lexeme),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast::Statement, InMemoryCatalog, Lexer};

    #[test]
    fn parses_drop_if_exists() {
        for sql in [
            "DROP TABLE IF EXISTS t;",
            "DROP SCHEMA IF EXISTS s;",
            "DROP SEQUENCE IF EXISTS seq;",
            "DROP TYPE IF EXISTS status;",
            "DROP DOMAIN IF EXISTS email_domain;",
            "DROP FUNCTION IF EXISTS calculate_total(integer);",
        ] {
            let tokens = Lexer::new(sql).lex().unwrap();
            let mut catalog = InMemoryCatalog::new();
            let parser = Parser::new(tokens, &mut catalog);
            let stmts = parser.parse_statements().expect(sql);
            assert_eq!(stmts.len(), 1, "{sql}");
        }
    }

    #[test]
    fn parses_drop_cascade_and_restrict() {
        fn parse(sql: &str) -> Statement {
            let tokens = Lexer::new(sql).lex().unwrap();
            let mut catalog = InMemoryCatalog::new();
            Parser::new(tokens, &mut catalog)
                .parse_statements()
                .expect(sql)
                .into_iter()
                .next()
                .unwrap()
        }

        // CASCADE sets the flag.
        for sql in [
            "DROP SCHEMA s CASCADE;",
            "DROP TABLE t CASCADE;",
            "DROP VIEW v CASCADE;",
            "DROP TYPE status CASCADE;",
            "DROP DOMAIN email_domain CASCADE;",
            "DROP FUNCTION calculate_total(integer) CASCADE;",
        ] {
            let stmt = parse(sql);
            let cascade = match &stmt {
                Statement::DropSchema { cascade, .. } => *cascade,
                Statement::DropTable { cascade, .. } => *cascade,
                Statement::DropView { cascade, .. } => *cascade,
                Statement::DropType { cascade, .. } => *cascade,
                Statement::DropDomain { cascade, .. } => *cascade,
                Statement::DropFunction { cascade, .. } => *cascade,
                _ => panic!("unexpected statement for {sql}: {stmt:?}"),
            };
            assert!(cascade, "CASCADE should be set for {sql}");
        }

        // RESTRICT leaves it false.
        for sql in [
            "DROP SCHEMA s RESTRICT;",
            "DROP TABLE t RESTRICT;",
            "DROP VIEW v RESTRICT;",
            "DROP TYPE status RESTRICT;",
            "DROP DOMAIN email_domain RESTRICT;",
        ] {
            let stmt = parse(sql);
            let cascade = match &stmt {
                Statement::DropSchema { cascade, .. } => *cascade,
                Statement::DropTable { cascade, .. } => *cascade,
                Statement::DropView { cascade, .. } => *cascade,
                Statement::DropType { cascade, .. } => *cascade,
                Statement::DropDomain { cascade, .. } => *cascade,
                _ => panic!("unexpected statement for {sql}: {stmt:?}"),
            };
            assert!(!cascade, "CASCADE should be false (RESTRICT) for {sql}");
        }

        // Neither defaults to RESTRICT (false).
        for sql in [
            "DROP SCHEMA s;",
            "DROP TABLE t;",
            "DROP VIEW v;",
            "DROP TYPE status;",
            "DROP DOMAIN email_domain;",
            "DROP FUNCTION calculate_total(integer);",
        ] {
            let stmt = parse(sql);
            let cascade = match &stmt {
                Statement::DropSchema { cascade, .. } => *cascade,
                Statement::DropTable { cascade, .. } => *cascade,
                Statement::DropView { cascade, .. } => *cascade,
                Statement::DropType { cascade, .. } => *cascade,
                Statement::DropDomain { cascade, .. } => *cascade,
                Statement::DropFunction { cascade, .. } => *cascade,
                _ => panic!("unexpected statement for {sql}: {stmt:?}"),
            };
            assert!(!cascade, "CASCADE should be false (default) for {sql}");
        }

        // IF EXISTS combined with CASCADE.
        assert!(matches!(
            parse("DROP SCHEMA IF EXISTS s CASCADE;"),
            Statement::DropSchema {
                if_exists: true,
                cascade: true,
                ..
            }
        ));
        assert!(matches!(
            parse("DROP TABLE IF EXISTS t CASCADE;"),
            Statement::DropTable {
                if_exists: true,
                cascade: true,
                ..
            }
        ));
        assert!(matches!(
            parse("DROP VIEW IF EXISTS v CASCADE;"),
            Statement::DropView {
                if_exists: true,
                cascade: true,
                ..
            }
        ));
    }

    #[test]
    fn parses_drop_function_args() {
        fn parse(sql: &str) -> Statement {
            let tokens = Lexer::new(sql).lex().unwrap();
            let mut catalog = InMemoryCatalog::new();
            Parser::new(tokens, &mut catalog)
                .parse_statements()
                .expect(sql)
                .into_iter()
                .next()
                .unwrap()
        }

        // Function with no args.
        assert!(matches!(
            parse("DROP FUNCTION f();"),
            Statement::DropFunction { args, .. } if args.is_empty()
        ));

        // Function with one arg.
        if let Statement::DropFunction { args, .. } = parse("DROP FUNCTION f(integer);") {
            assert_eq!(args, vec!["integer".to_string()]);
        } else {
            panic!("expected DropFunction");
        }

        // Function with multiple args.
        if let Statement::DropFunction { args, .. } =
            parse("DROP FUNCTION f(integer, text, boolean);")
        {
            assert_eq!(
                args,
                vec![
                    "integer".to_string(),
                    "text".to_string(),
                    "boolean".to_string()
                ]
            );
        } else {
            panic!("expected DropFunction");
        }
    }
}
