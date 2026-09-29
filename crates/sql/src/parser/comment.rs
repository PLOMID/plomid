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
    ast::{CommentObject, Statement},
    lexer::{Keyword, TokenKind},
};

fn is_kw_ident(parser: &Parser<'_>, word: &str) -> bool {
    matches!(
        parser.peek().kind,
        TokenKind::Identifier | TokenKind::Keyword(_)
    ) && parser.peek().lexeme.eq_ignore_ascii_case(word)
}

pub fn parse_comment(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Comment), "COMMENT")?;
    parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
    let first = parser.peek().clone();
    let object = match first.kind {
        TokenKind::Keyword(Keyword::Table) => {
            parser.advance();
            let name = parser.parse_table_name()?;
            CommentObject::Table { name }
        }
        TokenKind::Keyword(Keyword::Column) => {
            parser.advance();
            let qualified = parser.parse_qualified_identifier("table.column")?;
            let parts: Vec<&str> = qualified.split('.').collect();
            let (table, column) = match parts.len() {
                2 => (parts[0].to_string(), parts[1].to_string()),
                3 => (format!("{}.{}", parts[0], parts[1]), parts[2].to_string()),
                _ => {
                    return Err(ParseError::unexpected(
                        &first,
                        "COLUMN must be qualified as table.column or schema.table.column",
                    ))
                }
            };
            CommentObject::Column { table, column }
        }
        TokenKind::Keyword(Keyword::Type) => {
            parser.advance();
            let name = parser.parse_qualified_identifier("type name")?;
            CommentObject::Type { name }
        }
        TokenKind::Keyword(Keyword::Schema) => {
            parser.advance();
            let name = parser.parse_qualified_identifier("schema name")?;
            CommentObject::Schema { name }
        }
        TokenKind::Keyword(Keyword::Role) => {
            parser.advance();
            let name = parser.parse_qualified_identifier("role name")?;
            CommentObject::Role { name }
        }
        TokenKind::Keyword(Keyword::View) => {
            parser.advance();
            let name = parser.parse_table_name()?;
            CommentObject::View { name }
        }
        TokenKind::Keyword(Keyword::Index) => {
            parser.advance();
            let name = parser.parse_qualified_identifier("index name")?;
            CommentObject::Index { name }
        }
        TokenKind::Keyword(Keyword::Sequence) => {
            parser.advance();
            let name = parser.parse_qualified_identifier("sequence name")?;
            CommentObject::Sequence { name }
        }
        _ => {
            if is_kw_ident(parser, "database") {
                parser.advance();
                let _name = parser.parse_qualified_identifier("database name")?;
                CommentObject::Table {
                    name: "pg_database".to_string(),
                }
            } else if is_kw_ident(parser, "function") {
                parser.advance();
                let _name = parser.parse_qualified_identifier("function name")?;
                if parser.check(TokenKind::LParen) {
                    let mut depth = 1i32;
                    parser.advance();
                    while depth > 0 {
                        match parser.peek().kind {
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            TokenKind::Eof => break,
                            _ => {}
                        }
                        parser.advance();
                    }
                }
                CommentObject::Table {
                    name: "pg_proc".to_string(),
                }
            } else if is_kw_ident(parser, "procedure") {
                parser.advance();
                let _name = parser.parse_qualified_identifier("procedure name")?;
                if parser.check(TokenKind::LParen) {
                    let mut depth = 1i32;
                    parser.advance();
                    while depth > 0 {
                        match parser.peek().kind {
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            TokenKind::Eof => break,
                            _ => {}
                        }
                        parser.advance();
                    }
                }
                CommentObject::Table {
                    name: "pg_proc".to_string(),
                }
            } else if is_kw_ident(parser, "trigger") {
                parser.advance();
                let _name = parser.parse_qualified_identifier("trigger name")?;
                if is_kw_ident(parser, "on") {
                    parser.advance();
                    let _table = parser.parse_table_name()?;
                }
                CommentObject::Table {
                    name: "pg_trigger".to_string(),
                }
            } else if is_kw_ident(parser, "extension") {
                parser.advance();
                let _name = parser.parse_qualified_identifier("extension name")?;
                CommentObject::Table {
                    name: "pg_extension".to_string(),
                }
            } else if is_kw_ident(parser, "constraint") {
                parser.advance();
                let _name = parser.parse_qualified_identifier("constraint name")?;
                if is_kw_ident(parser, "on") {
                    parser.advance();
                    let _table = parser.parse_table_name()?;
                }
                CommentObject::Table {
                    name: "pg_constraint".to_string(),
                }
            } else {
                return Err(ParseError::unexpected(
                    &first,
                    "COMMENT ON target: TABLE, COLUMN, TYPE, SCHEMA, ROLE, VIEW, INDEX, SEQUENCE",
                ));
            }
        }
    };
    parser.consume(TokenKind::Keyword(Keyword::Is), "IS")?;
    let comment = if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Null)) {
        parser.advance();
        None
    } else {
        Some(
            parser
                .consume(TokenKind::StringLiteral, "comment text or NULL")?
                .lexeme,
        )
    };
    Ok(Statement::CommentOn { object, comment })
}
