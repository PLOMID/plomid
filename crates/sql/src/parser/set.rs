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
    ast::{Statement, Value},
    lexer::{Keyword, TokenKind},
};

fn is_ident_word(parser: &Parser<'_>, word: &str) -> bool {
    matches!(
        parser.peek().kind,
        TokenKind::Identifier | TokenKind::Keyword(_)
    ) && parser.peek().lexeme.eq_ignore_ascii_case(word)
}

fn consume_any(parser: &mut Parser<'_>) -> String {
    let lex = parser.peek().lexeme.clone();
    parser.advance();
    lex
}

fn parse_txn_options_to_end(parser: &mut Parser<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    loop {
        if parser.check(TokenKind::SemiColon) || matches!(parser.peek().kind, TokenKind::Eof) {
            break;
        }
        parts.push(consume_any(parser).to_ascii_lowercase());
    }
    parts.join(" ")
}

pub fn parse_set(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Set), "SET")?;
    let mut scope: Option<String> = None;
    if is_ident_word(parser, "session") {
        scope = Some("session".to_string());
        parser.advance();
    } else if is_ident_word(parser, "local") {
        scope = Some("local".to_string());
        parser.advance();
    }
    if is_ident_word(parser, "transaction") {
        parser.advance();
        let value = parse_txn_options_to_end(parser);
        return Ok(Statement::Set {
            name: "transaction".to_string(),
            value,
        });
    }
    if is_ident_word(parser, "characteristics") {
        parser.advance();
        if is_ident_word(parser, "as") {
            parser.advance();
        }
        if is_ident_word(parser, "transaction") {
            parser.advance();
        }
        let value = parse_txn_options_to_end(parser);
        return Ok(Statement::Set {
            name: "session_characteristics".to_string(),
            value,
        });
    }
    let mut name = parser.parse_qualified_identifier("parameter name")?;
    if name.eq_ignore_ascii_case("time") && is_ident_word(parser, "zone") {
        parser.advance();
        name = "TimeZone".to_string();
    } else if name.eq_ignore_ascii_case("names") {
        name = "client_encoding".to_string();
    }
    if parser.check(TokenKind::Eq) {
        parser.advance();
    } else {
        parser.consume(TokenKind::Keyword(Keyword::To), "TO or =")?;
    }
    // `SET search_path TO a, b, ...` takes a comma-separated list. Collect the
    // full list as a single comma-joined value so the executor can install it
    // as the session search_path.
    let mut values: Vec<String> = Vec::new();
    loop {
        let token = parser.peek().clone();
        let value = match token.kind {
            TokenKind::Identifier => {
                parser.advance();
                token.lexeme.clone()
            }
            TokenKind::StringLiteral => {
                parser.advance();
                token.lexeme.clone()
            }
            TokenKind::IntegerLiteral => {
                parser.advance();
                token.lexeme.clone()
            }
            TokenKind::Keyword(Keyword::On) => {
                parser.advance();
                "on".to_string()
            }
            TokenKind::Keyword(Keyword::True) => {
                parser.advance();
                "true".to_string()
            }
            TokenKind::Keyword(Keyword::False) => {
                parser.advance();
                "false".to_string()
            }
            _ => {
                let lex = token.lexeme.to_ascii_lowercase();
                if lex == "off" {
                    parser.advance();
                    "off".to_string()
                } else if values.is_empty() {
                    return Err(ParseError::unexpected(
                        &token,
                        "parameter value (identifier, string, integer, ON/OFF, TRUE/FALSE)",
                    ));
                } else {
                    break;
                }
            }
        };
        values.push(value);
        if parser.check(TokenKind::Comma) {
            parser.advance();
            continue;
        }
        break;
    }
    let value = values.join(", ");
    let _ = (scope, Value::Text(value.clone()));
    Ok(Statement::Set { name, value })
}
