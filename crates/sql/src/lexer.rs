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
//! Minimal deterministic SQL lexer for the PLOMID V1 subset.

use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword(Keyword),
    Identifier,
    IntegerLiteral,
    StringLiteral,
    /// `B'...'` / `b'...'` — PostgreSQL bit-string literal.
    BitLiteral,
    Star,
    Comma,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    SemiColon,
    Eq,
    Lt,
    Gt,
    Le,
    Ge,
    NotEq,
    Plus,
    Minus,
    Slash,
    Percent,
    /// `:` — used by `::` type casts and array slices.
    Colon,
    /// `::` — PostgreSQL-style type cast.
    DoubleColon,
    /// `->` — JSON field/element accessor returning JSON.
    Arrow,
    /// `->>` — JSON field/element accessor returning text.
    ArrowRight,
    /// `#>` — JSON path accessor returning JSON.
    HashArrow,
    /// `#>>` — JSON path accessor returning text.
    HashArrowRight,
    /// `#-` — JSONB path deletion operator (jsonb #- text[]).
    HashMinus,
    /// `@>` — JSON/JSONB containment.
    Contains,
    /// `<@` — inverse JSON/JSONB containment.
    ContainedBy,
    /// `?` — JSON key/array-string existence.
    JsonExists,
    /// `?|` — JSON key existence for any candidate.
    JsonExistsAny,
    /// `?&` — JSON key existence for all candidates.
    JsonExistsAll,
    /// `@?` — JSONPath operator (checks path existence).
    JsonPathExistsOp,
    /// `@@` — JSONPath match operator (checks predicate).
    JsonPathMatchOp,
    RegexMatch,
    RegexMatchInsensitive,
    RegexNotMatch,
    RegexNotMatchInsensitive,
    /// `^` — exponentiation / power operator.
    Caret,
    /// `||` — string concatenation operator.
    Concat,
    /// `&` — bitwise AND.
    Ampersand,
    /// single `|` — bitwise OR (distinct from `||` concatenation).
    Pipe,
    /// `#` — bitwise XOR.
    Hash,
    /// `<<` — bitwise left shift.
    LeftShift,
    /// `>>` — bitwise right shift.
    RightShift,
    Parameter,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Create,
    Table,
    Insert,
    Into,
    Values,
    Select,
    From,
    Where,
    Update,
    Set,
    Delete,
    Begin,
    Commit,
    Rollback,
    Integer,
    BigInt,
    SmallInt,
    Text,
    Varchar,
    Boolean,
    Not,
    Null,
    Primary,
    Key,
    Unique,
    Default,
    Check,
    On,
    To,
    Show,
    Distinct,
    Describe,
    Database,
    Use,
    Schema,
    View,
    Materialized,
    Index,
    Sequence,
    Function,
    Procedure,
    Trigger,
    Type,
    Domain,
    Role,
    User,
    Grant,
    Revoke,
    Extension,
    Drop,
    Alter,
    Add,
    Rename,
    True,
    False,
    Temp,
    Temporary,
    Group,
    By,
    Order,
    And,
    Or,
    Is,
    Asc,
    Desc,
    Having,
    If,
    Exists,
    As,
    For,
    Limit,
    Offset,
    Like,
    Ilike,
    Similar,
    In,
    Value,
    Between,
    Escape,
    Case,
    When,
    Then,
    Else,
    End,
    Coalesce,
    NullIf,
    Join,
    Lateral,
    Ordinality,
    Inner,
    Left,
    Right,
    Full,
    Outer,
    Cross,
    Using,
    Over,
    Partition,
    Rows,
    Range,
    Groups,
    Row,
    Preceding,
    Following,
    Unbounded,
    Current,
    Union,
    Intersect,
    Except,
    All,
    Any,
    With,
    Recursive,
    Explain,
    Analyze,
    Replace,
    Window,
    Array,
    Some,
    Only,
    Both,
    Leading,
    Trailing,
    Symmetric,
    Foreign,
    References,
    Cast,
    Extract,
    Date,
    Time,
    Timestamp,
    Timestamptz,
    Interval,
    CurrentDate,
    CurrentTime,
    CurrentTimestamp,
    LocalTime,
    LocalTimestamp,
    Filter,
    Nulls,
    First,
    Last,
    Comment,
    Enum,
    Column,
    Returning,
    Conflict,
    Grouping,
    Sets,
    Rollup,
    Cube,
    Do,
    Nothing,
    Copy,
    Stdin,
    Stdout,
    Collate,
    Vacuum,
    Constraint,
    Returns,
    Language,
    Cascade,
    Restrict,
    Absent,
    Without,
    Zone,
    Error,
    Empty,
    Format,
    Json,
    Path,
    Object,
    Scalar,
    Unknown,
    Storage,
    Extended,
    Plain,
    External,
    Main,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub line: u32,
    pub column: u32,
}

impl Token {
    pub fn new(kind: TokenKind, lexeme: impl Into<String>, line: u32, column: u32) -> Self {
        Self {
            kind,
            lexeme: lexeme.into(),
            line,
            column,
        }
    }
}

pub struct Lexer<'a> {
    input: Peekable<Chars<'a>>,
    line: u32,
    column: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input: input.chars().peekable(),
            line: 1,
            column: 1,
        }
    }

    pub fn lex(&mut self) -> Result<Vec<Token>, LexError> {
        tracing::trace!(target: "sql::lexer", "lex_start");
        let mut tokens = Vec::new();
        while let Some(token) = self.next_token()? {
            tracing::trace!(target: "sql::lexer", "token kind={:?} line={} column={}", token.kind, token.line, token.column);
            tokens.push(token);
        }
        tokens.push(Token::new(TokenKind::Eof, "", self.line, self.column));
        tracing::trace!(target: "sql::lexer", "lex_complete token_count={}", tokens.len());
        Ok(tokens)
    }

    fn next_token(&mut self) -> Result<Option<Token>, LexError> {
        self.skip_whitespace();
        let ch = match self.input.next() {
            Some(c) => c,
            None => return Ok(None),
        };
        let line = self.line;
        let column = self.column;
        self.advance_position(ch);

        match ch {
            ',' => Ok(Some(Token::new(TokenKind::Comma, ",", line, column))),
            ';' => Ok(Some(Token::new(TokenKind::SemiColon, ";", line, column))),
            '(' => Ok(Some(Token::new(TokenKind::LParen, "(", line, column))),
            ')' => Ok(Some(Token::new(TokenKind::RParen, ")", line, column))),
            '[' => Ok(Some(Token::new(TokenKind::LBracket, "[", line, column))),
            ']' => Ok(Some(Token::new(TokenKind::RBracket, "]", line, column))),
            '.' => Ok(Some(Token::new(TokenKind::Dot, ".", line, column))),
            '*' => Ok(Some(Token::new(TokenKind::Star, "*", line, column))),
            '+' => Ok(Some(Token::new(TokenKind::Plus, "+", line, column))),
            '/' => Ok(Some(Token::new(TokenKind::Slash, "/", line, column))),
            '%' => Ok(Some(Token::new(TokenKind::Percent, "%", line, column))),
            '^' => Ok(Some(Token::new(TokenKind::Caret, "^", line, column))),
            '|' if self.peek_char() == Some('|') => {
                self.input.next();
                self.advance_position('|');
                Ok(Some(Token::new(TokenKind::Concat, "||", line, column)))
            }
            '|' => Ok(Some(Token::new(TokenKind::Pipe, "|", line, column))),
            '&' => Ok(Some(Token::new(TokenKind::Ampersand, "&", line, column))),
            '=' => Ok(Some(Token::new(TokenKind::Eq, "=", line, column))),
            ':' if self.peek_char() == Some(':') => {
                self.input.next();
                self.advance_position(':');
                Ok(Some(Token::new(TokenKind::DoubleColon, "::", line, column)))
            }
            ':' => Ok(Some(Token::new(TokenKind::Colon, ":", line, column))),
            '<' if self.peek_char() == Some('<') => {
                self.input.next();
                self.advance_position('<');
                Ok(Some(Token::new(TokenKind::LeftShift, "<<", line, column)))
            }
            '<' if self.peek_char() == Some('=') => {
                self.input.next();
                self.advance_position('=');
                Ok(Some(Token::new(TokenKind::Le, "<=", line, column)))
            }
            '<' if self.peek_char() == Some('>') => {
                self.input.next();
                self.advance_position('>');
                Ok(Some(Token::new(TokenKind::NotEq, "<>", line, column)))
            }
            '<' if self.peek_char() == Some('@') => {
                self.input.next();
                self.advance_position('@');
                Ok(Some(Token::new(TokenKind::ContainedBy, "<@", line, column)))
            }
            '<' => Ok(Some(Token::new(TokenKind::Lt, "<", line, column))),
            '>' if self.peek_char() == Some('>') => {
                self.input.next();
                self.advance_position('>');
                Ok(Some(Token::new(TokenKind::RightShift, ">>", line, column)))
            }
            '>' if self.peek_char() == Some('=') => {
                self.input.next();
                self.advance_position('=');
                Ok(Some(Token::new(TokenKind::Ge, ">=", line, column)))
            }
            '>' => Ok(Some(Token::new(TokenKind::Gt, ">", line, column))),
            '@' if self.peek_char() == Some('>') => {
                self.input.next();
                self.advance_position('>');
                Ok(Some(Token::new(TokenKind::Contains, "@>", line, column)))
            }
            '@' if self.peek_char() == Some('?') => {
                self.input.next();
                self.advance_position('?');
                Ok(Some(Token::new(
                    TokenKind::JsonPathExistsOp,
                    "@?",
                    line,
                    column,
                )))
            }
            '@' if self.peek_char() == Some('@') => {
                self.input.next();
                self.advance_position('@');
                Ok(Some(Token::new(
                    TokenKind::JsonPathMatchOp,
                    "@@",
                    line,
                    column,
                )))
            }
            '@' => {
                return Err(LexError::new(
                    line,
                    column,
                    "unexpected character: @".to_string(),
                ))
            }
            '?' if self.peek_char() == Some('|') => {
                self.input.next();
                self.advance_position('|');
                Ok(Some(Token::new(
                    TokenKind::JsonExistsAny,
                    "?|",
                    line,
                    column,
                )))
            }
            '?' if self.peek_char() == Some('&') => {
                self.input.next();
                self.advance_position('&');
                Ok(Some(Token::new(
                    TokenKind::JsonExistsAll,
                    "?&",
                    line,
                    column,
                )))
            }
            '?' => Ok(Some(Token::new(TokenKind::JsonExists, "?", line, column))),
            '!' if self.peek_char() == Some('=') => {
                self.input.next();
                self.advance_position('=');
                Ok(Some(Token::new(TokenKind::NotEq, "!=", line, column)))
            }
            '!' if self.peek_char() == Some('~') => {
                self.input.next();
                self.advance_position('~');
                if self.peek_char() == Some('*') {
                    self.input.next();
                    self.advance_position('*');
                    Ok(Some(Token::new(
                        TokenKind::RegexNotMatchInsensitive,
                        "!~*",
                        line,
                        column,
                    )))
                } else {
                    Ok(Some(Token::new(
                        TokenKind::RegexNotMatch,
                        "!~",
                        line,
                        column,
                    )))
                }
            }
            '~' if self.peek_char() == Some('*') => {
                self.input.next();
                self.advance_position('*');
                Ok(Some(Token::new(
                    TokenKind::RegexMatchInsensitive,
                    "~*",
                    line,
                    column,
                )))
            }
            '~' => Ok(Some(Token::new(TokenKind::RegexMatch, "~", line, column))),
            '$' => self.read_parameter(line, column),
            '"' => self.read_quoted_identifier(line, column),
            '\'' => self.read_string(line, column),
            '-' if self.peek_char() == Some('-') => {
                self.skip_line_comment();
                self.next_token()
            }
            '-' if self.peek_char() == Some('>') => {
                self.input.next();
                self.advance_position('>');
                if self.peek_char() == Some('>') {
                    self.input.next();
                    self.advance_position('>');
                    Ok(Some(Token::new(TokenKind::ArrowRight, "->>", line, column)))
                } else {
                    Ok(Some(Token::new(TokenKind::Arrow, "->", line, column)))
                }
            }
            '#' if self.peek_char() == Some('-') => {
                self.input.next();
                self.advance_position('-');
                Ok(Some(Token::new(TokenKind::HashMinus, "#-", line, column)))
            }
            '#' if self.peek_char() == Some('>') => {
                self.input.next();
                self.advance_position('>');
                if self.peek_char() == Some('>') {
                    self.input.next();
                    self.advance_position('>');
                    Ok(Some(Token::new(
                        TokenKind::HashArrowRight,
                        "#>>",
                        line,
                        column,
                    )))
                } else {
                    Ok(Some(Token::new(TokenKind::HashArrow, "#>", line, column)))
                }
            }
            '#' => Ok(Some(Token::new(TokenKind::Hash, "#", line, column))),
            '-' => Ok(Some(Token::new(TokenKind::Minus, "-", line, column))),
            c if c.is_ascii_digit() => self.read_number(c, line, column),
            c if c.is_ascii_alphabetic() || c == '_' => self.read_ident_or_keyword(c, line, column),
            _ => Err(LexError::new(
                line,
                column,
                format!("unexpected character: {ch}"),
            )),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(&ch) = self.input.peek() {
            if ch.is_whitespace() {
                let c = self.input.next().expect("peek guarantees Some");
                self.advance_position(c);
            } else {
                break;
            }
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.input.next() {
            self.advance_position(ch);
            if ch == '\n' {
                break;
            }
        }
    }

    fn read_string(&mut self, line: u32, column: u32) -> Result<Option<Token>, LexError> {
        let mut value = String::new();
        while let Some(ch) = self.input.next() {
            self.advance_position(ch);
            if ch == '\'' {
                // Doubled single quote: consume and add a literal quote.
                if self.peek_char() == Some('\'') {
                    self.input.next();
                    self.advance_position('\'');
                    value.push('\'');
                } else {
                    break;
                }
            } else if ch == '\\' && self.peek_char() == Some('\'') {
                // PostgreSQL with standard_conforming_strings=on (the default)
                // treats backslash as a literal except `\'` inside a string.
                // Preserve `\'` as `'`; keep all other backslashes verbatim so
                // SQL escaping and JSON escaping stay separate layers.
                self.input.next();
                self.advance_position('\'');
                value.push('\'');
            } else {
                value.push(ch);
            }
        }
        Ok(Some(Token::new(
            TokenKind::StringLiteral,
            value.clone(),
            line,
            column,
        )))
    }

    /// Reads a PostgreSQL bit-string literal (`B'...'` / `b'...'`).
    ///
    /// PostgreSQL bit-string literals represent sequences of bits (0/1 characters).
    /// The `B` or `b` prefix indicates a bit string rather than a regular text string.
    /// The bit string body must contain only `0` and `1` characters.
    ///
    /// Unlike regular string literals, bit literals are semantically typed values
    /// that preserve their bit length and content. They are distinct from:
    /// - `'101010'` (a text string containing the characters "101010")
    /// - `101010` (an integer value)
    ///
    /// This method is called after the `B`/`b` prefix has been consumed and we're
    /// positioned at the opening quote of the bit string body.
    fn read_bit_literal(&mut self, line: u32, column: u32) -> Result<Option<Token>, LexError> {
        // Consume the opening quote.
        self.input.next();
        self.advance_position('\'');

        let mut value = String::new();
        while let Some(ch) = self.input.next() {
            self.advance_position(ch);
            if ch == '\'' {
                if self.peek_char() == Some('\'') {
                    // Doubled quote: consume and add a literal quote.
                    self.input.next();
                    self.advance_position('\'');
                    value.push('\'');
                } else {
                    // End of bit string.
                    break;
                }
            } else {
                value.push(ch);
            }
        }

        Ok(Some(Token::new(TokenKind::BitLiteral, value, line, column)))
    }

    /// Reads a PostgreSQL escape string literal (E'...').
    ///
    /// PostgreSQL escape strings process backslash sequences at the SQL layer
    /// before the value reaches any downstream parser (JSON, etc.). The
    /// supported sequences mirror PostgreSQL's standard_conforming_strings=off
    /// behavior for the E'...' syntax.
    ///
    /// Unicode escapes (\uXXXX) are converted to their UTF-8 representation
    /// so downstream JSON parsers see the actual character. This is critical
    /// for values like E'{"text":"\\u0041"}' where \\u0041 must become 'A'.
    fn read_escape_string(&mut self, line: u32, column: u32) -> Result<Option<Token>, LexError> {
        // Consume the opening quote.
        self.input.next();
        self.advance_position('\'');
        let mut value = String::new();
        while let Some(ch) = self.input.next() {
            self.advance_position(ch);
            if ch == '\'' {
                // Check for doubled single quote (literal quote character).
                // We need to peek at the next character without consuming it
                // until we know whether it's part of the string or the closing quote.
                match self.peek_char() {
                    Some('\'') => {
                        // Doubled quote: consume and add a literal quote.
                        self.input.next();
                        self.advance_position('\'');
                        value.push('\'');
                    }
                    _ => {
                        // End of string.
                        break;
                    }
                }
            } else if ch == '\\' {
                match self.input.next() {
                    Some('\'') => {
                        self.advance_position('\'');
                        value.push('\'');
                    }
                    Some('\\') => {
                        self.advance_position('\\');
                        value.push('\\');
                    }
                    Some('n') => {
                        self.advance_position('n');
                        value.push('\n');
                    }
                    Some('r') => {
                        self.advance_position('r');
                        value.push('\r');
                    }
                    Some('t') => {
                        self.advance_position('t');
                        value.push('\t');
                    }
                    Some('b') => {
                        self.advance_position('b');
                        value.push('\u{8}');
                    }
                    Some('f') => {
                        self.advance_position('f');
                        value.push('\u{c}');
                    }
                    Some('0') => {
                        self.advance_position('0');
                        value.push('\0');
                    }
                    Some('u') => {
                        // Unicode escape: read 4 hex digits.
                        //
                        // PostgreSQL E'...\uXXXX' sequences represent UTF-16 code
                        // units. For BMP characters (U+0000 to U+FFFF, excluding
                        // surrogates), this is a direct mapping to a Unicode
                        // scalar value.
                        //
                        // For surrogate code points (U+D800 to U+DFFF), we cannot
                        // create a Rust char directly. However, when the escape
                        // string is destined for JSON parsing (e.g. E'{"text":
                        // "\\uD83D\\uDE00"}'::jsonb), the JSON parser needs to
                        // see the individual \uXXXX sequences to combine them
                        // into a surrogate pair. Therefore, for surrogates, we
                        // preserve the original \uXXXX escape sequence rather
                        // than rejecting it.
                        self.advance_position('u');
                        let mut hex = String::with_capacity(4);
                        for _ in 0..4 {
                            match self.input.next() {
                                Some(c) => {
                                    self.advance_position(c);
                                    hex.push(c);
                                }
                                None => {
                                    return Err(LexError::new(
                                        line,
                                        column,
                                        "truncated Unicode escape".to_string(),
                                    ))
                                }
                            }
                        }
                        match u32::from_str_radix(&hex, 16) {
                            Ok(code) => {
                                // Check if this is a UTF-16 surrogate code point.
                                // Surrogates cannot be represented as Rust chars,
                                // but they are valid in JSON strings where they
                                // may form surrogate pairs.
                                if (0xD800..0xE000).contains(&code) {
                                    // Preserve the escape sequence for downstream
                                    // JSON parsing to handle as a surrogate pair.
                                    value.push('\\');
                                    value.push('u');
                                    value.push_str(&hex);
                                } else {
                                    match char::from_u32(code) {
                                        Some(c) => value.push(c),
                                        None => {
                                            return Err(LexError::new(
                                                line,
                                                column,
                                                format!("invalid Unicode code point: U+{code:04X}"),
                                            ))
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                return Err(LexError::new(
                                    line,
                                    column,
                                    format!("invalid Unicode escape: \\u{hex}"),
                                ))
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                value.push(ch);
            }
        }
        Ok(Some(Token::new(
            TokenKind::StringLiteral,
            value,
            line,
            column,
        )))
    }

    fn read_number(
        &mut self,
        first: char,
        line: u32,
        column: u32,
    ) -> Result<Option<Token>, LexError> {
        let mut lexeme = first.to_string();
        let mut is_float = false;
        while let Some(&ch) = self.input.peek() {
            if ch.is_ascii_digit() {
                lexeme.push(ch);
                let c = self.input.next().expect("peek guarantees Some");
                self.advance_position(c);
            } else if ch == '.' && !is_float {
                // Lookahead: a real decimal part is followed by a digit.
                let next = self.input.clone().nth(1);
                if next.is_some_and(|c| c.is_ascii_digit()) {
                    is_float = true;
                    lexeme.push(ch);
                    let c = self.input.next().expect("peek guarantees Some");
                    self.advance_position(c);
                } else {
                    break;
                }
            } else if (ch == 'e' || ch == 'E') && !lexeme.contains('e') && !lexeme.contains('E') {
                lexeme.push(ch);
                let c = self.input.next().expect("peek guarantees Some");
                self.advance_position(c);
                if let Some(&sign) = self.input.peek() {
                    if sign == '+' || sign == '-' {
                        lexeme.push(sign);
                        let s = self.input.next().expect("peek guarantees Some");
                        self.advance_position(s);
                    }
                }
                is_float = true;
            } else {
                break;
            }
        }
        Ok(Some(Token::new(
            TokenKind::IntegerLiteral,
            lexeme,
            line,
            column,
        )))
    }

    fn read_ident_or_keyword(
        &mut self,
        first: char,
        line: u32,
        column: u32,
    ) -> Result<Option<Token>, LexError> {
        let mut lexeme = first.to_string();
        while let Some(&ch) = self.input.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                lexeme.push(ch);
                let c = self.input.next().expect("peek guarantees Some");
                self.advance_position(c);
            } else {
                break;
            }
        }

        // PostgreSQL escape-string syntax: E'...' (e.g. E'hello\nworld').
        // When the identifier we just read is exactly 'E' or 'e' and is
        // immediately followed by a single quote, we reinterpret the whole
        // thing as an escape string literal. This must be handled at the
        // lexer level because escape-string processing happens before the
        // parser sees the token — the backslash sequences must be resolved
        // before the string reaches the JSON parser or expression evaluator.
        //
        // Without this, E'{"text":"\\u0041"}' would be lexed as identifier
        // E followed by a string literal containing the literal characters
        // \u0041, rather than the intended escape sequence.
        if (lexeme == "E" || lexeme == "e") && self.peek_char() == Some('\'') {
            return self.read_escape_string(line, column);
        }

        // PostgreSQL bit-string literal syntax: B'...' / b'...'.
        // When the identifier we just read is exactly 'B' or 'b' and is
        // immediately followed by a single quote, we reinterpret the whole
        // thing as a bit-string literal. This is analogous to E'...' handling
        // above — the bit-string syntax must be recognized at the lexer level
        // so the parser sees a single token rather than identifier B followed
        // by a string literal.
        //
        // Without this, B'101010' would be lexed as identifier B followed by
        // string literal '101010', which the parser then fails to interpret
        // as a bit value.
        if (lexeme == "B" || lexeme == "b") && self.peek_char() == Some('\'') {
            return self.read_bit_literal(line, column);
        }

        let kind = match lexeme.to_uppercase().as_str() {
            "CREATE" => TokenKind::Keyword(Keyword::Create),
            "TABLE" => TokenKind::Keyword(Keyword::Table),
            "INSERT" => TokenKind::Keyword(Keyword::Insert),
            "INTO" => TokenKind::Keyword(Keyword::Into),
            "VALUES" => TokenKind::Keyword(Keyword::Values),
            "SELECT" => TokenKind::Keyword(Keyword::Select),
            "FROM" => TokenKind::Keyword(Keyword::From),
            "WHERE" => TokenKind::Keyword(Keyword::Where),
            "UPDATE" => TokenKind::Keyword(Keyword::Update),
            "SET" => TokenKind::Keyword(Keyword::Set),
            "DELETE" => TokenKind::Keyword(Keyword::Delete),
            "BEGIN" => TokenKind::Keyword(Keyword::Begin),
            "COMMIT" => TokenKind::Keyword(Keyword::Commit),
            "ROLLBACK" => TokenKind::Keyword(Keyword::Rollback),
            "INTEGER" => TokenKind::Keyword(Keyword::Integer),
            "BIGINT" => TokenKind::Keyword(Keyword::BigInt),
            "SMALLINT" => TokenKind::Keyword(Keyword::SmallInt),
            "TEXT" => TokenKind::Keyword(Keyword::Text),
            "VARCHAR" | "CHAR" => TokenKind::Keyword(Keyword::Varchar),
            "BOOLEAN" | "BOOL" => TokenKind::Keyword(Keyword::Boolean),
            "NOT" => TokenKind::Keyword(Keyword::Not),
            "NULL" => TokenKind::Keyword(Keyword::Null),
            "PRIMARY" => TokenKind::Keyword(Keyword::Primary),
            "KEY" => TokenKind::Keyword(Keyword::Key),
            "UNIQUE" => TokenKind::Keyword(Keyword::Unique),
            "DEFAULT" => TokenKind::Keyword(Keyword::Default),
            "CHECK" => TokenKind::Keyword(Keyword::Check),
            "ON" => TokenKind::Keyword(Keyword::On),
            "TO" => TokenKind::Keyword(Keyword::To),
            "SHOW" => TokenKind::Keyword(Keyword::Show),
            "DISTINCT" => TokenKind::Keyword(Keyword::Distinct),
            "DESCRIBE" => TokenKind::Keyword(Keyword::Describe),
            "DATABASE" => TokenKind::Keyword(Keyword::Database),
            "USE" => TokenKind::Keyword(Keyword::Use),
            "SCHEMA" => TokenKind::Keyword(Keyword::Schema),
            "VIEW" => TokenKind::Keyword(Keyword::View),
            "MATERIALIZED" => TokenKind::Keyword(Keyword::Materialized),
            "INDEX" => TokenKind::Keyword(Keyword::Index),
            "SEQUENCE" => TokenKind::Keyword(Keyword::Sequence),
            "FUNCTION" => TokenKind::Keyword(Keyword::Function),
            "PROCEDURE" => TokenKind::Keyword(Keyword::Procedure),
            "TRIGGER" => TokenKind::Keyword(Keyword::Trigger),
            "TYPE" => TokenKind::Keyword(Keyword::Type),
            "DOMAIN" => TokenKind::Keyword(Keyword::Domain),
            "ROLE" => TokenKind::Keyword(Keyword::Role),
            "USER" => TokenKind::Keyword(Keyword::User),
            "GRANT" => TokenKind::Keyword(Keyword::Grant),
            "REVOKE" => TokenKind::Keyword(Keyword::Revoke),
            "EXTENSION" => TokenKind::Keyword(Keyword::Extension),
            "DROP" => TokenKind::Keyword(Keyword::Drop),
            "ALTER" => TokenKind::Keyword(Keyword::Alter),
            "ADD" => TokenKind::Keyword(Keyword::Add),
            "RENAME" => TokenKind::Keyword(Keyword::Rename),
            "TRUE" => TokenKind::Keyword(Keyword::True),
            "FALSE" => TokenKind::Keyword(Keyword::False),
            "TEMP" => TokenKind::Keyword(Keyword::Temp),
            "TEMPORARY" => TokenKind::Keyword(Keyword::Temporary),
            "GROUP" => TokenKind::Keyword(Keyword::Group),
            "BY" => TokenKind::Keyword(Keyword::By),
            "ORDER" => TokenKind::Keyword(Keyword::Order),
            "AND" => TokenKind::Keyword(Keyword::And),
            "OR" => TokenKind::Keyword(Keyword::Or),
            "IS" => TokenKind::Keyword(Keyword::Is),
            "ASC" => TokenKind::Keyword(Keyword::Asc),
            "DESC" => TokenKind::Keyword(Keyword::Desc),
            "HAVING" => TokenKind::Keyword(Keyword::Having),
            "IF" => TokenKind::Keyword(Keyword::If),
            "EXISTS" => TokenKind::Keyword(Keyword::Exists),
            "AS" => TokenKind::Keyword(Keyword::As),
            "FOR" => TokenKind::Keyword(Keyword::For),
            "LIMIT" => TokenKind::Keyword(Keyword::Limit),
            "OFFSET" => TokenKind::Keyword(Keyword::Offset),
            "LIKE" => TokenKind::Keyword(Keyword::Like),
            "ILIKE" => TokenKind::Keyword(Keyword::Ilike),
            "SIMILAR" => TokenKind::Keyword(Keyword::Similar),
            "IN" => TokenKind::Keyword(Keyword::In),
            "BETWEEN" => TokenKind::Keyword(Keyword::Between),
            "ESCAPE" => TokenKind::Keyword(Keyword::Escape),
            "CASE" => TokenKind::Keyword(Keyword::Case),
            "WHEN" => TokenKind::Keyword(Keyword::When),
            "THEN" => TokenKind::Keyword(Keyword::Then),
            "ELSE" => TokenKind::Keyword(Keyword::Else),
            "END" => TokenKind::Keyword(Keyword::End),
            "COALESCE" => TokenKind::Keyword(Keyword::Coalesce),
            "NULLIF" => TokenKind::Keyword(Keyword::NullIf),
            "JOIN" => TokenKind::Keyword(Keyword::Join),
            "LATERAL" => TokenKind::Keyword(Keyword::Lateral),
            "ORDINALITY" => TokenKind::Keyword(Keyword::Ordinality),
            "INNER" => TokenKind::Keyword(Keyword::Inner),
            "LEFT" => TokenKind::Keyword(Keyword::Left),
            "RIGHT" => TokenKind::Keyword(Keyword::Right),
            "FULL" => TokenKind::Keyword(Keyword::Full),
            "OUTER" => TokenKind::Keyword(Keyword::Outer),
            "CROSS" => TokenKind::Keyword(Keyword::Cross),
            "USING" => TokenKind::Keyword(Keyword::Using),
            "OVER" => TokenKind::Keyword(Keyword::Over),
            "PARTITION" => TokenKind::Keyword(Keyword::Partition),
            "ROWS" => TokenKind::Keyword(Keyword::Rows),
            "ROW" => TokenKind::Keyword(Keyword::Row),
            "RANGE" => TokenKind::Keyword(Keyword::Range),
            "GROUPS" => TokenKind::Keyword(Keyword::Groups),
            "PRECEDING" => TokenKind::Keyword(Keyword::Preceding),
            "FOLLOWING" => TokenKind::Keyword(Keyword::Following),
            "UNBOUNDED" => TokenKind::Keyword(Keyword::Unbounded),
            "CURRENT" => TokenKind::Keyword(Keyword::Current),
            "UNION" => TokenKind::Keyword(Keyword::Union),
            "INTERSECT" => TokenKind::Keyword(Keyword::Intersect),
            "EXCEPT" => TokenKind::Keyword(Keyword::Except),
            "ALL" => TokenKind::Keyword(Keyword::All),
            "ANY" => TokenKind::Keyword(Keyword::Any),
            "SOME" => TokenKind::Keyword(Keyword::Some),
            "WITH" => TokenKind::Keyword(Keyword::With),
            "RECURSIVE" => TokenKind::Keyword(Keyword::Recursive),
            "EXPLAIN" => TokenKind::Keyword(Keyword::Explain),
            "ANALYZE" => TokenKind::Keyword(Keyword::Analyze),
            "REPLACE" => TokenKind::Keyword(Keyword::Replace),
            "WINDOW" => TokenKind::Keyword(Keyword::Window),
            "ARRAY" => TokenKind::Keyword(Keyword::Array),
            "ONLY" => TokenKind::Keyword(Keyword::Only),
            "BOTH" => TokenKind::Keyword(Keyword::Both),
            "LEADING" => TokenKind::Keyword(Keyword::Leading),
            "TRAILING" => TokenKind::Keyword(Keyword::Trailing),
            "SYMMETRIC" => TokenKind::Keyword(Keyword::Symmetric),
            "FOREIGN" => TokenKind::Keyword(Keyword::Foreign),
            "REFERENCES" => TokenKind::Keyword(Keyword::References),
            "CAST" => TokenKind::Keyword(Keyword::Cast),
            "EXTRACT" => TokenKind::Keyword(Keyword::Extract),
            "DATE" => TokenKind::Keyword(Keyword::Date),
            "TIME" => TokenKind::Keyword(Keyword::Time),
            "TIMESTAMP" => TokenKind::Keyword(Keyword::Timestamp),
            "TIMESTAMPTZ" => TokenKind::Keyword(Keyword::Timestamptz),
            "INTERVAL" => TokenKind::Keyword(Keyword::Interval),
            "CURRENT_DATE" => TokenKind::Keyword(Keyword::CurrentDate),
            "CURRENT_TIME" => TokenKind::Keyword(Keyword::CurrentTime),
            "CURRENT_TIMESTAMP" => TokenKind::Keyword(Keyword::CurrentTimestamp),
            "LOCALTIME" => TokenKind::Keyword(Keyword::LocalTime),
            "LOCALTIMESTAMP" => TokenKind::Keyword(Keyword::LocalTimestamp),
            "FILTER" => TokenKind::Keyword(Keyword::Filter),
            "NULLS" => TokenKind::Keyword(Keyword::Nulls),
            "FIRST" => TokenKind::Keyword(Keyword::First),
            "LAST" => TokenKind::Keyword(Keyword::Last),
            "COMMENT" => TokenKind::Keyword(Keyword::Comment),
            "ENUM" => TokenKind::Keyword(Keyword::Enum),
            "VALUE" => TokenKind::Keyword(Keyword::Value),
            "COLUMN" => TokenKind::Keyword(Keyword::Column),
            "RETURNING" => TokenKind::Keyword(Keyword::Returning),
            "CONFLICT" => TokenKind::Keyword(Keyword::Conflict),
            "GROUPING" => TokenKind::Keyword(Keyword::Grouping),
            "SETS" => TokenKind::Keyword(Keyword::Sets),
            "ROLLUP" => TokenKind::Keyword(Keyword::Rollup),
            "CUBE" => TokenKind::Keyword(Keyword::Cube),
            "DO" => TokenKind::Keyword(Keyword::Do),
            "NOTHING" => TokenKind::Keyword(Keyword::Nothing),
            "COPY" => TokenKind::Keyword(Keyword::Copy),
            "STDIN" => TokenKind::Keyword(Keyword::Stdin),
            "STDOUT" => TokenKind::Keyword(Keyword::Stdout),
            "COLLATE" => TokenKind::Keyword(Keyword::Collate),
            "VACUUM" => TokenKind::Keyword(Keyword::Vacuum),
            "CONSTRAINT" => TokenKind::Keyword(Keyword::Constraint),
            "RETURNS" => TokenKind::Keyword(Keyword::Returns),
            "LANGUAGE" => TokenKind::Keyword(Keyword::Language),
            "CASCADE" => TokenKind::Keyword(Keyword::Cascade),
            "RESTRICT" => TokenKind::Keyword(Keyword::Restrict),
            "ABSENT" => TokenKind::Keyword(Keyword::Absent),
            "WITHOUT" => TokenKind::Keyword(Keyword::Without),
            // ZONE completes the multi-word `TIMESTAMP WITH TIME ZONE` type
            // name. Lexing it as a keyword keeps that type spelling stable
            // while leaving plain identifiers named "zone" reachable through
            // the qualified-name fallback in type parsing.
            "ZONE" => TokenKind::Keyword(Keyword::Zone),
            "ERROR" => TokenKind::Keyword(Keyword::Error),
            "EMPTY" => TokenKind::Keyword(Keyword::Empty),
            "FORMAT" => TokenKind::Keyword(Keyword::Format),
            "JSON" => TokenKind::Keyword(Keyword::Json),
            "PATH" => TokenKind::Keyword(Keyword::Path),
            "OBJECT" => TokenKind::Keyword(Keyword::Object),
            "SCALAR" => TokenKind::Keyword(Keyword::Scalar),
            "UNKNOWN" => TokenKind::Keyword(Keyword::Unknown),
            "STORAGE" => TokenKind::Keyword(Keyword::Storage),
            "EXTENDED" => TokenKind::Keyword(Keyword::Extended),
            "PLAIN" => TokenKind::Keyword(Keyword::Plain),
            "EXTERNAL" => TokenKind::Keyword(Keyword::External),
            "MAIN" => TokenKind::Keyword(Keyword::Main),
            _ => TokenKind::Identifier,
        };
        Ok(Some(Token::new(kind, lexeme, line, column)))
    }

    fn peek_char(&mut self) -> Option<char> {
        self.input.peek().copied()
    }

    fn read_parameter(&mut self, line: u32, column: u32) -> Result<Option<Token>, LexError> {
        // PostgreSQL dollar-quoting: `$$body$$`. The lexer returns the inner
        // text as a single StringLiteral token so embedded `$1`-style
        // parameter references are never confused with prepared-statement
        // parameter markers.
        if self.peek_char() == Some('$') {
            self.input.next(); // second '$' of the opening delimiter
            self.advance_position('$');
            let mut value = String::new();
            loop {
                let Some(ch) = self.input.next() else {
                    return Err(LexError::new(
                        line,
                        column,
                        "unterminated dollar-quoted string",
                    ));
                };
                self.advance_position(ch);
                if ch == '$' && self.peek_char() == Some('$') {
                    self.input.next();
                    self.advance_position('$');
                    break;
                }
                value.push(ch);
            }
            return Ok(Some(Token::new(
                TokenKind::StringLiteral,
                value,
                line,
                column,
            )));
        }
        let mut lexeme = String::from("$");
        while let Some(&ch) = self.input.peek() {
            if ch.is_ascii_digit() {
                lexeme.push(ch);
                self.input.next();
                self.advance_position(ch);
            } else {
                break;
            }
        }
        if lexeme.len() == 1 {
            return Err(LexError::new(
                line,
                column,
                "parameter marker requires a number",
            ));
        }
        Ok(Some(Token::new(TokenKind::Parameter, lexeme, line, column)))
    }

    fn read_quoted_identifier(
        &mut self,
        line: u32,
        column: u32,
    ) -> Result<Option<Token>, LexError> {
        let mut value = String::new();
        while let Some(ch) = self.input.next() {
            self.advance_position(ch);
            if ch == '"' {
                if self.peek_char() == Some('"') {
                    self.input.next();
                    self.advance_position('"');
                    value.push('"');
                } else {
                    return Ok(Some(Token::new(TokenKind::Identifier, value, line, column)));
                }
            } else {
                value.push(ch);
            }
        }
        Err(LexError::new(
            line,
            column,
            "unterminated quoted identifier",
        ))
    }

    fn advance_position(&mut self, ch: char) {
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl LexError {
    pub fn new(line: u32, column: u32, message: impl Into<String>) -> Self {
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for LexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "syntax error at line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for LexError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_create_table() {
        let tokens = Lexer::new("CREATE TABLE users (id INTEGER, name TEXT);")
            .lex()
            .unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Create),
                TokenKind::Keyword(Keyword::Table),
                TokenKind::Identifier,
                TokenKind::LParen,
                TokenKind::Identifier,
                TokenKind::Keyword(Keyword::Integer),
                TokenKind::Comma,
                TokenKind::Identifier,
                TokenKind::Keyword(Keyword::Text),
                TokenKind::RParen,
                TokenKind::SemiColon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_string_literal() {
        let tokens = Lexer::new("'Alice'").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
        assert_eq!(tokens[0].lexeme, "Alice");
    }

    #[test]
    fn lex_integer_literal() {
        let tokens = Lexer::new("42").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntegerLiteral);
        assert_eq!(tokens[0].lexeme, "42");
    }

    #[test]
    fn lex_keywords_case_insensitive() {
        let tokens = Lexer::new("create table").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Create));
        assert_eq!(tokens[1].kind, TokenKind::Keyword(Keyword::Table));
    }

    #[test]
    fn lex_select_star() {
        let tokens = Lexer::new("SELECT * FROM users;").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Select),
                TokenKind::Star,
                TokenKind::Keyword(Keyword::From),
                TokenKind::Identifier,
                TokenKind::SemiColon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_transaction_keywords() {
        let tokens = Lexer::new("BEGIN; COMMIT; ROLLBACK;").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::SemiColon,
                TokenKind::Keyword(Keyword::Commit),
                TokenKind::SemiColon,
                TokenKind::Keyword(Keyword::Rollback),
                TokenKind::SemiColon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_error_reports_position() {
        let err = Lexer::new("SELECT * FR@M users;").lex().unwrap_err();
        assert!(err.to_string().contains("syntax error at line 1"));
        assert!(err.to_string().contains("unexpected character"));
    }
}
