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
// DELETE statement parsing (`DELETE FROM t [USING ...] [WHERE ...] [RETURNING ...]`).
//
// The optional trailing `RETURNING` uses the same target grammar as
// `INSERT ... RETURNING` and is stored on `Statement::Delete::returning` for
// `dml::execute_delete[_txn]` to project the deleted rows.
//
// PostgreSQL's `DELETE ... USING` source relations are parsed with the same
// relation grammar as SELECT's FROM clause (tables with optional aliases,
// subqueries, joins; comma-separated relations form a cross join).
use super::{ParseError, Parser};
use crate::{
    ast::Statement,
    lexer::{Keyword, TokenKind},
};

pub fn parse_delete(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Delete), "DELETE")?;
    parser.consume(TokenKind::Keyword(Keyword::From), "FROM")?;
    let table = parser.parse_table_name()?;
    // Optional target alias (`DELETE FROM t d` / `DELETE FROM t AS d`).
    // Every clause keyword (`USING`, `WHERE`, `RETURNING`) lexes as
    // `Keyword`, never `Identifier`, so a bare alias is unambiguous.
    let alias = super::update::parse_target_alias(parser)?;

    // Optional `USING <relation>` source relations (PostgreSQL
    // `DELETE ... USING`), parsed with the same relation grammar as
    // SELECT's FROM clause.
    let mut using = None;
    if parser.check_keyword(Keyword::Using) {
        parser.advance();
        using = Some(super::select::parse_from_clause(parser)?);
    }

    let mut where_expr = None;
    if parser.check(TokenKind::Keyword(Keyword::Where)) {
        parser.advance();
        where_expr = Some(parser.parse_expression()?);
    }

    if parser.check_keyword(Keyword::Returning) {
        // Same shape as UPDATE: `RETURNING` ends the statement and yields a
        // SELECT target list (`*`, `col`, `t.*`, expressions).
        parser.advance();
        let returning = Some(super::select::parse_select_targets(parser)?);
        return Ok(Statement::Delete {
            table,
            alias,
            using,
            where_expr,
            returning,
        });
    }

    Ok(Statement::Delete {
        table,
        alias,
        using,
        where_expr,
        returning: None,
    })
}
