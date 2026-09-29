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
pub mod alter;
pub mod comment;
pub mod create;
pub mod delete;
pub mod drop;
pub mod expressions;
pub mod grant;
pub mod insert;
pub mod json;
pub mod select;
pub mod set;
pub mod show;
pub mod types;
pub mod update;
pub mod use_;

use crate::{
    ast::{Statement, Value},
    lexer::{Keyword, LexError, Token, TokenKind},
};

#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    UnexpectedToken {
        expected: String,
        found: String,
        line: usize,
        column: usize,
    },
    LexError(LexError),
    DuplicateTable(String),
    DuplicateColumn(String),
    UnknownTable(String),
    Unsupported {
        message: String,
        detail: Option<String>,
    },
}

impl ParseError {
    pub fn unexpected(token: &Token, expected: &str) -> Self {
        Self::UnexpectedToken {
            expected: expected.to_string(),
            found: token.lexeme.clone(),
            line: token.line as usize,
            column: token.column as usize,
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedToken {
                expected,
                found,
                line,
                column,
            } => write!(
                f,
                "syntax error at line {line}, column {column}: expected {expected}, found {found}"
            ),
            Self::LexError(err) => write!(f, "lexer error at line {}: {}", err.line, err.message),
            Self::DuplicateTable(name) => write!(f, "table \"{name}\" already exists"),
            Self::DuplicateColumn(name) => write!(f, "duplicate column: {name}"),
            Self::UnknownTable(name) => write!(f, "table \"{name}\" does not exist"),
            Self::Unsupported { message, detail } => {
                if let Some(detail) = detail {
                    write!(f, "{message}: {detail}")
                } else {
                    write!(f, "{message}")
                }
            }
        }
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(err: LexError) -> Self {
        Self::LexError(err)
    }
}

pub struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    catalog: &'a mut dyn crate::Catalog,
    /// Names of CTEs visible in the current statement scope. `parse_with`
    /// populates this so `FROM cte_name` parses even though the name is not a
    /// real catalog table; the executor later inlines the rewritten query.
    cte_names: Vec<String>,
    /// JSON_TABLE `COLUMNS (...)` spec left by parsing a `JSON_TABLE(...)`
    /// function call inside an expression. `parse_from_atom` reads this once
    /// it recognises the call as a `FromClause::TableFunction`.
    pending_json_columns: Option<Vec<crate::JsonTableColumn>>,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: Vec<Token>, catalog: &'a mut dyn crate::Catalog) -> Self {
        Self {
            tokens,
            pos: 0,
            catalog,
            cte_names: Vec::new(),
            pending_json_columns: None,
        }
    }

    pub fn catalog(&mut self) -> &mut dyn crate::Catalog {
        self.catalog
    }

    pub fn parse_statements(mut self) -> Result<Vec<Statement>, ParseError> {
        tracing::trace!(target: "sql::parser", "parse_start token_count={}", self.tokens.len());
        let mut statements = Vec::new();
        while !self.check(TokenKind::Eof) {
            let stmt = self.parse_statement()?;
            tracing::trace!(target: "sql::parser", "parsed_statement kind={:?}", std::mem::discriminant(&stmt));
            statements.push(stmt);
            if self.check(TokenKind::SemiColon) {
                self.advance();
            } else if !self.check(TokenKind::Eof) {
                let token = self.peek().clone();
                return Err(ParseError::unexpected(&token, "; or end of input"));
            }
        }
        tracing::trace!(target: "sql::parser", "parse_complete statement_count={}", statements.len());
        Ok(statements)
    }

    fn parse_statement(&mut self) -> Result<Statement, ParseError> {
        // Reset JSON_TABLE column state between statements so a previous
        // statement's `pending_json_columns` cannot leak into the next one.
        self.pending_json_columns = None;
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Keyword(Keyword::Create) => create::parse_create(self),
            TokenKind::Keyword(Keyword::Insert) => insert::parse_insert(self),
            TokenKind::Keyword(Keyword::Select) => select::parse_select(self),
            TokenKind::Keyword(Keyword::Values) => select::parse_values_statement(self),
            TokenKind::Keyword(Keyword::Update) => update::parse_update(self),
            TokenKind::Keyword(Keyword::Delete) => delete::parse_delete(self),
            TokenKind::Keyword(Keyword::Set) => set::parse_set(self),
            TokenKind::Keyword(Keyword::Show) => show::parse_show(self),
            TokenKind::Keyword(Keyword::Describe | Keyword::Desc) => {
                self.advance();
                Ok(Statement::Describe {
                    name: self.parse_table_name()?,
                })
            }
            TokenKind::Keyword(Keyword::Use) => use_::parse_use(self),
            TokenKind::Keyword(Keyword::Drop) => drop::parse_drop(self),
            TokenKind::Keyword(Keyword::Alter) => alter::parse_alter(self),
            TokenKind::Keyword(Keyword::Grant) => grant::parse_grant(self),
            TokenKind::Keyword(Keyword::Begin) => {
                self.advance();
                consume_transaction_options(self);
                Ok(Statement::Begin)
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("start") => {
                self.advance();
                let next = self.peek().clone();
                if matches!(next.kind, TokenKind::Identifier)
                    && next.lexeme.eq_ignore_ascii_case("transaction")
                {
                    self.advance();
                }
                consume_transaction_options(self);
                Ok(Statement::Begin)
            }
            TokenKind::Keyword(Keyword::With) => select::parse_with(self),
            TokenKind::Keyword(Keyword::Explain) => select::parse_explain(self),
            TokenKind::Keyword(Keyword::Copy) => insert::parse_copy(self),
            TokenKind::Keyword(Keyword::Commit) => {
                self.advance();
                consume_transaction_options(self);
                Ok(Statement::Commit)
            }
            TokenKind::Keyword(Keyword::Rollback) => {
                self.advance();
                if is_txn_opt_word(self, "to") {
                    self.advance();
                    if is_txn_opt_word(self, "savepoint") {
                        self.advance();
                    }
                    if matches!(self.peek().kind, TokenKind::Identifier) {
                        self.advance();
                    }
                }
                consume_transaction_options(self);
                Ok(Statement::Rollback)
            }
            TokenKind::Keyword(Keyword::Comment) => comment::parse_comment(self),
            TokenKind::Keyword(Keyword::Analyze) => {
                consume_housekeeping_tail(self);
                Ok(Statement::Analyze { table: None })
            }
            TokenKind::Keyword(Keyword::Vacuum) => {
                self.advance();
                vacuum_table(self)
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("vacuum") => {
                self.advance();
                vacuum_table(self)
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("reindex") => {
                self.advance();
                consume_housekeeping_tail(self);
                Ok(Statement::Reindex)
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("lock") => {
                self.advance();
                consume_housekeeping_tail(self);
                Ok(Statement::Lock)
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("cluster") => {
                self.advance();
                consume_housekeeping_tail(self);
                Ok(Statement::Cluster)
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("refresh") => {
                self.advance();
                if is_txn_opt_word(self, "materialized") {
                    self.advance();
                }
                if is_txn_opt_word(self, "view") {
                    self.advance();
                }
                consume_housekeeping_tail(self);
                Ok(Statement::RefreshMaterializedView {
                    name: String::new(),
                })
            }
            TokenKind::Identifier if self.peek().lexeme.eq_ignore_ascii_case("truncate") => {
                self.advance();
                // TRUNCATE empties the named table(s) of every row, so the
                // table names must reach the executor. Parse one or more
                // comma-separated names (PostgreSQL `TRUNCATE [TABLE] name
                // [, ...]`) before swallowing any trailing options
                // (RESTART IDENTITY / CASCADE / RESTRICT / ONLY ...).
                let mut tables: Vec<String> = Vec::new();
                loop {
                    tables.push(self.parse_table_name()?);
                    if self.check(TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                consume_housekeeping_tail(self);
                Ok(Statement::Truncate { tables })
            }
            TokenKind::Keyword(Keyword::Do) => {
                self.advance();
                // DO $$ body $$ — consume the dollar-quoted body as a raw string.
                // PLOMID V1 only supports the subset required by compatibility
                // tests: PERFORM, RAISE NOTICE, and EXCEPTION handlers. The body
                // is preserved as text and executed by the executor.
                let token = self.peek().clone();
                if token.kind == TokenKind::StringLiteral {
                    self.advance();
                    Ok(Statement::Do { body: token.lexeme })
                } else {
                    Err(ParseError::unexpected(&token, "dollar-quoted string body"))
                }
            }
            _ => Err(ParseError::unexpected(
                &token,
                "SQL statement (CREATE, INSERT, SELECT, UPDATE, DELETE, BEGIN, COMMIT, ROLLBACK, SET, SHOW, USE)",
            )),
        }
    }

    pub fn check(&self, kind: TokenKind) -> bool {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].kind == kind
        } else {
            false
        }
    }

    pub fn check_keyword(&self, keyword: Keyword) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Keyword(kind) if kind == keyword
        )
    }

    pub fn peek(&self) -> &Token {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos]
        } else {
            &self.tokens[self.tokens.len() - 1]
        }
    }

    pub fn peek_n(&self, offset: usize) -> &Token {
        &self.tokens[(self.pos + offset).min(self.tokens.len() - 1)]
    }

    pub fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    pub fn consume(&mut self, kind: TokenKind, expected: &str) -> Result<Token, ParseError> {
        if self.check(kind) {
            let token = self.tokens[self.pos].clone();
            self.pos += 1;
            Ok(token)
        } else {
            let token = self.peek().clone();
            Err(ParseError::unexpected(&token, expected))
        }
    }

    pub fn parse_qualified_identifier(&mut self, expected: &str) -> Result<String, ParseError> {
        // A small set of soft keywords that PostgreSQL allows as bare
        // identifiers/column names (`type`, `left`, `right`, ...).
        fn is_soft_identifier(kind: TokenKind) -> bool {
            matches!(
                kind,
                TokenKind::Identifier
                    | TokenKind::Keyword(
                        Keyword::Type
                            | Keyword::Replace
                            | Keyword::Left
                            | Keyword::Right
                            | Keyword::Default
                            | Keyword::Value
                            | Keyword::Key
                            | Keyword::Conflict
                            | Keyword::Row
                            // `format` is a function/column name (FORMAT is
                            // only a non-reserved JSON-clause word), so it
                            // must stay usable as a bare identifier.
                            | Keyword::Format
                    )
            )
        }
        let first_token = self.peek().clone();
        let first = match first_token.kind {
            kind if is_soft_identifier(kind) => {
                self.advance();
                first_token.lexeme
            }
            _ => return Err(ParseError::unexpected(&first_token, expected)),
        };
        let mut name = first;
        while self.check(TokenKind::Dot) {
            self.advance();
            let part_token = self.peek().clone();
            if !is_soft_identifier(part_token.kind) {
                return Err(ParseError::unexpected(&part_token, expected));
            }
            self.advance();
            name.push('.');
            name.push_str(&part_token.lexeme);
        }
        Ok(name)
    }

    /// Parses a (possibly qualified) type name after `::`. Unlike ordinary
    /// identifiers, type names may be SQL keywords (`TEXT`, `INTEGER`, ...).
    pub fn parse_type_name(&mut self, expected: &str) -> Result<String, ParseError> {
        let first_token = self.peek().clone();
        let first = match first_token.kind {
            TokenKind::Identifier | TokenKind::Keyword(_) => {
                self.advance();
                first_token.lexeme
            }
            _ => return Err(ParseError::unexpected(&first_token, expected)),
        };
        let mut name = first;

        // PostgreSQL has several multi-word type names (e.g. `double
        // precision`, `character varying`, `timestamp with time zone`). The
        // type registry owns their aliases, but the parser must preserve the
        // full name so those aliases resolve consistently for casts and
        // expressions. The trailing words lex as identifiers or keywords
        // (`WITH`, `TIME`), so greedily match the known multi-word forms.
        let peek = |tokens: &[Token], pos: usize, offset: usize| -> Option<String> {
            tokens
                .get(pos + offset)
                .map(|t| t.lexeme.to_ascii_lowercase())
        };
        let first_lower = name.to_ascii_lowercase();
        let second = peek(&self.tokens, self.pos, 0);
        let third = peek(&self.tokens, self.pos, 1);
        let fourth = peek(&self.tokens, self.pos, 2);
        let words: Option<Vec<String>> = match (
            first_lower.as_str(),
            second.as_deref(),
            third.as_deref(),
            fourth.as_deref(),
        ) {
            ("character", Some("varying"), _, _) => Some(vec!["varying".to_string()]),
            ("double", Some("precision"), _, _) => Some(vec!["precision".to_string()]),
            ("bit", Some("varying"), _, _) => Some(vec!["varying".to_string()]),
            ("timestamp", Some(w), Some("time"), Some("zone")) if w == "with" || w == "without" => {
                Some(vec![w.to_string(), "time".to_string(), "zone".to_string()])
            }
            ("time", Some(w), Some("time"), Some("zone")) if w == "with" || w == "without" => {
                Some(vec![w.to_string(), "time".to_string(), "zone".to_string()])
            }
            _ => None,
        };
        if let Some(words) = words {
            for word in &words {
                name.push(' ');
                name.push_str(word);
            }
            for _ in 0..words.len() {
                self.advance();
            }
        }
        while self.check(TokenKind::Dot) {
            self.advance();
            let token = self.peek().clone();
            match token.kind {
                TokenKind::Identifier | TokenKind::Keyword(_) => {
                    self.advance();
                    name.push('.');
                    name.push_str(&token.lexeme);
                }
                _ => return Err(ParseError::unexpected(&token, expected)),
            }
        }
        while self.check(TokenKind::LBracket) && self.peek_n(1).kind == TokenKind::RBracket {
            self.advance();
            self.consume(TokenKind::RBracket, "]")?;
            name.push_str("[]");
        }
        if self.check(TokenKind::LParen) {
            self.advance();
            name.push('(');
            loop {
                let token = self.consume(TokenKind::IntegerLiteral, "type modifier")?;
                name.push_str(&token.lexeme);
                if self.check(TokenKind::Comma) {
                    self.advance();
                    name.push(',');
                } else {
                    break;
                }
            }
            self.consume(TokenKind::RParen, ")")?;
            name.push(')');
        }
        Ok(name)
    }

    pub fn parse_table_name(&mut self) -> Result<String, ParseError> {
        let qualified = self.parse_qualified_identifier("table name")?;
        let table = qualified
            .rsplit('.')
            .next()
            .ok_or_else(|| ParseError::Unsupported {
                message: "invalid table name".to_string(),
                detail: None,
            })?
            .to_string();
        // Any schema-qualified name (including `public.`) resolves directly to
        // that schema — never through the session search_path. This keeps
        // `public.customers` and `plomid_test.customers` distinct even when
        // `search_path` lists an earlier schema.
        if qualified.contains('.') {
            return Ok(qualified);
        }
        if self
            .cte_names
            .iter()
            .any(|cte| cte.eq_ignore_ascii_case(&table))
        {
            return Ok(table);
        }
        if Self::is_system_relation_name(&table) {
            return Ok(table);
        }
        // Resolve views and tables to their fully-qualified catalog identity.
        // Returning the qualified name (e.g. `plomid_torture.orders`) keeps row
        // storage, scans, DML and catalog operations schema-aware even when the
        // user wrote an unqualified name.
        if let Some(view) = self.catalog().get_view(&table) {
            return Ok(view.name.clone());
        }
        self.catalog()
            .get_table(&table)
            .map(|schema| schema.name.clone())
            .map_err(|_| ParseError::UnknownTable(table.clone()))
    }

    fn is_system_relation_name(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "pg_am"
                | "pg_database"
                | "pg_namespace"
                | "pg_collation"
                | "pg_class"
                | "pg_attribute"
                | "pg_attrdef"
                | "pg_type"
                | "pg_aggregate"
                | "pg_operator"
                | "pg_opclass"
                | "pg_amop"
                | "pg_amproc"
                | "pg_cast"
                | "pg_statistic"
                | "pg_conversion"
                | "pg_language"
                | "pg_enum"
                | "pg_index"
                | "pg_constraint"
                | "pg_description"
                | "pg_shdescription"
                | "pg_proc"
                | "pg_roles"
                | "pg_user"
                | "pg_group"
                | "pg_tablespace"
                | "pg_settings"
                | "pg_tables"
                | "pg_views"
                | "pg_indexes"
                | "pg_user_mapping"
                | "pg_extension"
                | "pg_replication_slots"
                | "pg_replication_origin_status"
                | "pg_stat_activity"
                | "pg_stat_database"
                | "pg_stat_user_tables"
                | "pg_stat_all_tables"
                | "pg_foreign_server"
                | "pg_foreign_data_wrapper"
                | "pg_auth_members"
                | "pg_depend"
                | "pg_shdepend"
                | "pg_trigger"
                | "pg_policy"
                | "pg_policies"
                | "pg_statistic_ext"
                | "pg_publication"
                | "pg_publication_rel"
                | "pg_inherits"
                | "pg_rewrite"
                | "pg_sequence"
                | "pg_sequences"
                | "pg_matviews"
                | "pg_show_all_settings"
                | "pg_default_acl"
        )
    }

    pub fn check_identifier(&self, expected: &str) -> bool {
        matches!(self.peek().kind, TokenKind::Identifier)
            && self.peek().lexeme.eq_ignore_ascii_case(expected)
    }

    pub fn parse_value(&mut self) -> Result<Value, ParseError> {
        expressions::parse_value(self)
    }

    pub fn parse_expression(&mut self) -> Result<crate::ast::Expression, ParseError> {
        expressions::parse_expression(self)
    }

    /// Parses a single unqualified identifier.
    pub fn parse_identifier(&mut self) -> Result<String, ParseError> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Identifier => {
                self.advance();
                Ok(token.lexeme)
            }
            _ => Err(ParseError::unexpected(&token, "identifier")),
        }
    }
}

fn is_txn_opt_word(parser: &Parser<'_>, word: &str) -> bool {
    matches!(
        parser.peek().kind,
        TokenKind::Identifier | TokenKind::Keyword(_)
    ) && parser.peek().lexeme.eq_ignore_ascii_case(word)
}

fn consume_transaction_options(parser: &mut Parser<'_>) {
    if is_txn_opt_word(parser, "transaction") || is_txn_opt_word(parser, "work") {
        parser.advance();
    }
    loop {
        if is_txn_opt_word(parser, "isolation") {
            parser.advance();
            if is_txn_opt_word(parser, "level") {
                parser.advance();
            }
            // Consume the isolation level: READ COMMITTED, READ UNCOMMITTED,
            // REPEATABLE READ, SERIALIZABLE, or DEFAULT.
            // These are multi-word identifiers that need to be consumed.
            match parser.peek().kind {
                TokenKind::Identifier | TokenKind::Keyword(_) => {
                    let word = parser.peek().lexeme.to_ascii_lowercase();
                    if word == "read"
                        || word == "repeatable"
                        || word == "serializable"
                        || word == "default"
                    {
                        parser.advance();
                        // Consume the second word if needed (e.g., "committed" in "read committed")
                        if is_txn_opt_word(parser, "committed")
                            || is_txn_opt_word(parser, "uncommitted")
                            || is_txn_opt_word(parser, "only")
                            || is_txn_opt_word(parser, "write")
                        {
                            parser.advance();
                        }
                    } else {
                        // Unknown isolation level, consume one token
                        parser.advance();
                    }
                }
                _ => break,
            }
        } else if is_txn_opt_word(parser, "read") {
            parser.advance();
            if is_txn_opt_word(parser, "write") || is_txn_opt_word(parser, "only") {
                parser.advance();
            }
        } else if is_txn_opt_word(parser, "not") {
            parser.advance();
            if is_txn_opt_word(parser, "deferrable") {
                parser.advance();
            }
        } else if is_txn_opt_word(parser, "deferrable") {
            parser.advance();
        } else if parser.check(TokenKind::Comma) {
            parser.advance();
        } else {
            break;
        }
    }
}

/// Parses the optional relation of a `VACUUM` statement.
///
/// Supported forms are `VACUUM` (every table in the current database) and
/// `VACUUM [schema.]table` (one table). PostgreSQL's option words are
/// tolerated: `FULL`/`ANALYZE` and the non-reserved `FREEZE`/`VERBOSE` are
/// skipped so a following relation is still recognized, which keeps
/// `VACUUM FULL t` targeting `t` rather than silently widening to every table.
/// Whatever the tail still holds is left to [`consume_housekeeping_tail`] so
/// the statement stream stays positioned exactly as before.
fn vacuum_table(parser: &mut Parser<'_>) -> Result<Statement, ParseError> {
    loop {
        match parser.peek().kind {
            TokenKind::Keyword(Keyword::Full) | TokenKind::Keyword(Keyword::Analyze) => {
                parser.advance();
            }
            TokenKind::Identifier
                if matches!(
                    parser.peek().lexeme.to_ascii_uppercase().as_str(),
                    "FREEZE" | "VERBOSE"
                ) =>
            {
                parser.advance();
            }
            _ => break,
        }
    }
    let table = if matches!(parser.peek().kind, TokenKind::Identifier) {
        Some(parser.parse_qualified_identifier("table name")?)
    } else {
        None
    };
    consume_housekeeping_tail(parser);
    Ok(Statement::Vacuum { table })
}

fn consume_housekeeping_tail(parser: &mut Parser<'_>) {
    loop {
        if parser.check(TokenKind::SemiColon) || matches!(parser.peek().kind, TokenKind::Eof) {
            break;
        }
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
        } else {
            parser.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ast::{
            ColumnDef, ColumnType, CreateStatement, Expression, FromClause, SelectTarget,
            Statement, Value,
        },
        catalog::Catalog,
        Lexer,
    };

    #[test]
    fn parse_create_table() {
        let tokens = Lexer::new("CREATE TABLE users (id INTEGER, name TEXT);")
            .lex()
            .unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Statement::CreateTable { .. }));
    }

    #[test]
    fn parse_alter_table_column_operations() {
        let mut catalog = crate::InMemoryCatalog::new();
        catalog
            .create_table(
                "users".into(),
                vec![ColumnDef {
                    name: "id".into(),
                    col_type: ColumnType::int4(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .unwrap();
        let add = Parser::new(
            Lexer::new("ALTER TABLE users ADD COLUMN name TEXT;")
                .lex()
                .unwrap(),
            &mut catalog,
        )
        .parse_statements()
        .unwrap();
        assert!(matches!(add[0], Statement::AlterTableAddColumn { .. }));
        let drop = Parser::new(
            Lexer::new("ALTER TABLE users DROP COLUMN name;")
                .lex()
                .unwrap(),
            &mut catalog,
        )
        .parse_statements()
        .unwrap();
        assert!(matches!(drop[0], Statement::AlterTableDropColumn { .. }));
    }

    #[test]
    fn common_postgres_type_aliases_use_existing_physical_types() {
        let mut catalog = crate::InMemoryCatalog::new();
        let statements = Parser::new(
            Lexer::new("CREATE TABLE accounts (id BIGINT, label VARCHAR);")
                .lex()
                .unwrap(),
            &mut catalog,
        )
        .parse_statements()
        .unwrap();
        let Statement::CreateTable { columns, .. } = &statements[0] else {
            panic!("expected CREATE TABLE");
        };
        assert_eq!(columns[0].col_type.type_oid, plomid_types::TypeOid::INT8);
        assert_eq!(columns[1].col_type.type_oid, plomid_types::TypeOid::VARCHAR);
    }

    #[test]
    fn parses_gui_generated_create_table_ddl() {
        let mut catalog = crate::InMemoryCatalog::new();
        let statements = Parser::new(
            Lexer::new(
                "CREATE TABLE IF NOT EXISTS public.gui_users (\
                 id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,\
                 email CHARACTER VARYING(255) NOT NULL,\
                 created_at TIMESTAMP WITHOUT TIME ZONE\
                 ) WITH (oids = false) TABLESPACE pg_default;",
            )
            .lex()
            .unwrap(),
            &mut catalog,
        )
        .parse_statements()
        .unwrap();
        let Statement::CreateTable {
            if_not_exists,
            columns,
            ..
        } = &statements[0]
        else {
            panic!("expected CREATE TABLE");
        };
        assert!(*if_not_exists);
        assert!(columns[0].col_type.serial);
        assert_eq!(columns[1].col_type.type_oid, plomid_types::TypeOid::VARCHAR);
        assert_eq!(
            columns[2].col_type.type_oid,
            plomid_types::TypeOid::TIMESTAMP
        );
        assert_eq!(columns[1].col_type.typmod, 259);
    }

    #[test]
    fn create_dispatcher_distinguishes_database_and_index() {
        let mut catalog = crate::catalog::InMemoryCatalog::new();
        let database = Parser::new(
            Lexer::new("CREATE DATABASE analytics;").lex().unwrap(),
            &mut catalog,
        )
        .parse_statements()
        .unwrap();
        assert_eq!(
            database,
            vec![Statement::Create(CreateStatement::Database {
                name: "analytics".into()
            })]
        );

        let index = Parser::new(
            Lexer::new("CREATE INDEX users_id_idx;").lex().unwrap(),
            &mut catalog,
        )
        .parse_statements()
        .unwrap();
        assert_eq!(
            index,
            vec![Statement::Create(CreateStatement::Index {
                name: "users_id_idx".into()
            })]
        );
    }

    #[test]
    fn parse_insert() {
        let tokens = Lexer::new("INSERT INTO users (id, name) VALUES (1, 'Alice');")
            .lex()
            .unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
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
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        matches!(stmts[0], Statement::Insert { .. });
    }

    #[test]
    fn parse_select() {
        let tokens = Lexer::new("SELECT * FROM users;").lex().unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
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
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        matches!(stmts[0], Statement::Select { .. });
    }

    #[test]
    fn parse_select_without_trailing_semicolon() {
        let tokens = Lexer::new("SELECT 1").lex().unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let statements = parser.parse_statements().unwrap();
        assert_eq!(
            statements,
            vec![Statement::Select {
                distinct: false,
                distinct_on: None,
                targets: vec![SelectTarget::Expr {
                    expr: Expression::Literal(Value::Int4(1)),
                    alias: None,
                }],
                from: None,
                where_expr: None,
                group_by: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
            }]
        );
    }

    #[test]
    fn parse_show_without_parameter_as_tables() {
        let mut catalog = crate::catalog::InMemoryCatalog::new();
        let statements = Parser::new(Lexer::new("SHOW;").lex().unwrap(), &mut catalog)
            .parse_statements()
            .unwrap();
        assert_eq!(
            statements,
            vec![Statement::Show {
                name: "tables".into()
            }]
        );
    }

    #[test]
    fn parse_use_database() {
        let mut catalog = crate::catalog::InMemoryCatalog::new();
        let statements = Parser::new(Lexer::new("USE plomid").lex().unwrap(), &mut catalog)
            .parse_statements()
            .unwrap();
        assert_eq!(
            statements,
            vec![Statement::Use {
                database: "plomid".into()
            }]
        );
    }

    #[test]
    fn parse_client_compatibility_expressions() {
        let tokens = Lexer::new("SELECT version(), current_database(), 1 AS x;")
            .lex()
            .expect("lexer should accept compatibility expressions");
        let mut catalog = crate::InMemoryCatalog::new();
        let statements = Parser::new(tokens, &mut catalog)
            .parse_statements()
            .expect("parser should accept compatibility expressions");
        assert!(matches!(
            &statements[0],
            Statement::Select { targets, .. }
                if matches!(&targets[0], SelectTarget::FunctionCall { name, .. } if name == "version")
                    && matches!(&targets[2], SelectTarget::Expr { alias: Some(alias), .. } if alias == "x")
        ));
    }

    #[test]
    fn parse_keyword_as_explicit_column_alias() {
        let tokens = Lexer::new("SELECT 1 AS schema, 2 AS table;")
            .lex()
            .expect("keyword aliases should lex");
        let mut catalog = crate::catalog::InMemoryCatalog::new();
        let statements = Parser::new(tokens, &mut catalog)
            .parse_statements()
            .expect("keyword aliases should parse");
        assert!(matches!(
            &statements[0],
            Statement::Select { targets, .. }
                if matches!(&targets[0], SelectTarget::Expr { alias: Some(alias), .. } if alias == "schema")
                && matches!(&targets[1], SelectTarget::Expr { alias: Some(alias), .. } if alias == "table")
        ));
    }

    #[test]
    fn parse_qualified_table_and_column() {
        let tokens = Lexer::new("SELECT users.id FROM public.users WHERE users.id = 1;")
            .lex()
            .expect("qualified identifiers should lex");
        let mut catalog = crate::InMemoryCatalog::new();
        catalog
            .create_table(
                "public.users".to_string(),
                vec![ColumnDef {
                    name: "id".to_string(),
                    col_type: ColumnType::int4(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .expect("test table should be created");
        let statements = Parser::new(tokens, &mut catalog)
            .parse_statements()
            .expect("qualified identifiers should parse");
        assert!(matches!(
            &statements[0],
            Statement::Select {
                from: Some(FromClause::Table { name, .. }),
                targets,
                ..
            } if name == "public.users" && targets == &vec![SelectTarget::column("users.id")]
        ));
    }

    #[test]
    fn parse_update() {
        let tokens = Lexer::new("UPDATE users SET name = 'Bob' WHERE id = 1;")
            .lex()
            .unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
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
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        matches!(stmts[0], Statement::Update { .. });
    }

    #[test]
    fn parse_delete() {
        let tokens = Lexer::new("DELETE FROM users WHERE id = 1;").lex().unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
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
                ],
                Vec::new(),
            )
            .unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        matches!(stmts[0], Statement::Delete { .. });
    }

    #[test]
    fn parse_unknown_table_error() {
        let tokens = Lexer::new("SELECT * FROM missing;").lex().unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let err = parser.parse_statements().unwrap_err();
        assert!(matches!(err, ParseError::UnknownTable(_)));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn syntax_error_reports_line_and_column() {
        let tokens = Lexer::new("SELECT * FORM users;").lex().unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let err = parser.parse_statements().unwrap_err();
        match err {
            ParseError::UnexpectedToken { line, column, .. } => {
                assert_eq!(line, 1);
                assert_eq!(column, 10);
            }
            _ => panic!("expected UnexpectedToken error, got: {err}"),
        }
    }

    #[test]
    fn multiline_syntax_error_reports_position() {
        let tokens = Lexer::new("SELECT *\nFROM\nWHERE id = 1;").lex().unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let err = parser.parse_statements().unwrap_err();
        match err {
            ParseError::UnexpectedToken { line, column, .. } => {
                assert_eq!(line, 3);
                assert!(column > 0);
            }
            _ => panic!("expected UnexpectedToken error, got: {err}"),
        }
    }

    #[test]
    fn parse_create_domain_with_position_check() {
        // TR2.2: CREATE DOMAIN with CHECK using POSITION('@' IN VALUE)
        let tokens =
            Lexer::new("CREATE DOMAIN email_addr AS TEXT CHECK (POSITION('@' IN VALUE) > 1);")
                .lex()
                .unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Statement::Create(CreateStatement::Domain { name, .. }) => {
                assert_eq!(name, "email_addr");
            }
            other => panic!("expected CREATE DOMAIN, got {other:?}"),
        }
    }

    #[test]
    fn parse_conflict_keyword_as_table_name() {
        // CONFLICT is a keyword but should be usable as a soft identifier
        // (table / column name) in PostgreSQL compatibility mode.
        let tokens = Lexer::new("CREATE TABLE conflict (id INTEGER);")
            .lex()
            .unwrap();
        let mut catalog = crate::InMemoryCatalog::new();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Statement::CreateTable { .. }));
    }

    #[test]
    fn select_value_keyword_as_column() {
        // TR2.3: `value` (lowercase) works as a column name in SELECT.
        let mut catalog = crate::InMemoryCatalog::new();
        catalog
            .create_table(
                "t".to_string(),
                vec![ColumnDef {
                    name: "value".to_string(),
                    col_type: ColumnType::text(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .unwrap();
        let tokens = Lexer::new("SELECT value FROM t;").lex().unwrap();
        let parser = Parser::new(tokens, &mut catalog);
        let stmts = parser.parse_statements().unwrap();
        assert_eq!(stmts.len(), 1);
        matches!(stmts[0], Statement::Select { .. });
    }
}
