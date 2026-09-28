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
//! Surface syntax for the document modality.
//!
//! Every place the SQL grammar *recognises* a JSON construct is collected here,
//! so "where is JSON parsed?" has exactly one answer:
//!
//! * the `JSON_TABLE(expr, 'path' COLUMNS (...))` table-function grammar,
//!   including per-column `PATH`, `EXISTS`, `ORDINALITY` and the
//!   `DEFAULT` / `ERROR ON ERROR` fallbacks;
//! * the argument and trailing-clause grammar shared by the keyword-headed
//!   `JSON(...)` and `JSON_OBJECT(...)` constructors — `RETURNING`, arity
//!   checks, `NULL ON NULL` / `ABSENT ON NULL`, `WITH [UNIQUE] KEYS`;
//! * the `IS [NOT] JSON [VALUE|OBJECT|ARRAY|SCALAR]` predicate.
//!
//! # Where the boundary sits
//!
//! A hybrid table keeps JSON in an ordinary column beside ordinary scalars, so a
//! modality owns *behaviour*, never its own table model. Each fact has one home:
//!
//! | Concern | Owner |
//! | --- | --- |
//! | what a JSON value *is* (`JsonbValue`, its text/wire form) | `plomid-types` |
//! | the `json` / `jsonb` column types and their OIDs | `plomid-types` registry |
//! | operators, path evaluation, subscripting, `JSON_TABLE` expansion | `plomid-json` |
//! | the vocabulary the parser emits (`JsonKind`, `JsonTableColumn`, `NullHandling`) | `plomid-json`, re-exported by `plomid-sql` |
//! | how a JSON construct is *written* in a statement | this module |
//!
//! # Why the grammar stays in the parser
//!
//! Recognising a construct is a statement-level job: it needs the token stream,
//! recursive expression parsing, the parser's SQL type names and
//! [`ParseError`]. `plomid-json` sits *below* the SQL layer and depends on
//! neither the lexer nor the AST, so hosting this code there would mean
//! inventing a token-abstraction trait with a single implementation — more
//! indirection in the grammar for no real gain in separation. The modality's
//! behaviour is fully decoupled; only its syntax is parsed in this crate.
//!
//! The identifier and string helpers are shared with [`super::expressions`]
//! (`pub(super)`) rather than duplicated, because `JSON_TABLE`, `JSON(...)` and
//! `JSON_OBJECT(...)` all lex their arguments exactly like an ordinary call.
//!
//! # Lowering JSON operators
//!
//! The parser only recognises an operator's *spelling*; what an operator *does*
//! is decided by the execution layer. The operator-to-function tables at the end
//! of this module are therefore the whole of the parser's knowledge about JSON
//! operator behaviour — the function names live there rather than being
//! open-coded in the binary-operator arms. Routing through scalar functions also
//! means `@?` and `@@` reuse the single JSONPath engine in `plomid-json`
//! instead of adding a second evaluator.

use super::expressions::{consume_identifier, parse_expression, parse_quoted_string};
use super::{ParseError, Parser};
use crate::{
    ast::{
        Expression, JsonKind, JsonTableColumn, JsonTableColumnDefault, JsonTableColumnKind, Value,
    },
    lexer::{Keyword, TokenKind},
};

/// True when the token at the cursor can start a SQL/JSON trailing
/// constructor clause (`RETURNING`, bare `NULL`/`ABSENT`, `WITH`,
/// `WITHOUT`, `DEFAULT`, `ERROR`, `FORMAT`).
pub(super) fn is_trailing_clause_start(parser: &Parser<'_>) -> bool {
    parser.check_keyword(Keyword::Returning)
        || parser.check_keyword(Keyword::Null)
        || parser.check_keyword(Keyword::Absent)
        || parser.check_keyword(Keyword::With)
        || parser.check_keyword(Keyword::Without)
        || parser.check_keyword(Keyword::Default)
        || parser.check_keyword(Keyword::Error)
        || parser.check_keyword(Keyword::Format)
}

/// True when the token `offset` ahead can start a trailing clause.
/// Used after a `,` to decide whether the comma separates another value
/// argument or introduces `, RETURNING ...` / `, NULL ON NULL` / ...
pub(super) fn is_trailing_clause_ahead(parser: &Parser<'_>, offset: usize) -> bool {
    let tok = parser.peek_n(offset);
    matches!(
        tok.kind,
        TokenKind::Keyword(
            Keyword::Returning
                | Keyword::Null
                | Keyword::Absent
                | Keyword::With
                | Keyword::Without
                | Keyword::Default
                | Keyword::Error
                | Keyword::Format,
        )
    )
}

/// Parses `(args... [clause...])` for the keyword-headed `JSON(...)` call.
/// Returns `(args, returning, null_handling, unique_keys)`.
///
/// `JSON(...)` takes plain scalar args (no KEY/VALUE pairs), so this reuses
/// the generic comma-arg loop with clause-aware lookahead: a `,` continues
/// args only when the next token cannot start a trailing constructor clause
/// (`RETURNING`/`NULL ON NULL`/`ABSENT ON NULL`/`WITH...`/`DEFAULT...`/
/// `ERROR...`/`FORMAT...`). A *bare* (comma-less) `NULL`/`ABSENT` arg
/// immediately followed by `ON` is the SQL/JSON `... ON NULL` clause that
/// `parse_expression` already consumed as a NULL literal — drop the spurious
/// arg and parse the clause instead.
///
/// Parsed SQL/JSON call arguments: positional args, optional `RETURNING`
/// type name, null-handling, and wrapper-behavior flag.
type JsonCallArgs = (
    Vec<Expression>,
    Option<String>,
    Option<crate::ast::NullHandling>,
    Option<bool>,
);
pub(super) fn parse_json_call_args(parser: &mut Parser<'_>) -> Result<JsonCallArgs, ParseError> {
    let mut fargs = Vec::new();
    if !parser.check(TokenKind::RParen) {
        fargs.push(parse_expression(parser)?);
        while parser.check(TokenKind::Comma) {
            if is_trailing_clause_ahead(parser, 1) {
                break;
            }
            parser.advance();
            if is_trailing_clause_start(parser) {
                break;
            }
            fargs.push(parse_expression(parser)?);
        }
        // Bare `... NULL ON NULL` / `... ABSENT ON NULL` without a comma:
        // parse_expression consumed the NULL/ABSENT-adjacent token as a
        // literal arg; detect and convert it into the clause.
        if matches!(fargs.last(), Some(Expression::Literal(Value::Null)))
            && (parser.check_keyword(Keyword::On)
                || parser.peek().lexeme.eq_ignore_ascii_case("on"))
        {
            fargs.pop();
        }
    }
    // Trailing clauses for keyword-headed JSON(...): only RETURNING is
    // valid here, but tolerate NULL/ABSENT ON NULL etc. for symmetry.
    let mut ret: Option<String> = None;
    let mut nh: Option<crate::ast::NullHandling> = None;
    let mut uq: Option<bool> = None;
    loop {
        if parser.check(TokenKind::RParen) {
            break;
        }
        if parser.check(TokenKind::Comma) {
            if is_trailing_clause_ahead(parser, 1) {
                parser.advance();
                continue;
            }
            break;
        }
        if nh.is_none() && parser.check_keyword(Keyword::Null) {
            parser.advance();
            parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
            parser.consume(TokenKind::Keyword(Keyword::Null), "NULL")?;
            nh = Some(crate::ast::NullHandling::NullOnNull);
            continue;
        }
        if nh.is_none() && parser.check_keyword(Keyword::Absent) {
            parser.advance();
            parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
            parser.consume(TokenKind::Keyword(Keyword::Null), "NULL")?;
            nh = Some(crate::ast::NullHandling::AbsentOnNull);
            continue;
        }
        if ret.is_none() && parser.check_keyword(Keyword::Returning) {
            parser.advance();
            ret = Some(parser.parse_type_name("RETURNING data type")?);
            continue;
        }
        break;
    }
    let _ = &mut uq;
    Ok((fargs, ret, nh, uq))
}

/// Parses one or more `KEY k VALUE v [, KEY k VALUE v ...]` pairs used by
/// `JSON_OBJECT(...)`.  Falls back to comma-separated expressions whenever the
/// KEY token is not seen, which covers `json_object(ARRAY[k, v, ...])`.
pub(super) fn parse_json_object_pairs(
    parser: &mut Parser<'_>,
    args: &mut Vec<Expression>,
) -> Result<(), ParseError> {
    if parser.check_keyword(Keyword::Key) {
        loop {
            parser.consume(TokenKind::Keyword(Keyword::Key), "KEY")?;
            args.push(parse_expression(parser)?);
            parser.consume(TokenKind::Keyword(Keyword::Value), "VALUE")?;
            // Parse the VALUE expression. Clause keywords (RETURNING / WITH /
            // WITHOUT / bare NULL|ABSENT ON NULL) terminate the value; a
            // consumed bare NULL followed by ON is the clause, not the value.
            if is_trailing_clause_start(parser) {
                let followed_by_on =
                    parser.check_keyword(Keyword::Null) || parser.check_keyword(Keyword::Absent);
                if followed_by_on {
                    let is_clause = parser.peek_n(1).kind == TokenKind::Keyword(Keyword::On)
                        || parser.peek_n(1).lexeme.eq_ignore_ascii_case("on");
                    if !is_clause {
                        args.push(parse_expression(parser)?);
                    }
                }
                break;
            }
            args.push(parse_expression(parser)?);
            // A NULL literal just consumed followed by ON means the NULL was
            // really the start of `NULL ON NULL`: drop it, it's the clause.
            if matches!(args.last(), Some(Expression::Literal(Value::Null)))
                && (parser.check_keyword(Keyword::On)
                    || parser.peek().lexeme.eq_ignore_ascii_case("on"))
            {
                args.pop();
                break;
            }
            // Bare ABSENT/NULL ON NULL / RETURNING / WITH... after the value
            // (no comma): end pairs so the clause loop consumes it.
            if parser.check_keyword(Keyword::Returning)
                || parser.check_keyword(Keyword::With)
                || parser.check_keyword(Keyword::Without)
                || ((parser.check_keyword(Keyword::Absent) || parser.check_keyword(Keyword::Null))
                    && (parser.peek_n(1).kind == TokenKind::Keyword(Keyword::On)
                        || parser.peek_n(1).lexeme.eq_ignore_ascii_case("on")))
            {
                break;
            }
            // VALUE-COMMA-CHECK: `,` continues pairs only via `, KEY ...`.
            if !parser.check(TokenKind::Comma) {
                break;
            }
            if parser.peek_n(1).kind == TokenKind::Keyword(Keyword::Key) {
                parser.advance();
                continue;
            }
            parser.advance();
            if !parser.check_keyword(Keyword::Key) {
                // Trailing comma leads into constructor clauses (RETURNING /
                // NULL ON NULL / WITH UNIQUE KEYS); stop consuming pairs.
                break;
            }
        }
    } else {
        // Plain expression form: `json_object(array[...])`, plus the
        // SQL-standard pair forms `key : value` and `key VALUE value`
        // (e.g. `JSON_OBJECT('a' : 1, 'b' : 2 WITH UNIQUE KEYS)`) which use
        // `:` or the VALUE keyword as the key/value separator instead of KEY.
        args.push(parse_expression(parser)?);
        if parser.check(TokenKind::Colon) || parser.check_keyword(Keyword::Value) {
            parser.advance();
            args.push(parse_expression(parser)?);
        }
        while parser.check(TokenKind::Comma) {
            parser.advance();
            args.push(parse_expression(parser)?);
            if parser.check(TokenKind::Colon) || parser.check_keyword(Keyword::Value) {
                parser.advance();
                args.push(parse_expression(parser)?);
            }
        }
    }
    Ok(())
}

/// Parses a `JSON_TABLE(...)` function call, including the special
/// `COLUMNS (...)` clause that lives inside the parentheses.
pub(super) fn parse_json_table_call(parser: &mut Parser<'_>) -> Result<Expression, ParseError> {
    parser.consume(TokenKind::LParen, "(")?;
    let json_arg = parse_expression(parser)?;
    parser.consume(TokenKind::Comma, ",")?;
    let path_arg = parse_expression(parser)?;
    let args = vec![json_arg, path_arg];
    if parser.check(TokenKind::Comma) {
        parser.advance();
    }
    let columns = if parser.check_identifier("columns") {
        parser.advance();
        parse_json_table_columns(parser)?
    } else {
        Vec::new()
    };
    parser.consume(TokenKind::RParen, ")")?;
    parser.pending_json_columns = Some(columns);
    Ok(Expression::FunctionCall {
        name: "json_table".to_string(),
        args,
        distinct: false,
        filter: None,
        order_by: Vec::new(),
        returning: None,
        null_handling: None,
        unique_keys: None,
    })
}

/// Parses a JSON_TABLE `COLUMNS ( ... )` clause into structured column specs.
/// Accepts an optional leading `COLUMNS` keyword: the top-level caller
/// consumes it before dispatching, while `NESTED PATH ... COLUMNS (...)`
/// reaches this function with the keyword still present.
pub(super) fn parse_json_table_columns(
    parser: &mut Parser<'_>,
) -> Result<Vec<JsonTableColumn>, ParseError> {
    if parser.check_identifier("columns") {
        parser.advance();
    }
    parser.consume(TokenKind::LParen, "(")?;
    let mut columns = Vec::new();
    if !parser.check(TokenKind::RParen) {
        loop {
            columns.push(parse_json_table_column(parser)?);
            if parser.check(TokenKind::Comma) {
                parser.advance();
            } else {
                break;
            }
        }
    }
    parser.consume(TokenKind::RParen, ")")?;
    Ok(columns)
}

/// Parses a single JSON_TABLE column declaration.
pub(super) fn parse_json_table_column(
    parser: &mut Parser<'_>,
) -> Result<JsonTableColumn, ParseError> {
    if parser.check_identifier("nested") {
        parser.advance();
        consume_identifier(parser, "path", "PATH")?;
        let path = parse_quoted_string(parser)?;
        let columns = parse_json_table_columns(parser)?;
        return Ok(JsonTableColumn {
            name: None,
            type_name: None,
            kind: JsonTableColumnKind::Nested { columns },
            path: Some(path),
            default_mode: JsonTableColumnDefault::None,
            default_value: None,
            error_default_mode: JsonTableColumnDefault::None,
            error_default_value: None,
        });
    }

    let name = parser.parse_qualified_identifier("column name")?;

    if parser.check_keyword(Keyword::For) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Ordinality), "ORDINALITY")?;
        return Ok(JsonTableColumn {
            name: Some(name),
            type_name: None,
            kind: JsonTableColumnKind::Ordinality,
            path: None,
            default_mode: JsonTableColumnDefault::None,
            default_value: None,
            error_default_mode: JsonTableColumnDefault::None,
            error_default_value: None,
        });
    }

    let type_name = parser.parse_type_name("data type")?;

    if parser.check(TokenKind::Keyword(Keyword::Exists)) {
        parser.advance();
        consume_identifier(parser, "path", "PATH")?;
        let path = parse_quoted_string(parser)?;
        return Ok(JsonTableColumn {
            name: Some(name),
            type_name: Some(type_name),
            kind: JsonTableColumnKind::Exists,
            path: Some(path),
            default_mode: JsonTableColumnDefault::None,
            default_value: None,
            error_default_mode: JsonTableColumnDefault::None,
            error_default_value: None,
        });
    }

    if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Format))
        || parser.check_identifier("format")
    {
        parser.advance();
        if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Json))
            || parser.check_identifier("json")
        {
            parser.advance();
        }
    }
    if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Json))
        || parser.check_identifier("json")
    {
        // FORMAT JSON above may not have consumed "JSON" if FORMAT wasn't
        // detected because it came as a bare identifier; also accept the
        // explicit keyword Json if no FORMAT token but Json token precedes PATH.
        if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Json)) {
            parser.advance();
        }
    }
    // PATH
    if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::On)) && false {
        unreachable!()
    }
    if parser.check(TokenKind::Keyword(Keyword::Json)) && false {
        unreachable!()
    }
    if parser.check_keyword(Keyword::Path) || parser.check_identifier("path") {
        parser.advance();
    } else {
        consume_identifier(parser, "path", "PATH")?;
    }
    let path = parse_quoted_string(parser)?;

    let mut default_mode = JsonTableColumnDefault::None;
    let mut default_value: Option<String> = None;
    if parser.check_keyword(Keyword::Default) {
        parser.advance();
        let value = parse_quoted_string(parser)?;
        parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
        if parser.check_keyword(Keyword::Empty) || parser.check_identifier("empty") {
            parser.advance();
            if parser.check_keyword(Keyword::Error) || parser.check_identifier("error") {
                parser.advance();
                (default_mode, default_value) = (JsonTableColumnDefault::EmptyOrError, Some(value));
            } else {
                (default_mode, default_value) = (JsonTableColumnDefault::Empty, Some(value));
            }
        } else if parser.check_keyword(Keyword::Error) || parser.check_identifier("error") {
            parser.advance();
            (default_mode, default_value) = (JsonTableColumnDefault::Error, Some(value));
        } else {
            (default_mode, default_value) = (JsonTableColumnDefault::Empty, Some(value));
        }
    }

    // PostgreSQL accepts multiple DEFAULT clauses per column: a later
    // `DEFAULT ... ON ERROR` distinguishes conversion-error handling from the
    // `ON EMPTY` / `ON EMPTY OR ERROR` target of an earlier clause
    // (e.g. `DEFAULT '999' ON EMPTY DEFAULT '888' ON ERROR`).
    let mut error_default_mode = JsonTableColumnDefault::None;
    let mut error_default_value: Option<String> = None;
    while parser.check_keyword(Keyword::Default) {
        parser.advance();
        let value = parse_quoted_string(parser)?;
        parser.consume(TokenKind::Keyword(Keyword::On), "ON")?;
        let (mode, value) =
            if parser.check_keyword(Keyword::Empty) || parser.check_identifier("empty") {
                parser.advance();
                if parser.check_keyword(Keyword::Error) || parser.check_identifier("error") {
                    parser.advance();
                    (JsonTableColumnDefault::EmptyOrError, Some(value))
                } else {
                    (JsonTableColumnDefault::Empty, Some(value))
                }
            } else if parser.check_keyword(Keyword::Error) || parser.check_identifier("error") {
                parser.advance();
                (JsonTableColumnDefault::Error, Some(value))
            } else {
                (JsonTableColumnDefault::Empty, Some(value))
            };
        match mode {
            JsonTableColumnDefault::Error => {
                (error_default_mode, error_default_value) = (JsonTableColumnDefault::Error, value);
            }
            JsonTableColumnDefault::Empty => {
                (default_mode, default_value) = (JsonTableColumnDefault::Empty, value);
            }
            JsonTableColumnDefault::EmptyOrError => {
                (default_mode, default_value) =
                    (JsonTableColumnDefault::EmptyOrError, value.clone());
                (error_default_mode, error_default_value) =
                    (JsonTableColumnDefault::EmptyOrError, value);
            }
            JsonTableColumnDefault::None => {}
        }
    }

    Ok(JsonTableColumn {
        name: Some(name),
        type_name: Some(type_name),
        kind: JsonTableColumnKind::Regular,
        path: Some(path),
        default_mode,
        default_value,
        error_default_mode,
        error_default_value,
    })
}

/// Parses the `IS [NOT] JSON [VALUE|OBJECT|ARRAY|SCALAR]` predicate.
///
/// Called once the caller has consumed `IS [NOT]` and confirmed the cursor sits
/// on the `JSON` keyword. `negated` carries the `NOT`; `left` is the expression
/// under test.
pub(super) fn parse_is_json_predicate(
    parser: &mut Parser<'_>,
    left: Expression,
    negated: bool,
) -> Result<Expression, ParseError> {
    parser.advance(); // JSON

    // Optional shape qualifier; a bare `IS JSON` accepts any JSON document.
    let kind = if parser.check_keyword(Keyword::Value) {
        parser.advance();
        JsonKind::Value
    } else if parser.check_keyword(Keyword::Object) {
        parser.advance();
        JsonKind::Object
    } else if parser.check_keyword(Keyword::Array) {
        parser.advance();
        JsonKind::Array
    } else if parser.check_keyword(Keyword::Scalar) {
        parser.advance();
        JsonKind::Scalar
    } else {
        JsonKind::Any
    };

    Ok(Expression::IsJson {
        expr: Box::new(left),
        kind,
        negated,
    })
}

/// Scalar function implementing a JSON predicate operator.
///
/// Returns `json_contains` / `json_contained_by` for `@>` and `<@`, the
/// existence family for `?`, `?|` and `?&`, and the JSONPath operators `@?` and
/// `@@`.
///
/// The caller has already matched the operator token, so an unknown spelling is
/// a programming error rather than user input.
pub(super) fn predicate_function(operator: &str) -> &'static str {
    match operator {
        "@>" => "json_contains",
        "<@" => "json_contained_by",
        "?" => "json_exists",
        "?|" => "json_exists_any",
        "?&" => "json_exists_all",
        "@?" => "jsonb_path_exists_op",
        "@@" => "jsonb_path_match_op",
        other => unreachable!("no JSON predicate function for operator {other:?}"),
    }
}

/// Scalar function implementing the `#>` / `#>>` accessor operators, which walk
/// a `text[]` path. `#>` keeps the result as `json`; `#>>` renders it as `text`.
pub(super) fn accessor_function(as_text: bool) -> &'static str {
    if as_text {
        "json_path_text"
    } else {
        "json_path"
    }
}

/// Scalar function implementing `#-`, which deletes the value at a path.
pub(super) const DELETE_PATH_FUNCTION: &str = "jsonb_delete_path";

#[cfg(test)]
mod tests {
    //! Parser coverage for the JSON surface syntax defined in this module.
    use super::*;
    use crate::ast::{FromClause, JsonTableColumnDefault, JsonTableColumnKind, Statement};
    use crate::{InMemoryCatalog, Lexer, Parser};

    /// Parses a SELECT statement and returns its FROM clause.
    fn parse_from(sql: &str) -> FromClause {
        let tokens = Lexer::new(sql).lex().unwrap();
        let mut catalog = InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        match parser
            .parse_statements()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
        {
            Statement::Select {
                from: Some(from), ..
            } => from,
            other => panic!("expected SELECT with FROM, got {other:?}"),
        }
    }

    fn json_table_from(sql: &str) -> (String, Vec<JsonTableColumn>) {
        match parse_from(sql) {
            FromClause::TableFunction {
                name, json_columns, ..
            } => (name, json_columns),
            other => panic!("expected TableFunction, got {other:?}"),
        }
    }

    #[test]
    fn json_table_basic_columns_parse() {
        let (name, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[{\"id\":1}]', '$[*]' COLUMNS (id INTEGER PATH '$.id', name TEXT PATH '$.name', active BOOLEAN PATH '$.active')) AS jt;",
        );
        assert_eq!(name, "json_table");
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].name.as_deref(), Some("id"));
        assert_eq!(columns[0].type_name.as_deref(), Some("INTEGER"));
        assert_eq!(columns[0].path.as_deref(), Some("$.id"));
        assert!(matches!(columns[0].kind, JsonTableColumnKind::Regular));
        assert_eq!(columns[1].name.as_deref(), Some("name"));
        assert_eq!(columns[1].type_name.as_deref(), Some("TEXT"));
        assert!(matches!(columns[1].kind, JsonTableColumnKind::Regular));
        assert_eq!(columns[2].name.as_deref(), Some("active"));
        assert_eq!(columns[2].type_name.as_deref(), Some("BOOLEAN"));
    }

    #[test]
    fn json_table_ordinality_column_parses() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (rn FOR ORDINALITY, name TEXT PATH '$.name')) AS jt;",
        );
        assert!(matches!(columns[0].kind, JsonTableColumnKind::Ordinality));
        assert_eq!(columns[0].name.as_deref(), Some("rn"));
        assert!(columns[0].type_name.is_none());
        assert!(matches!(columns[1].kind, JsonTableColumnKind::Regular));
    }

    #[test]
    fn json_table_exists_column_parses() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (has_active BOOLEAN EXISTS PATH '$.active')) AS jt;",
        );
        assert!(matches!(columns[0].kind, JsonTableColumnKind::Exists));
        assert_eq!(columns[0].type_name.as_deref(), Some("BOOLEAN"));
        assert_eq!(columns[0].path.as_deref(), Some("$.active"));
    }

    #[test]
    fn json_table_format_json_column_parses() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (tags JSON FORMAT JSON PATH '$.tags')) AS jt;",
        );
        assert!(matches!(columns[0].kind, JsonTableColumnKind::Regular));
        assert_eq!(columns[0].type_name.as_deref(), Some("JSON"));
        assert_eq!(columns[0].path.as_deref(), Some("$.tags"));
    }

    #[test]
    fn json_table_default_on_empty_parses() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (name TEXT PATH '$.name' DEFAULT '\"UNKNOWN\"' ON EMPTY)) AS jt;",
        );
        assert_eq!(columns[0].default_mode, JsonTableColumnDefault::Empty);
        assert_eq!(columns[0].default_value.as_deref(), Some("\"UNKNOWN\""));
    }

    #[test]
    fn json_table_on_empty_and_on_error_parse() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (id INTEGER PATH '$.id' DEFAULT '999' ON EMPTY DEFAULT '888' ON ERROR)) AS jt;",
        );
        assert_eq!(columns[0].default_mode, JsonTableColumnDefault::Empty);
        assert_eq!(columns[0].default_value.as_deref(), Some("999"));
        assert_eq!(columns[0].error_default_mode, JsonTableColumnDefault::Error);
        assert_eq!(columns[0].error_default_value.as_deref(), Some("888"));
    }

    #[test]
    fn json_table_default_on_error_parses() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('[]', '$[*]' COLUMNS (id INTEGER PATH '$.id' DEFAULT '999' ON ERROR)) AS jt;",
        );
        assert_eq!(columns[0].default_mode, JsonTableColumnDefault::Error);
        assert_eq!(columns[0].default_value.as_deref(), Some("999"));
        assert_eq!(columns[0].error_default_mode, JsonTableColumnDefault::None);
        assert_eq!(columns[0].error_default_value, None);
    }

    #[test]
    fn json_table_nested_columns_parse() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('{}', '$[*]' COLUMNS (id INTEGER PATH '$.id', NESTED PATH '$.orders[*]' COLUMNS (order_id INTEGER PATH '$.id'))) AS jt;",
        );
        assert!(matches!(columns[0].kind, JsonTableColumnKind::Regular));
        let JsonTableColumnKind::Nested { columns: nested } = &columns[1].kind else {
            panic!("expected nested column, got {:?}", columns[1].kind);
        };
        assert!(columns[1].name.is_none());
        assert_eq!(columns[1].path.as_deref(), Some("$.orders[*]"));
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].name.as_deref(), Some("order_id"));
    }

    #[test]
    fn json_table_multi_level_nested_parses() {
        let (_, columns) = json_table_from(
            "SELECT * FROM JSON_TABLE('{}', '$[*]' COLUMNS (dept TEXT PATH '$.name', NESTED PATH '$.teams[*]' COLUMNS (team TEXT PATH '$.name', NESTED PATH '$.members[*]' COLUMNS (member TEXT PATH '$.name')))) AS jt;",
        );
        let JsonTableColumnKind::Nested { columns: teams } = &columns[1].kind else {
            panic!("expected nested teams column");
        };
        let JsonTableColumnKind::Nested { columns: members } = &teams[1].kind else {
            panic!("expected nested members column");
        };
        assert_eq!(members[0].name.as_deref(), Some("member"));
    }

    #[test]
    fn json_table_column_without_path_is_rejected() {
        // A regular column must declare its PATH; accepting a bare
        // `id INTEGER` would silently bind an implicit path.
        let tokens = Lexer::new("SELECT * FROM JSON_TABLE('{}', '$' COLUMNS (id INTEGER)) AS jt;")
            .lex()
            .unwrap();
        let mut catalog = InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        assert!(
            parser.parse_statements().is_err(),
            "COLUMNS (id INTEGER) without PATH must not parse"
        );
    }
}
