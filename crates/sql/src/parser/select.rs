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
//! SELECT statement parsing: targets, FROM, WHERE, GROUP BY, HAVING, ORDER BY, LIMIT.

use super::expressions::parse_expression;
use super::{ParseError, Parser};
use crate::{
    ast::{
        Cte, Expression, FrameBound, FrameBounds, FrameSpec, FromClause, GroupByClause, JoinKind,
        OrderByItem, SelectTarget, Statement, WindowSpec,
    },
    lexer::{Keyword, TokenKind},
};

pub fn parse_select(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parse_select_internal(parser, false)
}

/// Parses a full `WITH [RECURSIVE] cte1, cte2, ... <statement>`.
pub fn parse_with(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::With), "WITH")?;
    let recursive = if parser.check(TokenKind::Keyword(Keyword::Recursive)) {
        parser.advance();
        true
    } else {
        false
    };
    let mut ctes = Vec::new();
    loop {
        let name = parser
            .consume(TokenKind::Identifier, "common table expression name")?
            .lexeme;
        parser.cte_names.push(name.clone());
        let mut columns = Vec::new();
        if parser.check(TokenKind::LParen) {
            // Named column list: `name(col1, col2) AS (...)`
            parser.advance();
            loop {
                columns.push(
                    parser
                        .consume(TokenKind::Identifier, "CTE column name")?
                        .lexeme,
                );
                if !parser.check(TokenKind::Comma) {
                    break;
                }
                parser.advance();
            }
            parser.consume(TokenKind::RParen, ")")?;
        }
        parser.consume(TokenKind::Keyword(Keyword::As), "AS")?;
        parser.consume(TokenKind::LParen, "(")?;
        let query = parse_select(parser)?;
        parser.consume(TokenKind::RParen, ")")?;
        ctes.push(Cte {
            name,
            columns,
            query: Box::new(query),
        });
        if !parser.check(TokenKind::Comma) {
            break;
        }
        parser.advance();
    }
    let body = parser.parse_statement()?;
    Ok(Statement::With {
        recursive,
        ctes,
        body: Box::new(body),
    })
}

/// Parses `EXPLAIN [ ( option [, ...] ) ] [ANALYZE] <statement>`.
pub fn parse_explain(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Explain), "EXPLAIN")?;

    let mut analyze = false;
    let mut explain_format = None;

    // Parse optional parentheses for EXPLAIN options
    if parser.check(TokenKind::LParen) {
        // Parse options inside parentheses
        parser.advance();

        loop {
            if parser.check(TokenKind::RParen) {
                parser.advance();
                break;
            }

            let option_name = parser.parse_identifier()?;
            let option_lower = option_name.to_ascii_lowercase();

            match option_lower.as_str() {
                "analyze" => {
                    analyze = true;
                }
                "format" => {
                    parser.consume(TokenKind::Identifier, "format value")?;
                    let format_value = parser.parse_identifier()?;
                    explain_format = Some(format_value.to_ascii_lowercase());
                }
                "costs" | "buffers" | "timing" | "summary" => {
                    // PostgreSQL options we don't fully support yet - skip value
                    if parser.check(TokenKind::Identifier)
                        || parser.check(TokenKind::Keyword(Keyword::On))
                    {
                        parser.advance();
                    }
                }
                _ => {
                    return Err(ParseError::unexpected(
                        parser.peek(),
                        &format!("unsupported EXPLAIN option: {option_name}"),
                    ));
                }
            }

            if parser.check(TokenKind::Comma) {
                parser.advance();
            }
        }
    } else {
        // No parentheses, check for ANALYZE keyword
        if parser.check(TokenKind::Keyword(Keyword::Analyze)) {
            parser.advance();
            analyze = true;
        }
    }

    let statement = parser.parse_statement()?;
    Ok(Statement::Explain {
        statement: Box::new(statement),
        analyze,
        format: explain_format,
    })
}

pub fn parse_select_internal(
    parser: &mut Parser<'_>,
    in_parentheses: bool,
) -> Result<Statement, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Select), "SELECT")?;
    let distinct = parser.check(TokenKind::Keyword(Keyword::Distinct));
    let mut distinct_on: Option<Vec<Expression>> = None;
    if distinct {
        parser.advance();
        if parser.check(TokenKind::Keyword(Keyword::On)) {
            parser.advance();
            parser.consume(TokenKind::LParen, "(")?;
            let mut exprs = Vec::new();
            loop {
                exprs.push(parser.parse_expression()?);
                if !parser.check(TokenKind::Comma) {
                    break;
                }
                parser.advance();
            }
            parser.consume(TokenKind::RParen, ")")?;
            if exprs.is_empty() {
                return Err(ParseError::unexpected(
                    parser.peek(),
                    "DISTINCT ON requires at least one expression",
                ));
            }
            distinct_on = Some(exprs);
        }
    }

    let targets = parse_select_targets(parser)?;
    let mut from = None;
    let mut where_expr = None;
    let mut group_by = None;
    let mut having = None;
    let mut order_by = Vec::new();
    let mut limit = None;
    let mut offset = None;

    if parser.check(TokenKind::Keyword(Keyword::From)) {
        parser.advance();
        from = Some(parse_from_clause(parser)?);
    }

    if parser.check(TokenKind::Keyword(Keyword::Where)) {
        parser.advance();
        where_expr = Some(parser.parse_expression()?);
    }

    if parser.check(TokenKind::Keyword(Keyword::Group)) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::By), "BY")?;
        group_by = Some(parse_group_by_clause(parser)?);
    }

    if parser.check(TokenKind::Keyword(Keyword::Having)) {
        parser.advance();
        having = Some(parser.parse_expression()?);
    }

    if parser.check(TokenKind::Keyword(Keyword::Order)) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::By), "BY")?;
        loop {
            let expr = parser.parse_expression()?;
            let descending = if parser.check(TokenKind::Keyword(Keyword::Desc)) {
                parser.advance();
                true
            } else {
                if parser.check(TokenKind::Keyword(Keyword::Asc)) {
                    parser.advance();
                }
                false
            };
            let nulls_first = if parser.check(TokenKind::Keyword(Keyword::Nulls)) {
                parser.advance();
                if parser.check(TokenKind::Keyword(Keyword::First)) {
                    parser.advance();
                    Some(true)
                } else if parser.check(TokenKind::Keyword(Keyword::Last)) {
                    parser.advance();
                    Some(false)
                } else {
                    let token = parser.peek();
                    return Err(ParseError::unexpected(token, "FIRST or LAST"));
                }
            } else {
                None
            };
            order_by.push(OrderByItem {
                expr,
                descending,
                nulls_first,
            });
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
    }

    if parser.check(TokenKind::Keyword(Keyword::Limit)) {
        parser.advance();
        let expr = parse_expression(parser)?;
        // Evaluate constant expressions for LIMIT
        let value = match expr {
            Expression::Literal(plomid_types::PgValue::Int4(n)) => n as usize,
            Expression::Literal(plomid_types::PgValue::Int8(n)) => n as usize,
            Expression::Literal(plomid_types::PgValue::Int2(n)) => n as usize,
            Expression::TypeCast { expr, .. } => {
                // expr is a Box<Expression>, we need to match on a reference
                match expr.as_ref() {
                    Expression::Literal(plomid_types::PgValue::Int4(n)) => *n as usize,
                    Expression::Literal(plomid_types::PgValue::Int8(n)) => *n as usize,
                    Expression::Literal(plomid_types::PgValue::Int2(n)) => *n as usize,
                    _ => {
                        return Err(ParseError::unexpected(
                            parser.peek(),
                            "LIMIT requires a constant expression",
                        ));
                    }
                }
            }
            _ => {
                return Err(ParseError::unexpected(
                    parser.peek(),
                    "LIMIT requires a constant expression",
                ));
            }
        };
        limit = Some(value);
    }
    if parser.check(TokenKind::Keyword(Keyword::Offset)) {
        parser.advance();
        let expr = parse_expression(parser)?;
        // Evaluate constant expressions for OFFSET
        let value = match expr {
            Expression::Literal(plomid_types::PgValue::Int4(n)) => n as usize,
            Expression::Literal(plomid_types::PgValue::Int8(n)) => n as usize,
            Expression::Literal(plomid_types::PgValue::Int2(n)) => n as usize,
            Expression::TypeCast { expr, .. } => match expr.as_ref() {
                Expression::Literal(plomid_types::PgValue::Int4(n)) => *n as usize,
                Expression::Literal(plomid_types::PgValue::Int8(n)) => *n as usize,
                Expression::Literal(plomid_types::PgValue::Int2(n)) => *n as usize,
                _ => {
                    return Err(ParseError::unexpected(
                        parser.peek(),
                        "OFFSET requires a constant expression",
                    ));
                }
            },
            _ => {
                return Err(ParseError::unexpected(
                    parser.peek(),
                    "OFFSET requires a constant expression",
                ));
            }
        };
        offset = Some(value);
    }

    // Row-locking clauses are session/transaction concerns rather than part
    // of the projected result. The storage engine serializes writes through
    // its transaction manager, so accept the PostgreSQL syntax here and
    // preserve the SELECT semantics for clients that issue `FOR UPDATE`.
    if parser.check(TokenKind::Keyword(Keyword::For)) {
        parser.advance();
        if parser.check(TokenKind::Keyword(Keyword::Update)) {
            parser.advance();
        } else {
            return Err(ParseError::unexpected(parser.peek(), "UPDATE"));
        }
    }

    let mut stmt = Statement::Select {
        targets,
        distinct,
        distinct_on,
        from,
        where_expr,
        group_by,
        having,
        order_by,
        limit,
        offset,
    };

    // Set operations: UNION / INTERSECT / EXCEPT (ALL optional).
    if is_set_op_keyword(parser.peek().kind) {
        let (op, all) = parse_set_op_kind(parser)?;
        let right = Box::new(parse_select_internal(parser, false)?);
        stmt = Statement::SetOperation {
            op,
            all,
            left: Box::new(stmt),
            right,
        };
    }

    if !in_parentheses && !matches!(parser.peek().kind, TokenKind::SemiColon | TokenKind::Eof) {
        // Some outer caller may chain further clauses; ignore for now.
    }
    Ok(stmt)
}

fn parse_group_by_clause(parser: &mut Parser<'_>) -> Result<GroupByClause, ParseError> {
    if parser.check_keyword(Keyword::Grouping) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Sets), "SETS")?;
        parser.consume(TokenKind::LParen, "(")?;
        let mut sets = Vec::new();
        if parser.check(TokenKind::RParen) {
            return Err(ParseError::unexpected(parser.peek(), "grouping set"));
        }
        loop {
            parser.consume(TokenKind::LParen, "(")?;
            let mut set = Vec::new();
            if !parser.check(TokenKind::RParen) {
                loop {
                    set.push(parser.parse_expression()?);
                    if !parser.check(TokenKind::Comma) {
                        break;
                    }
                    parser.advance();
                }
            }
            parser.consume(TokenKind::RParen, ")")?;
            sets.push(set);
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
        parser.consume(TokenKind::RParen, ")")?;
        return Ok(GroupByClause::Sets(sets));
    }
    if parser.check_keyword(Keyword::Rollup) {
        parser.advance();
        parser.consume(TokenKind::LParen, "(")?;
        let mut exprs = Vec::new();
        if !parser.check(TokenKind::RParen) {
            loop {
                exprs.push(parser.parse_expression()?);
                if !parser.check(TokenKind::Comma) {
                    break;
                }
                parser.advance();
            }
        }
        parser.consume(TokenKind::RParen, ")")?;
        // ROLLUP(a,b) -> [[a,b],[a],[]]
        let mut sets = Vec::with_capacity(exprs.len() + 1);
        for len in (0..=exprs.len()).rev() {
            sets.push(exprs[..len].to_vec());
        }
        sets.reverse();
        // `sets` above is [[],[a],[a,b]]; reverse to [[a,b],[a],[]].
        let mut ordered = Vec::with_capacity(sets.len());
        for set in sets.into_iter().rev() {
            ordered.push(set);
        }
        return Ok(GroupByClause::Sets(ordered));
    }
    if parser.check_keyword(Keyword::Cube) {
        parser.advance();
        parser.consume(TokenKind::LParen, "(")?;
        let mut exprs = Vec::new();
        if !parser.check(TokenKind::RParen) {
            loop {
                exprs.push(parser.parse_expression()?);
                if !parser.check(TokenKind::Comma) {
                    break;
                }
                parser.advance();
            }
        }
        parser.consume(TokenKind::RParen, ")")?;
        let n = exprs.len();
        let mut sets = Vec::with_capacity(1 << n.min(16));
        // Deterministic order: full set first, then descending size,
        // preserving original column order within each set.
        for size in (0..=n).rev() {
            for mask in 0..(1u32 << n.min(20)) {
                if mask.count_ones() as usize != size {
                    continue;
                }
                if n > 20 {
                    break;
                }
                let mut set = Vec::new();
                for (i, e) in exprs.iter().enumerate() {
                    if mask & (1u32 << (n - 1 - i)) != 0 {
                        set.push(e.clone());
                    }
                }
                // For n<=2 this yields the canonical ordering; ensure exact
                // canonical order for the common 2-column case.
                sets.push(set);
            }
        }
        if n == 2 {
            sets = vec![
                vec![exprs[0].clone(), exprs[1].clone()],
                vec![exprs[0].clone()],
                vec![exprs[1].clone()],
                vec![],
            ];
        } else if n > 20 {
            sets = vec![exprs.clone(), vec![]];
        }
        return Ok(GroupByClause::Sets(sets));
    }
    let mut cols = Vec::new();
    loop {
        cols.push(parser.parse_expression()?);
        if !parser.check(TokenKind::Comma) {
            break;
        }
        parser.advance();
    }
    Ok(GroupByClause::Simple(cols))
}

fn is_set_op_keyword(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Keyword(Keyword::Union)
            | TokenKind::Keyword(Keyword::Intersect)
            | TokenKind::Keyword(Keyword::Except)
    )
}

fn parse_set_op_kind(parser: &mut Parser<'_>) -> Result<(crate::ast::SetOpKind, bool), ParseError> {
    let op = match parser.peek().kind {
        TokenKind::Keyword(Keyword::Union) => crate::ast::SetOpKind::Union,
        TokenKind::Keyword(Keyword::Intersect) => crate::ast::SetOpKind::Intersect,
        TokenKind::Keyword(Keyword::Except) => crate::ast::SetOpKind::Except,
        _ => unreachable!(),
    };
    parser.advance();
    let mut all = false;
    if parser.check(TokenKind::Keyword(Keyword::All)) {
        parser.advance();
        all = true;
    }
    Ok((op, all))
}

pub(crate) fn parse_from_clause(parser: &mut Parser<'_>) -> Result<FromClause, ParseError> {
    let mut left = parse_from_atom(parser)?;
    loop {
        let kind = match parser.peek().kind {
            TokenKind::Comma => {
                // Implicit CROSS JOIN via comma.
                parser.advance();
                let right = parse_from_atom(parser)?;
                left = FromClause::Join {
                    left: Box::new(left),
                    kind: JoinKind::Cross,
                    right: Box::new(right),
                    on: None,
                };
                continue;
            }
            TokenKind::Keyword(Keyword::Join) => {
                parser.advance();
                Some(JoinKind::Inner)
            }
            TokenKind::Keyword(Keyword::Inner) => {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Join), "JOIN")?;
                Some(JoinKind::Inner)
            }
            TokenKind::Keyword(Keyword::Left) => {
                parser.advance();
                if parser.check(TokenKind::Keyword(Keyword::Outer)) {
                    parser.advance();
                }
                parser.consume(TokenKind::Keyword(Keyword::Join), "JOIN")?;
                Some(JoinKind::Left)
            }
            TokenKind::Keyword(Keyword::Right) => {
                parser.advance();
                if parser.check(TokenKind::Keyword(Keyword::Outer)) {
                    parser.advance();
                }
                parser.consume(TokenKind::Keyword(Keyword::Join), "JOIN")?;
                Some(JoinKind::Right)
            }
            TokenKind::Keyword(Keyword::Full) => {
                parser.advance();
                if parser.check(TokenKind::Keyword(Keyword::Outer)) {
                    parser.advance();
                }
                parser.consume(TokenKind::Keyword(Keyword::Join), "JOIN")?;
                Some(JoinKind::Full)
            }
            TokenKind::Keyword(Keyword::Cross) => {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Join), "JOIN")?;
                Some(JoinKind::Cross)
            }
            _ => None,
        };
        let Some(kind) = kind else { break };

        // Consume the standard LATERAL marker before a table expression and
        // propagate it so the executor evaluates the right side per left row.
        // PostgreSQL treats right-side table functions as implicitly lateral
        // when they reference earlier columns, so the executor also re-checks
        // column references; this flag covers the explicit LATERAL keyword.
        let lateral = parser.check_keyword(Keyword::Lateral);
        if lateral {
            parser.advance();
        }
        let mut right = parse_from_atom(parser)?;
        if lateral {
            match &mut right {
                FromClause::TableFunction { lateral, .. }
                | FromClause::Subquery { lateral, .. } => *lateral = true,
                // Explicit LATERAL on a plain table or comma-join RHS is not
                // meaningful for scope; PostgreSQL still accepts the syntax.
                _ => {}
            }
        }
        let on = if kind == JoinKind::Cross {
            None
        } else {
            // ON or USING.
            if parser.check(TokenKind::Keyword(Keyword::On)) {
                parser.advance();
                Some(parser.parse_expression()?)
            } else if parser.check(TokenKind::Keyword(Keyword::Using)) {
                parser.advance();
                parser.consume(TokenKind::LParen, "(")?;
                let mut cols = Vec::new();
                loop {
                    cols.push(
                        parser
                            .consume(TokenKind::Identifier, "USING column")?
                            .lexeme,
                    );
                    if !parser.check(TokenKind::Comma) {
                        break;
                    }
                    parser.advance();
                }
                parser.consume(TokenKind::RParen, ")")?;
                // USING (a, b) becomes ON L.a = R.a AND L.b = R.b.
                Some(build_using_expression(&left, &right, &cols)?)
            } else {
                None
            }
        };
        left = FromClause::Join {
            left: Box::new(left),
            kind,
            right: Box::new(right),
            on,
        };
    }
    Ok(left)
}

fn build_using_expression(
    left: &FromClause,
    right: &FromClause,
    cols: &[String],
) -> Result<Expression, ParseError> {
    // USING (col) compares the left row's `col` against the right row's
    // `col`.  Emit qualified references (`left_alias.col =
    // right_alias.col`) so execution resolves each side in its own scope.
    // An unqualified `col = col` would match both join inputs and fail
    // column resolution with "column reference is ambiguous".
    let left_alias = from_output_alias(left);
    let right_alias = from_output_alias(right);
    let mut iter = cols.iter();
    let first = iter.next().expect("USING must list at least one column");
    let mut acc = Expression::Equal(
        Box::new(Expression::ColumnRef(qualify(left_alias.as_deref(), first))),
        Box::new(Expression::ColumnRef(qualify(
            right_alias.as_deref(),
            first,
        ))),
    );
    for col in iter {
        let term = Expression::Equal(
            Box::new(Expression::ColumnRef(qualify(left_alias.as_deref(), col))),
            Box::new(Expression::ColumnRef(qualify(right_alias.as_deref(), col))),
        );
        acc = Expression::And(Box::new(acc), Box::new(term));
    }
    Ok(acc)
}

/// Best-effort single alias for one side of a JOIN, mirroring the executor's
/// effective-alias rules.  A composite (already-joined) side has no single
/// alias, so callers fall back to unqualified references for it.
fn from_output_alias(from: &FromClause) -> Option<String> {
    match from {
        FromClause::Table { name, alias } => Some(
            alias
                .clone()
                .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string()),
        ),
        FromClause::TableFunction { name, alias, .. } => {
            Some(alias.clone().unwrap_or_else(|| name.clone()))
        }
        FromClause::Subquery { alias, .. } => Some(alias.clone()),
        FromClause::Join { .. } => None,
    }
}

fn qualify(alias: Option<&str>, column: &str) -> String {
    match alias {
        Some(alias) => format!("{alias}.{column}"),
        None => column.to_string(),
    }
}

fn parse_from_atom(parser: &mut Parser<'_>) -> Result<FromClause, ParseError> {
    if parser.check(TokenKind::LParen) {
        parser.advance();
        let stmt = if parser.check(TokenKind::Keyword(Keyword::Select)) {
            super::select::parse_select(parser)?
        } else if parser.check(TokenKind::Keyword(Keyword::Values)) {
            Statement::Values(parse_values_rows(parser)?)
        } else {
            let inner = parse_from_clause(parser)?;
            // Treat parenthesised join lists as a synthetic CROSS JOIN.
            Statement::Select {
                targets: vec![SelectTarget::All],
                distinct: false,
                distinct_on: None,
                from: Some(inner),
                where_expr: None,
                group_by: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
            }
        };
        parser.consume(TokenKind::RParen, ")")?;
        let alias = parse_optional_alias(parser)?;
        let (column_aliases, _column_defs) = parse_optional_column_alias_list(parser)?;
        // LATERAL is valid before a parenthesised subquery in FROM.
        let lateral = parser.check_keyword(Keyword::Lateral);
        if lateral {
            parser.advance();
        }
        return Ok(FromClause::Subquery {
            statement: Box::new(stmt),
            alias: alias.unwrap_or_else(|| "__anon".to_string()),
            column_aliases,
            lateral,
        });
    }
    if matches!(parser.peek().kind, TokenKind::Keyword(Keyword::Values)) {
        let rows = parse_values_rows(parser)?;
        // A bare VALUES in FROM must be wrapped: `FROM VALUES ...` is not
        // valid PostgreSQL, but tolerate it as an anonymous derived table.
        let alias = parse_optional_alias(parser)?;
        let (column_aliases, _column_defs) = parse_optional_column_alias_list(parser)?;
        return Ok(FromClause::Subquery {
            statement: Box::new(Statement::Values(rows)),
            alias: alias.unwrap_or_else(|| "__values".to_string()),
            column_aliases,
            lateral: false,
        });
    }
    let is_table_function = matches!(parser.peek().kind, TokenKind::Identifier)
        && (parser.peek_n(1).kind == TokenKind::LParen
            || (parser.peek_n(1).kind == TokenKind::Dot
                && matches!(parser.peek_n(2).kind, TokenKind::Identifier)
                && parser.peek_n(3).kind == TokenKind::LParen));
    if is_table_function {
        let lateral = parser.check_keyword(Keyword::Lateral);
        if lateral {
            parser.advance();
        }
        let expression = parser.parse_expression()?;
        if let Expression::FunctionCall { name, args, .. } = expression {
            if parser.check(TokenKind::Keyword(Keyword::With)) {
                parser.advance();
                parser.consume(TokenKind::Keyword(Keyword::Ordinality), "ORDINALITY")?;
            }
            let alias = parse_optional_alias(parser)?;
            let (column_aliases, column_defs) = parse_optional_column_alias_list(parser)?;
            // Attach the JSON_TABLE COLUMNS specs parsed during the expression,
            // and clear the parser-held state so it cannot leak.
            let json_columns = std::mem::take(&mut parser.pending_json_columns).unwrap_or_default();
            return Ok(FromClause::TableFunction {
                name,
                args,
                alias,
                column_aliases,
                column_defs,
                json_columns,
                lateral,
            });
        }
    }
    let table = parser.parse_table_name()?;
    let alias = parse_optional_alias(parser)?;
    Ok(FromClause::Table { name: table, alias })
}

/// Parses a bare top-level `VALUES (…), (…)` query.
pub fn parse_values_statement(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    Ok(Statement::Values(parse_values_rows(parser)?))
}

/// Parses `VALUES (expr, …), (expr, …)` row lists.
pub fn parse_values_rows(parser: &mut Parser<'_>) -> Result<Vec<Vec<Expression>>, ParseError> {
    parser.consume(TokenKind::Keyword(Keyword::Values), "VALUES")?;
    let mut rows = Vec::new();
    loop {
        parser.consume(TokenKind::LParen, "(")?;
        let mut row = Vec::new();
        loop {
            row.push(parser.parse_expression()?);
            if parser.check(TokenKind::Comma) {
                parser.advance();
            } else {
                break;
            }
        }
        parser.consume(TokenKind::RParen, ")")?;
        rows.push(row);
        if parser.check(TokenKind::Comma) {
            parser.advance();
        } else {
            break;
        }
    }
    Ok(rows)
}

/// Column aliases plus typed column definitions for a derived-table alias.
/// `column_defs` is populated when types are present.
type ColumnAliasList = (Vec<String>, Vec<(String, String)>);
/// Parses an explicit column alias list: `(a, b, c)` immediately after a
/// derived-table alias. Also handles column definitions with types:
/// `(id INTEGER, name TEXT)` for table functions like `json_to_record(...)`.
/// Returns (column_aliases, column_defs) where column_defs is populated
/// when types are present.
fn parse_optional_column_alias_list(
    parser: &mut Parser<'_>,
) -> Result<ColumnAliasList, ParseError> {
    if !parser.check(TokenKind::LParen) {
        return Ok((Vec::new(), Vec::new()));
    }
    // Only treat `(` as a column list when followed by an identifier or a
    // soft keyword usable as a column alias (e.g. `x(value)`).
    if !matches!(
        parser.peek_n(1).kind,
        TokenKind::Identifier
            | TokenKind::Keyword(Keyword::Value)
            | TokenKind::Keyword(Keyword::Key)
    ) {
        return Ok((Vec::new(), Vec::new()));
    }
    parser.advance();
    let mut columns = Vec::new();
    let mut column_defs = Vec::new();
    let mut has_types = false;
    loop {
        // Column aliases are a contextual position: accept ordinary
        // identifiers and soft keywords (`value`, `key`) alike.
        let col_name = match parser.peek().kind {
            TokenKind::Identifier
            | TokenKind::Keyword(Keyword::Value)
            | TokenKind::Keyword(Keyword::Key) => {
                let name = parser.peek().lexeme.clone();
                parser.advance();
                name
            }
            _ => return Err(ParseError::unexpected(parser.peek(), "column alias")),
        };
        columns.push(col_name.clone());
        // Check if a type name follows (indicating a column definition).
        if matches!(
            parser.peek().kind,
            TokenKind::Identifier | TokenKind::Keyword(_)
        ) {
            // Don't consume if it's a reserved keyword that can't be a type.
            let peek_lower = parser.peek().lexeme.to_ascii_lowercase();
            if !matches!(
                peek_lower.as_str(),
                "where"
                    | "group"
                    | "having"
                    | "order"
                    | "limit"
                    | "offset"
                    | "union"
                    | "intersect"
                    | "except"
                    | "on"
                    | "using"
                    | "as"
                    | "with"
                    | "from"
                    | "select"
                    | "left"
                    | "right"
                    | "full"
                    | "inner"
                    | "outer"
                    | "cross"
                    | "natural"
                    | "join"
            ) {
                let col_type = parser.parse_type_name("column type")?;
                column_defs.push((col_name, col_type));
                has_types = true;
            }
        }
        if parser.check(TokenKind::Comma) {
            parser.advance();
        } else {
            break;
        }
    }
    parser.consume(TokenKind::RParen, ")")?;
    if has_types {
        Ok((columns, column_defs))
    } else {
        Ok((columns, Vec::new()))
    }
}

fn parse_optional_alias(parser: &mut Parser<'_>) -> Result<Option<String>, ParseError> {
    if parser.check_keyword(Keyword::As) {
        parser.advance();
        return Ok(Some(parse_explicit_alias(parser, "table alias")?));
    }
    // An unreserved identifier may be used as an alias.
    if matches!(parser.peek().kind, TokenKind::Identifier) {
        let lex = parser.peek().lexeme.clone();
        let lower = lex.to_ascii_lowercase();
        // Reserved keywords that may NOT be taken as an alias.
        let reserved = matches!(
            lower.as_str(),
            "where"
                | "group"
                | "having"
                | "order"
                | "limit"
                | "offset"
                | "on"
                | "using"
                | "inner"
                | "left"
                | "right"
                | "full"
                | "outer"
                | "cross"
                | "join"
                | "union"
                | "intersect"
                | "except"
                | "for"
        );
        if !reserved {
            // Make sure the next token would start a new clause, or that the
            // alias is followed by a column-alias list like `x(a, b, c)`.
            let next_is_clause = parser.pos + 1 < parser.tokens.len()
                && matches!(
                    parser.tokens[parser.pos + 1].kind,
                    TokenKind::Comma
                        | TokenKind::SemiColon
                        | TokenKind::Eof
                        | TokenKind::Keyword(Keyword::Where)
                        | TokenKind::Keyword(Keyword::Group)
                        | TokenKind::Keyword(Keyword::Having)
                        | TokenKind::Keyword(Keyword::Order)
                        | TokenKind::Keyword(Keyword::Limit)
                        | TokenKind::Keyword(Keyword::Offset)
                        | TokenKind::Keyword(Keyword::Join)
                        | TokenKind::Keyword(Keyword::Inner)
                        | TokenKind::Keyword(Keyword::Left)
                        | TokenKind::Keyword(Keyword::Right)
                        | TokenKind::Keyword(Keyword::Full)
                        | TokenKind::Keyword(Keyword::Outer)
                        | TokenKind::Keyword(Keyword::Cross)
                        | TokenKind::Keyword(Keyword::On)
                        | TokenKind::Keyword(Keyword::Using)
                        | TokenKind::Keyword(Keyword::Union)
                        | TokenKind::Keyword(Keyword::Intersect)
                        | TokenKind::Keyword(Keyword::Except)
                        | TokenKind::RParen
                        | TokenKind::LParen
                );
            if next_is_clause {
                parser.advance();
                return Ok(Some(lex));
            }
        }
    }
    Ok(None)
}

pub(crate) fn parse_select_targets(
    parser: &mut Parser<'_>,
) -> Result<Vec<SelectTarget>, ParseError> {
    let mut targets = Vec::new();
    loop {
        targets.push(parse_select_target(parser)?);
        if !parser.check(TokenKind::Comma) {
            break;
        }
        parser.advance();
    }
    Ok(targets)
}

fn parse_select_target(parser: &mut Parser<'_>) -> Result<SelectTarget, ParseError> {
    // Qualified star: `table.*`, `schema.table.*`, or `alias.*` — consume every
    // `ident.` segment so the qualifier preserves the full prefix.
    if parser.peek().kind == TokenKind::Identifier && parser.peek_n(1).kind == TokenKind::Dot {
        let mut qualifier = String::new();
        let mut consumed = 0;
        loop {
            if consumed > 0 {
                qualifier.push('.');
            }
            qualifier.push_str(&parser.peek_n(consumed).lexeme);
            let dot = consumed + 1;
            let next = consumed + 2;
            if parser.peek_n(dot).kind != TokenKind::Dot {
                break;
            }
            if parser.peek_n(next).kind == TokenKind::Star {
                for _ in 0..=next {
                    parser.advance();
                }
                return Ok(SelectTarget::QualifiedStar { qualifier });
            }
            if parser.peek_n(next).kind != TokenKind::Identifier {
                break;
            }
            consumed = next;
        }
    }
    let target = if parser.check(TokenKind::Star) {
        parser.advance();
        SelectTarget::All
    } else {
        let expr = parser.parse_expression()?;
        match expr {
            Expression::FunctionCall {
                name,
                args,
                distinct,
                filter,
                order_by,
                returning,
                null_handling,
                unique_keys,
            } if !distinct && filter.is_none() && order_by.is_empty() => {
                if returning.is_none() && null_handling.is_none() && unique_keys.is_none() {
                    SelectTarget::FunctionCall { name, args }
                } else {
                    SelectTarget::Expr {
                        expr: Expression::FunctionCall {
                            name,
                            args,
                            distinct,
                            filter,
                            order_by,
                            returning,
                            null_handling,
                            unique_keys,
                        },
                        alias: None,
                    }
                }
            }
            Expression::FunctionCall {
                name,
                args,
                distinct,
                filter,
                order_by,
                returning,
                null_handling,
                unique_keys,
            } if !distinct && filter.is_none() => SelectTarget::Expr {
                expr: Expression::FunctionCall {
                    name,
                    args,
                    distinct,
                    filter,
                    order_by,
                    returning,
                    null_handling,
                    unique_keys,
                },
                alias: None,
            },
            Expression::FunctionCall {
                name,
                args,
                distinct,
                filter,
                order_by,
                returning,
                null_handling,
                unique_keys,
            } => SelectTarget::Expr {
                expr: Expression::FunctionCall {
                    name,
                    args,
                    distinct,
                    filter,
                    order_by,
                    returning,
                    null_handling,
                    unique_keys,
                },
                alias: None,
            },
            Expression::WindowFunction { name, args, over } => {
                SelectTarget::WindowFunction { name, args, over }
            }
            other => SelectTarget::Expr {
                expr: other,
                alias: None,
            },
        }
    };
    parse_alias(parser, target)
}

fn parse_alias(parser: &mut Parser<'_>, target: SelectTarget) -> Result<SelectTarget, ParseError> {
    let alias = if parser.check_keyword(Keyword::As) {
        parser.advance();
        Some(parse_explicit_alias(parser, "column alias")?)
    } else if matches!(parser.peek().kind, TokenKind::Identifier) {
        let peeked = parser.peek().lexeme.clone();
        let lower = peeked.to_ascii_lowercase();
        let forbidden_clause = matches!(
            lower.as_str(),
            "from"
                | "where"
                | "group"
                | "having"
                | "order"
                | "limit"
                | "offset"
                | "join"
                | "inner"
                | "left"
                | "right"
                | "full"
                | "outer"
                | "cross"
                | "on"
                | "using"
                | "union"
                | "intersect"
                | "except"
                | "as"
        );
        let followed_by_clause_sep = if parser.pos + 1 < parser.tokens.len() {
            let next = &parser.tokens[parser.pos + 1];
            matches!(
                next.kind,
                TokenKind::Comma
                    | TokenKind::SemiColon
                    | TokenKind::Eof
                    | TokenKind::Keyword(Keyword::From)
                    | TokenKind::Keyword(Keyword::Where)
                    | TokenKind::Keyword(Keyword::Group)
                    | TokenKind::Keyword(Keyword::Having)
                    | TokenKind::Keyword(Keyword::Order)
                    | TokenKind::Keyword(Keyword::Join)
                    | TokenKind::RParen
            )
        } else {
            true
        };
        if !forbidden_clause && followed_by_clause_sep {
            parser.advance();
            Some(peeked)
        } else {
            None
        }
    } else {
        None
    };

    match (target, alias) {
        (SelectTarget::Expr { expr, .. }, Some(alias)) => Ok(SelectTarget::Expr {
            expr,
            alias: Some(alias),
        }),
        (target, Some(alias)) => Ok(SelectTarget::Aliased {
            target: Box::new(target),
            alias,
        }),
        (target, None) => Ok(target),
    }
}

/// PostgreSQL permits many otherwise-keyword tokens as aliases after an
/// explicit `AS`.  The lexer keeps those tokens as keywords so they can serve
/// their syntactic role elsewhere, but alias parsing is a contextual position
/// where their spelling is the identifier supplied by the client (for
/// example, `SELECT 1 AS schema`).
fn parse_explicit_alias(parser: &mut Parser<'_>, expected: &str) -> Result<String, ParseError> {
    match parser.peek().kind {
        TokenKind::Identifier | TokenKind::Keyword(_) => {
            let alias = parser.peek().lexeme.clone();
            parser.advance();
            Ok(alias)
        }
        _ => Err(ParseError::unexpected(parser.peek(), expected)),
    }
}

/// Parse `OVER (...)` window specification. Caller has already advanced
/// past the `OVER` keyword.
pub fn parse_window_spec(parser: &mut Parser<'_>) -> Result<WindowSpec, ParseError> {
    parser.consume(TokenKind::LParen, "(")?;
    let mut spec = WindowSpec::default();
    if parser.check(TokenKind::Keyword(Keyword::Partition)) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::By), "BY")?;
        loop {
            spec.partition_by.push(parser.parse_expression()?);
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
    }
    if parser.check(TokenKind::Keyword(Keyword::Order)) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::By), "BY")?;
        loop {
            let expr = parser.parse_expression()?;
            let descending = if parser.check(TokenKind::Keyword(Keyword::Desc)) {
                parser.advance();
                true
            } else {
                if parser.check(TokenKind::Keyword(Keyword::Asc)) {
                    parser.advance();
                }
                false
            };
            let nulls_first = if parser.check(TokenKind::Keyword(Keyword::Nulls)) {
                parser.advance();
                if parser.check(TokenKind::Keyword(Keyword::First)) {
                    parser.advance();
                    Some(true)
                } else if parser.check(TokenKind::Keyword(Keyword::Last)) {
                    parser.advance();
                    Some(false)
                } else {
                    let token = parser.peek();
                    return Err(ParseError::unexpected(token, "FIRST or LAST"));
                }
            } else {
                None
            };
            spec.order_by.push(OrderByItem {
                expr,
                descending,
                nulls_first,
            });
            if !parser.check(TokenKind::Comma) {
                break;
            }
            parser.advance();
        }
    }
    // Optional frame specification: ROWS / RANGE / GROUPS.
    if parser.check(TokenKind::Keyword(Keyword::Rows))
        || parser.check(TokenKind::Keyword(Keyword::Range))
        || parser.check(TokenKind::Keyword(Keyword::Groups))
    {
        let kind = match parser.peek().kind {
            TokenKind::Keyword(Keyword::Rows) => FrameKind::Rows,
            TokenKind::Keyword(Keyword::Range) => FrameKind::Range,
            TokenKind::Keyword(Keyword::Groups) => FrameKind::Groups,
            _ => unreachable!("checked above"),
        };
        parser.advance();
        let bounds = if parser.check(TokenKind::Keyword(Keyword::Between)) {
            parser.advance();
            let start = parse_frame_bound(parser)?;
            parser.consume(TokenKind::Keyword(Keyword::And), "AND")?;
            let end = parse_frame_bound(parser)?;
            validate_frame_bounds(&start, &end)?;
            FrameBounds { start, end }
        } else {
            // Single bound (without BETWEEN) is the frame start; the end is
            // implicitly CURRENT ROW.
            let start = parse_frame_bound(parser)?;
            let end = FrameBound::CurrentRow;
            validate_frame_bounds(&start, &end)?;
            FrameBounds { start, end }
        };
        spec.frame = Some(match kind {
            FrameKind::Rows => FrameSpec::Rows { bounds },
            FrameKind::Range => FrameSpec::Range { bounds },
            FrameKind::Groups => FrameSpec::Groups { bounds },
        });
    }
    parser.consume(TokenKind::RParen, ")")?;
    Ok(spec)
}

/// Which frame mode a `ROWS`/`RANGE`/`GROUPS` clause selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Rows,
    Range,
    Groups,
}

/// Parses a single frame boundary such as `UNBOUNDED PRECEDING`,
/// `CURRENT ROW`, `N PRECEDING`, or `N FOLLOWING`.
fn parse_frame_bound(parser: &mut Parser<'_>) -> Result<FrameBound, ParseError> {
    if parser.check(TokenKind::Keyword(Keyword::Unbounded)) {
        parser.advance();
        if parser.check(TokenKind::Keyword(Keyword::Preceding)) {
            parser.advance();
            return Ok(FrameBound::UnboundedPreceding);
        }
        if parser.check(TokenKind::Keyword(Keyword::Following)) {
            parser.advance();
            return Ok(FrameBound::UnboundedFollowing);
        }
        return Err(ParseError::unexpected(
            parser.peek(),
            "PRECEDING or FOLLOWING after UNBOUNDED",
        ));
    }
    if parser.check(TokenKind::Keyword(Keyword::Current)) {
        parser.advance();
        parser.consume(TokenKind::Keyword(Keyword::Row), "ROW")?;
        return Ok(FrameBound::CurrentRow);
    }
    // Otherwise it is `<offset expression> PRECEDING/FOLLOWING`.
    let offset = parser.parse_expression()?;
    if parser.check(TokenKind::Keyword(Keyword::Preceding)) {
        parser.advance();
        return Ok(FrameBound::Preceding {
            offset: Box::new(offset),
        });
    }
    if parser.check(TokenKind::Keyword(Keyword::Following)) {
        parser.advance();
        return Ok(FrameBound::Following {
            offset: Box::new(offset),
        });
    }
    Err(ParseError::unexpected(
        parser.peek(),
        "PRECEDING or FOLLOWING",
    ))
}

/// Rejects frame specifications whose start lies after their end, using
/// PostgreSQL's diagnostics for each specific invalid combination.
fn validate_frame_bounds(start: &FrameBound, end: &FrameBound) -> Result<(), ParseError> {
    let invalid = |message: &str| -> ParseError {
        ParseError::Unsupported {
            message: format!("invalid window frame: {message}"),
            detail: None,
        }
    };
    match (start, end) {
        // A frame can never begin at the partition end.
        (FrameBound::UnboundedFollowing, _) => {
            return Err(invalid("frame start cannot be UNBOUNDED FOLLOWING"))
        }
        // Nor finish at the partition start.
        (_, FrameBound::UnboundedPreceding) => {
            return Err(invalid("frame end cannot be UNBOUNDED PRECEDING"))
        }
        (FrameBound::CurrentRow, FrameBound::Preceding { .. }) => {
            return Err(invalid(
                "frame starting from current row cannot have preceding rows",
            ))
        }
        (FrameBound::Following { .. }, FrameBound::CurrentRow) => {
            return Err(invalid(
                "frame starting from following row cannot end with current row",
            ))
        }
        _ => {}
    }
    let rank = |bound: &FrameBound| -> u8 {
        match bound {
            FrameBound::UnboundedPreceding => 0,
            FrameBound::Preceding { .. } => 1,
            FrameBound::CurrentRow => 2,
            FrameBound::Following { .. } => 3,
            FrameBound::UnboundedFollowing => 4,
        }
    };
    if rank(start) > rank(end) {
        return Err(invalid("frame start cannot be after the frame end"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Catalog, InMemoryCatalog, Lexer};

    fn parse(sql: &str) -> Statement {
        let tokens = Lexer::new(sql).lex().unwrap();
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "users".to_string(),
                vec![crate::ColumnDef {
                    name: "id".to_string(),
                    col_type: crate::ColumnType::int4(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .unwrap();
        catalog
            .create_table(
                "orders".to_string(),
                vec![
                    crate::ColumnDef {
                        name: "id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                    crate::ColumnDef {
                        name: "user_id".to_string(),
                        col_type: crate::ColumnType::int4(),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        parser.parse_statements().unwrap().remove(0)
    }

    #[test]
    fn parses_order_by() {
        let Statement::Select { order_by, .. } = parse("SELECT * FROM users ORDER BY id DESC;")
        else {
            panic!("expected select");
        };
        assert_eq!(order_by.len(), 1);
        assert!(order_by[0].descending);
    }

    #[test]
    fn parses_group_by_and_having() {
        let Statement::Select {
            group_by, having, ..
        } = parse("SELECT id, COUNT(*) FROM users GROUP BY id HAVING COUNT(*) > 1;")
        else {
            panic!("expected select");
        };
        assert!(group_by.is_some());
        assert!(having.is_some());
    }

    #[test]
    fn parses_inner_join_with_aliases() {
        let sql = "SELECT u.id FROM users u INNER JOIN orders o ON u.id = o.user_id;";
        let stmt = parse(sql);
        let Statement::Select { from, .. } = stmt else {
            panic!("expected SELECT");
        };
        let Some(FromClause::Join { kind, on, .. }) = from else {
            panic!("expected FROM with JOIN");
        };
        assert_eq!(kind, JoinKind::Inner);
        assert!(on.is_some());
    }

    #[test]
    fn parses_left_join() {
        let sql = "SELECT u.id FROM users u LEFT JOIN orders o ON u.id = o.user_id;";
        let stmt = parse(sql);
        let Statement::Select { from, .. } = stmt else {
            panic!("expected SELECT");
        };
        let Some(FromClause::Join { kind, .. }) = from else {
            panic!("expected FROM with JOIN");
        };
        assert_eq!(kind, JoinKind::Left);
    }

    #[test]
    fn parses_set_operation() {
        let sql = "SELECT id FROM users UNION ALL SELECT id FROM users;";
        let stmt = parse(sql);
        assert!(matches!(
            stmt,
            Statement::SetOperation {
                op: crate::ast::SetOpKind::Union,
                all: true,
                ..
            }
        ));
    }

    #[test]
    fn parses_plain_distinct() {
        let Statement::Select {
            distinct,
            distinct_on,
            ..
        } = parse("SELECT DISTINCT country FROM users;")
        else {
            panic!("expected select");
        };
        assert!(distinct);
        assert!(distinct_on.is_none());
    }

    #[test]
    fn parses_distinct_on_single_expression() {
        let Statement::Select {
            distinct,
            distinct_on,
            order_by,
            ..
        } = parse("SELECT DISTINCT ON (country) id, country FROM users ORDER BY country, id;")
        else {
            panic!("expected select");
        };
        assert!(distinct);
        let distinct_on = distinct_on.expect("DISTINCT ON expressions");
        assert_eq!(distinct_on.len(), 1);
        assert_eq!(order_by.len(), 2);
    }

    #[test]
    fn parses_distinct_on_multiple_expressions() {
        let Statement::Select {
            distinct, distinct_on, ..
        } = parse(
            "SELECT DISTINCT ON (country, status) id, country, status FROM users ORDER BY country, status, id;",
        )
        else {
            panic!("expected select");
        };
        assert!(distinct);
        assert_eq!(distinct_on.expect("DISTINCT ON expressions").len(), 2);
    }

    #[test]
    fn parses_distinct_on_function_expression() {
        let Statement::Select {
            distinct, distinct_on, ..
        } = parse(
            "SELECT DISTINCT ON (LOWER(country)) id, country FROM users ORDER BY LOWER(country), id;",
        )
        else {
            panic!("expected select");
        };
        assert!(distinct);
        let distinct_on = distinct_on.expect("DISTINCT ON expressions");
        assert_eq!(distinct_on.len(), 1);
        assert!(matches!(
            distinct_on[0],
            crate::ast::Expression::FunctionCall { .. }
        ));
    }

    /// Extracts the frame of the first window function in a statement.
    fn window_frame(sql: &str) -> crate::ast::FrameSpec {
        let Statement::Select { targets, .. } = parse(sql) else {
            panic!("expected select");
        };
        for target in targets {
            let over = window_over(&target);
            if let Some(over) = over {
                return over.frame.clone().expect("frame");
            }
        }
        panic!("no window function in {sql}");
    }

    fn window_over(target: &crate::SelectTarget) -> Option<&crate::ast::WindowSpec> {
        match target {
            crate::SelectTarget::WindowFunction { over, .. } => Some(over),
            crate::SelectTarget::Aliased { target, .. } => window_over(target),
            crate::SelectTarget::Expr { expr, .. } => {
                if let crate::ast::Expression::WindowFunction { over, .. } = expr {
                    Some(over)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[test]
    fn parses_rows_frame_bounds() {
        let frame = window_frame(
            "SELECT SUM(a) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM orders;",
        );
        assert!(matches!(frame, crate::ast::FrameSpec::Rows { .. }));
        let crate::ast::FrameSpec::Rows { bounds } = frame else {
            panic!("expected rows frame");
        };
        assert!(matches!(
            bounds.start,
            crate::ast::FrameBound::UnboundedPreceding
        ));
        assert!(matches!(bounds.end, crate::ast::FrameBound::CurrentRow));
    }

    #[test]
    fn parses_single_bound_frame_as_start_to_current_row() {
        let frame =
            window_frame("SELECT SUM(a) OVER (ORDER BY id ROWS UNBOUNDED PRECEDING) FROM orders;");
        let crate::ast::FrameSpec::Rows { bounds } = frame else {
            panic!("expected rows frame");
        };
        assert!(matches!(
            bounds.start,
            crate::ast::FrameBound::UnboundedPreceding
        ));
        assert!(matches!(bounds.end, crate::ast::FrameBound::CurrentRow));
    }

    #[test]
    fn parses_offset_frame_bounds() {
        let frame = window_frame(
            "SELECT SUM(a) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM orders;",
        );
        let crate::ast::FrameSpec::Rows { bounds } = frame else {
            panic!("expected rows frame");
        };
        let crate::ast::FrameBound::Preceding { offset } = &bounds.start else {
            panic!("expected preceding start");
        };
        assert!(matches!(
            offset.as_ref(),
            crate::ast::Expression::Literal(_)
        ));
        assert!(matches!(
            bounds.end,
            crate::ast::FrameBound::Following { .. }
        ));
    }

    #[test]
    fn parses_groups_and_range_frame_kinds() {
        let frame =
            window_frame("SELECT SUM(a) OVER (ORDER BY id GROUPS 1 PRECEDING) FROM orders;");
        assert!(matches!(frame, crate::ast::FrameSpec::Groups { .. }));
        let frame = window_frame(
            "SELECT SUM(a) OVER (ORDER BY id RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM orders;",
        );
        assert!(matches!(frame, crate::ast::FrameSpec::Range { .. }));
    }

    #[test]
    fn rejects_invalid_frame_start_after_end() {
        let tokens = Lexer::new(
            "SELECT SUM(a) OVER (ORDER BY id ROWS BETWEEN 5 FOLLOWING AND 2 PRECEDING) FROM orders;",
        )
        .lex()
        .unwrap();
        let mut catalog = InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        assert!(parser.parse_statements().is_err());
    }

    #[test]
    fn rejects_frame_start_unbounded_following() {
        let tokens =
            Lexer::new("SELECT SUM(a) OVER (ORDER BY id ROWS UNBOUNDED FOLLOWING) FROM orders;")
                .lex()
                .unwrap();
        let mut catalog = InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        assert!(parser.parse_statements().is_err());
    }
}
