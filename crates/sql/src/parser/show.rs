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
    ast::Statement,
    lexer::{Keyword, TokenKind},
};

pub fn parse_show(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Show), "SHOW")?;
    if parser.check(TokenKind::SemiColon) || parser.check(TokenKind::Eof) {
        return Ok(Statement::Show {
            name: "tables".to_string(),
        });
    }
    if (matches!(parser.peek().kind, TokenKind::Identifier)
        && parser.peek().lexeme.eq_ignore_ascii_case("tables"))
        || matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Table))
    {
        parser.advance();
        return Ok(Statement::Show {
            name: "tables".to_string(),
        });
    }
    let name = parser.parse_qualified_identifier("parameter or object name")?;
    Ok(Statement::Show { name })
}
