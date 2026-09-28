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
    ast::ColumnType,
    lexer::{Keyword, TokenKind},
};
use plomid_types::{encode_typmod, PgType, SerialKind, TypmodSpec, NO_TYPEMOD};

pub fn parse_column_type(parser: &mut Parser<'_>) -> Result<ColumnType, ParseError> {
    let token = parser.peek().clone();
    let mut type_name = match token.kind {
        TokenKind::Keyword(
            Keyword::Integer
            | Keyword::BigInt
            | Keyword::SmallInt
            | Keyword::Text
            | Keyword::Varchar
            | Keyword::Boolean
            | Keyword::Date
            | Keyword::Time
            | Keyword::Timestamp
            | Keyword::Timestamptz
            | Keyword::Json,
        ) => token.lexeme.clone(),
        TokenKind::Identifier => token.lexeme.clone(),
        _ => {
            return Err(ParseError::unexpected(
                &token,
                "column type (e.g. INTEGER, VARCHAR(10), NUMERIC(20,5), TEXT[])",
            ))
        }
    };
    parser.advance();

    // PostgreSQL has several multi-token type names.  The type registry
    // already owns their aliases; the parser must preserve the full name so
    // those aliases are resolved consistently for DDL and result metadata.
    let second = parser.peek().lexeme.to_ascii_lowercase();
    let first = type_name.to_ascii_lowercase();
    let words = match (first.as_str(), second.as_str()) {
        ("character", "varying")
        | ("double", "precision")
        | ("timestamp", "without")
        | ("timestamp", "with") => Some(1),
        _ => None,
    };
    if words.is_some() {
        type_name.push(' ');
        type_name.push_str(&parser.peek().lexeme);
        parser.advance();
        if first == "timestamp" {
            let zone = parser.peek().lexeme.to_ascii_lowercase();
            if (second == "without" || second == "with") && zone == "time" {
                type_name.push(' ');
                type_name.push_str(&parser.peek().lexeme);
                parser.advance();
                if parser.peek().lexeme.eq_ignore_ascii_case("zone") {
                    type_name.push(' ');
                    type_name.push_str(&parser.peek().lexeme);
                    parser.advance();
                }
            }
        }
    }

    if parser.check(TokenKind::Dot) {
        parser.advance();
        let second_token = parser.peek().clone();
        match second_token.kind {
            TokenKind::Identifier | TokenKind::Keyword(_) => {
                parser.advance();
                type_name.push('.');
                type_name.push_str(&second_token.lexeme);
            }
            _ => {
                return Err(ParseError::unexpected(
                    &second_token,
                    "second part of qualified type name",
                ))
            }
        }
    }

    // SERIAL / BIGSERIAL / SMALLSERIAL are column shorthand, not real types.
    if let Some(kind) = SerialKind::by_name(&type_name) {
        return Ok(ColumnType::new_serial(kind.base().oid()));
    }

    let pg = PgType::by_name(&type_name);
    let pg = match pg {
        Some(pg) => pg,
        None => {
            // Not a builtin type: allow user-defined types (CREATE TYPE) and
            // domains (CREATE DOMAIN) registered in the catalog, matching
            // PostgreSQL's type resolution for column definitions.
            if let Some(oid) = parser.catalog().user_type_oid(&type_name) {
                return Ok(ColumnType::new(
                    plomid_types::TypeOid(oid),
                    plomid_types::NO_TYPEMOD,
                ));
            }
            return Err(ParseError::Unsupported {
                message: format!(
                    "type \"{}\" is not supported",
                    type_name.to_ascii_lowercase()
                ),
                detail: None,
            });
        }
    };
    if !pg.can_be_column_type() {
        return Err(ParseError::Unsupported {
            message: format!("type \"{}\" cannot be used as a column type", pg.name()),
            detail: None,
        });
    }

    let is_array = if parser.check(TokenKind::LBracket) {
        parser.advance();
        parser.consume(TokenKind::RBracket, "]")?;
        true
    } else {
        false
    };

    let mut typmod = NO_TYPEMOD;
    if !is_array && parser.check(TokenKind::LParen) {
        parser.advance();
        let mut args = Vec::new();
        loop {
            let amount = parser
                .consume(TokenKind::IntegerLiteral, "type modifier length")?
                .lexeme
                .parse::<i64>()
                .map_err(|_| ParseError::unexpected(&token, "valid type modifier length"))?;
            args.push(amount);
            if parser.check(TokenKind::Comma) {
                parser.advance();
            } else {
                break;
            }
        }
        parser.consume(TokenKind::RParen, ")")?;
        let spec = TypmodSpec::parse(&format!(
            "({})",
            args.iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ))
        .map_err(|error| ParseError::Unsupported {
            message: error,
            detail: None,
        })?;
        typmod = encode_typmod(pg, spec).map_err(|error| ParseError::Unsupported {
            message: error,
            detail: None,
        })?;
    }

    let type_oid = if is_array {
        pg.array_oid().ok_or_else(|| ParseError::Unsupported {
            message: format!("type \"{}\" has no array type", pg.name()),
            detail: None,
        })?
    } else {
        pg.oid()
    };
    Ok(ColumnType::new(type_oid, typmod))
}
