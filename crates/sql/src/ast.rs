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
//! Abstract syntax tree for the PLOMID V1 SQL subset.
//!
//! The AST represents the structure of parsed SQL statements. V1 supports a
//! restricted subset sufficient for basic database operations: DDL, DML, and
//! transaction control. Bare `SELECT` expressions without a `FROM` clause are
//! also supported for compatibility with standard client tooling.

// The document modality's AST vocabulary lives in `plomid-json`, so the parser
// consumes the modality's own description instead of defining one.
pub use plomid_json::ast::{
    JsonKind, JsonTableColumn, JsonTableColumnDefault, JsonTableColumnKind, NullHandling,
};

/// A top-level SQL statement.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    CreateTable {
        name: String,
        if_not_exists: bool,
        temporary: bool,
        columns: Vec<ColumnDef>,
        constraints: Vec<Constraint>,
    },
    /// Extensible CREATE dispatcher for database objects not yet backed by a
    /// catalog subsystem. Keeping these forms distinct prevents the parser
    /// from treating every CREATE command as CREATE TABLE.
    Create(CreateStatement),
    CreateIndex {
        name: String,
        table: String,
        column: String,
        /// Ordered indexed column list for composite indexes
        /// (`CREATE INDEX ... ON t (a, b)`). Exactly `[column]` for a plain
        /// single-column index, empty for an expression index (whose key
        /// comes from `expression`). The tuple is the conflict domain.
        columns: Vec<String>,
        /// For expression indexes (`CREATE INDEX ... ON t((payload ->> 'x'))`)
        /// the parsed index expression, preserved as an AST — never resolved
        /// through its textual/Debug form. `None` for plain column indexes.
        expression: Option<Expression>,
        unique: bool,
        if_not_exists: bool,
        /// Optional index method like "GIN", "BTREE", "HASH".
        using: Option<String>,
        /// PostgreSQL GIN operator class (e.g. `jsonb_path_ops`). This is
        /// metadata that tells the index how to build keys, distinct from
        /// the column/expression being indexed. Preserved for catalog
        /// fidelity; execution support depends on the underlying index type.
        operator_class: Option<String>,
    },
    /// `CREATE VIEW name AS SELECT ...` (or `CREATE OR REPLACE VIEW`).
    CreateView {
        name: String,
        columns: Vec<String>,
        query: Box<Statement>,
        or_replace: bool,
    },
    /// A bare `VALUES (…), (…), …` row constructor query.
    Values(Vec<Vec<Expression>>),
    Insert {
        table: String,
        columns: Option<Vec<String>>,
        source: InsertSource,
        returning: Option<Vec<SelectTarget>>,
        on_conflict: Option<OnConflict>,
    },
    /// `COPY table [(…)] FROM STDIN` / `COPY … TO STDOUT`.
    ///
    /// The network layer owns COPY protocol framing. The SQL layer exposes
    /// `Copy` only as a parsed statement shape so the executor can bridge the
    /// incoming row stream onto the shared validated insertion path.
    Copy {
        table: String,
        columns: Option<Vec<String>>,
        direction: CopyDirection,
    },
    Select {
        targets: Vec<SelectTarget>,
        distinct: bool,
        distinct_on: Option<Vec<Expression>>,
        from: Option<FromClause>,
        where_expr: Option<Expression>,
        group_by: Option<GroupByClause>,
        having: Option<Expression>,
        order_by: Vec<OrderByItem>,
        limit: Option<usize>,
        offset: Option<usize>,
    },
    /// A composite SELECT formed by UNION/INTERSECT/EXCEPT combining two SELECTs.
    SetOperation {
        op: SetOpKind,
        all: bool,
        left: Box<Statement>,
        right: Box<Statement>,
    },
    /// A SELECT preceded by a WITH clause.
    With {
        recursive: bool,
        ctes: Vec<Cte>,
        body: Box<Statement>,
    },
    /// `EXPLAIN <stmt>` - logical plan rendering.
    Explain {
        statement: Box<Statement>,
        analyze: bool,
        /// Optional EXPLAIN format (e.g., "text", "json"). None means default text format.
        format: Option<String>,
    },
    Update {
        table: String,
        /// Optional target alias: `UPDATE products p SET ...` or
        /// `UPDATE products AS p SET ...`. The alias qualifies references to
        /// target columns in SET / WHERE / RETURNING (the evaluator already
        /// unqualifies `alias.col`, so only parsing needs to accept it).
        alias: Option<String>,
        /// Optional `UPDATE ... FROM <relation>` source. The FROM relation is
        /// only a source of rows for SET/WHERE/RETURNING expression
        /// evaluation — it is never itself a write target. Reuses the same
        /// `FromClause` shape as SELECT (tables, subqueries, joins).
        from: Option<FromClause>,
        assignments: Vec<(Expression, Expression)>,
        where_expr: Option<Expression>,
        /// PostgreSQL `UPDATE ... RETURNING <targets>` (e.g. section 35 of
        /// `PLOMID_TORTURE.sql`: `RETURNING id, credit_limit`). `None` means
        /// plain UPDATE (row-count result); `Some(targets)` means project the
        /// post-update row per affected tuple and return it as `Rows`.
        returning: Option<Vec<SelectTarget>>,
    },
    Delete {
        table: String,
        /// Optional target alias: `DELETE FROM products p` or
        /// `DELETE FROM products AS p`. The alias qualifies references to
        /// target columns in WHERE / USING conditions / RETURNING (the
        /// evaluator already unqualifies `alias.col`, so only parsing needs
        /// to accept it).
        alias: Option<String>,
        /// Optional `DELETE ... USING <relation list>` source relations
        /// (PostgreSQL `DELETE ... USING`). These relations are only a source
        /// of rows for WHERE / RETURNING expression evaluation — they are
        /// never themselves deleted. Reuses the same `FromClause` shape as
        /// SELECT (tables, subqueries, joins; comma-separated relations are a
        /// cross join, matching SELECT's FROM grammar).
        using: Option<FromClause>,
        where_expr: Option<Expression>,
        /// PostgreSQL `DELETE ... RETURNING <targets>` (e.g. `RETURNING id,
        /// email`). Evaluated against the pre-delete row, mirroring PG which
        /// exposes the deleted tuple to RETURNING.
        returning: Option<Vec<SelectTarget>>,
    },
    Begin,
    Commit,
    Rollback,
    Set {
        name: String,
        value: String,
    },
    Show {
        name: String,
    },
    /// SQL-client convenience command. PostgreSQL itself normally exposes
    /// this through the wire Describe message, but some database tools send
    /// DESCRIBE as SQL during discovery.
    Describe {
        name: String,
    },
    Use {
        database: String,
    },
    GrantRole {
        role: String,
        member: String,
    },
    DropTable {
        name: String,
        if_exists: bool,
        /// `CASCADE` (recursively drop dependent objects) vs. the default/
        /// `RESTRICT` (refuse when dependent objects exist).
        cascade: bool,
    },
    DropView {
        name: String,
        if_exists: bool,
        /// `CASCADE` (recursively drop dependent objects) vs. the default/
        /// `RESTRICT` (refuse when dependent objects exist).
        cascade: bool,
    },
    DropSchema {
        name: String,
        if_exists: bool,
        /// `CASCADE` (recursively drop contained objects) vs. the default/
        /// `RESTRICT` (refuse when the schema has dependent objects).
        cascade: bool,
    },
    DropSequence {
        name: String,
        if_exists: bool,
    },
    DropIndex {
        name: String,
        if_exists: bool,
    },
    /// `DROP TYPE name [CASCADE | RESTRICT]`.
    DropType {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    /// `DROP DOMAIN name [CASCADE | RESTRICT]`.
    DropDomain {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    /// `DROP FUNCTION name(args) [CASCADE | RESTRICT]`.
    DropFunction {
        name: String,
        args: Vec<String>,
        if_exists: bool,
        cascade: bool,
    },
    AlterTableRename {
        table: String,
        new_name: String,
    },
    AlterTableRenameColumn {
        table: String,
        old_name: String,
        new_name: String,
    },
    AlterTableAddColumn {
        table: String,
        column: ColumnDef,
        if_not_exists: bool,
    },
    AlterTableAddConstraint {
        table: String,
        constraint: Constraint,
    },
    AlterTableDropColumn {
        table: String,
        column: String,
        /// `CASCADE` (drop dependent objects) vs. the default/
        /// `RESTRICT` (refuse when dependent objects exist).
        cascade: bool,
    },
    CommentOn {
        object: CommentObject,
        comment: Option<String>,
    },
    Vacuum {
        table: Option<String>,
    },
    Analyze {
        table: Option<String>,
    },
    Reindex,
    Lock,
    Cluster,
    RefreshMaterializedView {
        name: String,
    },
    Truncate {
        tables: Vec<String>,
    },
    /// `DO $$ ... $$` anonymous code block (simplified PL/pgSQL execution).
    ///
    /// PLOMID V1 only supports the subset required by compatibility tests:
    /// PERFORM (discard result), RAISE NOTICE, and exception handling.
    Do {
        body: String,
    },
}

/// One expression in an INSERT VALUES row.
///
/// Plain literals share a variant with NULL/boolean/text, while parameter
/// references and scalar subqueries make room for richer INSERT inputs
/// without breaking literal-only call sites.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertValue {
    /// A typed literal value.
    Literal(Value),
    /// A bare `DEFAULT` keyword, meaning "use the column's DEFAULT".
    Default,
    /// An arbitrary scalar expression evaluated per inserted row.
    Expression(Box<Expression>),
}

/// The data source for an INSERT statement.
///
/// `VALUES` rows and `SELECT` results share one insertion path at the
/// executor/storage layer; the AST keeps the two forms distinct because
/// they have different parsing and planning constraints.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertSource {
    /// `VALUES (…), (…), …` — one or more explicit row tuples.
    Values(Vec<Vec<InsertValue>>),
    /// `DEFAULT VALUES` — shorthand for `VALUES (DEFAULT, DEFAULT, …)`.
    DefaultValues,
    /// `INSERT … SELECT …` — rows produced by a subquery.
    Select(Box<Statement>),
}

/// Target of `ON CONFLICT` conflict-resolution.
///
/// PostgreSQL infers the arbiter uniqueness constraint from an optional
/// explicit column list, or from the table's unique/primary-key columns when
/// no list is given.
#[derive(Debug, Clone, PartialEq)]
pub enum OnConflictTarget {
    /// `ON CONFLICT DO NOTHING` with no explicit inference target.
    NoTarget,
    /// `ON CONFLICT (col, ...)` — explicit arbiter column list.
    Columns(Vec<String>),
}

/// `ON CONFLICT ...` clause of an INSERT statement.
///
/// Carries the arbiter target plus the chosen action (`DO NOTHING` or
/// `DO UPDATE SET <assignments> [WHERE <cond>]`).
#[derive(Debug, Clone, PartialEq)]
pub enum OnConflict {
    /// `ON CONFLICT [target] DO NOTHING` — skip a conflicting row.
    DoNothing { target: OnConflictTarget },
    /// `ON CONFLICT [target] DO UPDATE SET ... [WHERE ...]`.
    DoUpdate {
        target: OnConflictTarget,
        assignments: Vec<(Expression, Expression)>,
        where_expr: Option<Expression>,
    },
}

/// Direction of a COPY statement: rows flowing into or out of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyDirection {
    /// `COPY … FROM …` — bulk row ingestion.
    From,
    /// `COPY … TO …` — bulk row extraction.
    To,
}

/// A single CTE definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Cte {
    /// CTE name.
    pub name: String,
    /// Optional column alias list.
    pub columns: Vec<String>,
    /// The CTE's defining query.
    pub query: Box<Statement>,
}

/// Set operation kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOpKind {
    /// `UNION` or `UNION ALL`.
    Union,
    /// `INTERSECT` or `INTERSECT ALL`.
    Intersect,
    /// `EXCEPT` or `EXCEPT ALL`.
    Except,
}

/// A `FROM` clause, either a single table reference or a JOIN tree.
#[derive(Debug, Clone, PartialEq)]
pub enum FromClause {
    Table {
        name: String,
        alias: Option<String>,
    },
    TableFunction {
        name: String,
        args: Vec<Expression>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        /// Column definitions with types for table functions like
        /// `json_to_record(...) AS x(id INTEGER, name TEXT)`.
        /// Each entry is (column_name, type_name).
        column_defs: Vec<(String, String)>,
        /// `JSON_TABLE(... COLUMNS (...))` column specs. Empty for every
        /// non-JSON_TABLE table function.
        json_columns: Vec<JsonTableColumn>,
        /// Whether this table function is LATERAL (can reference outer columns).
        lateral: bool,
    },
    Join {
        left: Box<FromClause>,
        kind: JoinKind,
        right: Box<FromClause>,
        on: Option<Expression>,
    },
    Subquery {
        statement: Box<Statement>,
        alias: String,
        /// Renames of the subquery's output columns from an explicit column
        /// alias list: `FROM (SELECT ...) AS x(a, b)`.
        column_aliases: Vec<String>,
        /// Whether this subquery is LATERAL (can reference outer columns).
        lateral: bool,
    },
}

/// Supported JOIN kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

/// Target of a `COMMENT ON` statement.
#[derive(Debug, Clone, PartialEq)]
pub enum CommentObject {
    Table { name: String },
    Column { table: String, column: String },
    Type { name: String },
    Schema { name: String },
    Role { name: String },
    View { name: String },
    Index { name: String },
    Sequence { name: String },
}

/// Object kinds selected by the `CREATE` dispatcher.
#[derive(Debug, Clone, PartialEq)]
pub enum CreateStatement {
    Database {
        name: String,
    },
    Schema {
        name: String,
        if_not_exists: bool,
    },
    View {
        name: String,
    },
    MaterializedView {
        name: String,
    },
    Index {
        name: String,
    },
    Sequence {
        name: String,
    },
    /// `CREATE FUNCTION name(args) RETURNS type LANGUAGE lang AS $$ body $$`.
    Function {
        name: String,
        args: Vec<FunctionArg>,
        returns: String,
        language: String,
        body: String,
    },
    Procedure {
        name: String,
    },
    Trigger {
        name: String,
    },
    /// `CREATE TYPE name AS ENUM ('a', 'b', ...)`.
    Type {
        name: String,
        labels: Option<Vec<String>>,
        /// Composite type attributes: [(column_name, type_name), ...].
        /// `None` for enums, `Some([])` for standalone composites without columns.
        attributes: Option<Vec<(String, String)>>,
    },
    /// `CREATE DOMAIN name AS type [CONSTRAINT ... CHECK (...)]`.
    Domain {
        name: String,
        base_type: String,
        constraints: Vec<DomainConstraint>,
    },
    Role {
        name: String,
        login: bool,
        password: Option<String>,
    },
    Extension {
        name: String,
    },
    Unsupported {
        object: String,
    },
}

/// A function argument: `name type`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionArg {
    pub name: String,
    pub data_type: String,
}

/// A domain constraint: optional name + CHECK expression.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainConstraint {
    pub name: Option<String>,
    pub check: String,
}

/// A column definition in a CREATE TABLE statement.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub col_type: ColumnType,
    pub constraints: Vec<Constraint>,
}

/// Representation of the GROUP BY clause.
///
/// Legacy `GROUP BY a, b, c` maps to `Simple(vec![a, b, c])`.
/// GROUPING SETS / ROLLUP / CUBE all normalize to the `Sets` representation
/// (parser expansion), where each inner vector is one explicit grouping set. Empty inner
/// vector is the grand-total set `()`.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupByClause {
    Simple(Vec<Expression>),
    Sets(Vec<Vec<Expression>>),
}

impl GroupByClause {
    #[must_use]
    pub fn is_simple(&self) -> Option<&Vec<Expression>> {
        match self {
            Self::Simple(v) => Some(v),
            Self::Sets(_) => None,
        }
    }

    #[must_use]
    pub fn to_sets(&self) -> Vec<Vec<Expression>> {
        match self {
            Self::Simple(v) => vec![v.clone()],
            Self::Sets(v) => v.clone(),
        }
    }

    #[must_use]
    pub fn union_exprs(&self) -> Vec<Expression> {
        let mut seen: Vec<Expression> = Vec::new();
        for set in self.to_sets() {
            for e in set {
                if !seen.iter().any(|s| s == &e) {
                    seen.push(e);
                }
            }
        }
        seen
    }
}

/// Action taken when a referenced row changes. Mirrors PostgreSQL
/// { ON DELETE / ON UPDATE { NO ACTION | RESTRICT | CASCADE | SET NULL | SET DEFAULT }.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKeyAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

/// MATCH type for composite foreign keys. Only Simple is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKeyMatch {
    Simple,
    Full,
    Partial,
}

/// A table or column constraint.
///
/// Column-level constraints (e.g. `id INTEGER PRIMARY KEY`) and table-level
/// constraints (e.g. `UNIQUE (email)`, `CHECK (age >= 0)`) share one common
/// representation so enforcement for INSERT/UPDATE is a single code path.
/// `columns` names the constrained columns; `expr` carries the operand of
/// `CHECK`/`DEFAULT` constraints.
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub name: Option<String>,
    pub kind: ConstraintKind,
    pub columns: Vec<String>,
    pub expr: Option<Expression>,
}

impl Constraint {
    #[must_use]
    pub fn new(kind: ConstraintKind, columns: Vec<String>) -> Self {
        Self {
            name: None,
            kind,
            columns,
            expr: None,
        }
    }

    #[must_use]
    pub fn check(expr: Expression, columns: Vec<String>) -> Self {
        Self {
            name: None,
            kind: ConstraintKind::Check,
            columns,
            expr: Some(expr),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintKind {
    NotNull,
    PrimaryKey,
    Unique,
    Check,
    Default,
    /// `GENERATED ALWAYS AS (expr) STORED` — the column value is computed from
    /// `expr` at write time. Distinct from `Default` (fill value on absent
    /// input) and from identity columns (`col_type.serial`).
    GeneratedAlways,
    /// `FOREIGN KEY (local_cols) REFERENCES ref_table(ref_cols) ...`
    ForeignKey {
        ref_table: String,
        ref_columns: Vec<String>,
        on_delete: ForeignKeyAction,
        on_update: ForeignKeyAction,
        match_type: ForeignKeyMatch,
    },
}

// `ColumnType` is the type registry's column contract, shared by every layer.
pub use plomid_types::ColumnType;

/// A SQL runtime value.
///
/// PLOMID values are the authoritative `plomid_types::PgValue` enumeration. SQL
/// text literals, comparisons, casts, and formatting all delegate to `plomid-types`.
pub use plomid_types::PgValue as Value;

/// Comparison operators for quantified comparisons and regular comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOperator {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// Quantifier for quantified comparisons: ANY (including SOME) or ALL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantifier {
    Any,
    All,
}

/// Boolean truth value requested by `expr IS [NOT] TRUE/FALSE/UNKNOWN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsBooleanKind {
    /// `IS TRUE`
    True,
    /// `IS FALSE`
    False,
    /// `IS UNKNOWN` (SQL three-valued logic: expression is NULL)
    Unknown,
}

/// The expression tree covers literals, column references, comparisons,
/// boolean logic, arithmetic, NULL predicates, function calls, and
/// SQL-specific forms such as `IN`, `BETWEEN`, `LIKE`, `CASE`, `COALESCE`,
/// and subqueries. It is produced once by the parser and evaluated by the
/// executor against a row (or group) using standard SQL three-valued logic.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// A reference to a column. `table.column` and `column` are both
    /// represented here; resolution lives in the binder/executor.
    ColumnRef(String),
    Literal(Value),
    /// `*` used as the argument of an aggregate such as `count(*)`.
    Star,
    Equal(Box<Expression>, Box<Expression>),
    NotEqual(Box<Expression>, Box<Expression>),
    Less(Box<Expression>, Box<Expression>),
    LessOrEqual(Box<Expression>, Box<Expression>),
    Greater(Box<Expression>, Box<Expression>),
    GreaterOrEqual(Box<Expression>, Box<Expression>),
    IsNull(Box<Expression>),
    IsNotNull(Box<Expression>),
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Not(Box<Expression>),
    Add(Box<Expression>, Box<Expression>),
    Subtract(Box<Expression>, Box<Expression>),
    Multiply(Box<Expression>, Box<Expression>),
    Divide(Box<Expression>, Box<Expression>),
    Modulo(Box<Expression>, Box<Expression>),
    /// PostgreSQL string concatenation (`||`).
    Concat(Box<Expression>, Box<Expression>),
    Negate(Box<Expression>),
    /// `expr IN (v1, v2, ...)` or `expr IN (SELECT ...)`.
    In {
        expr: Box<Expression>,
        list: Vec<Expression>,
        subquery: Option<Box<Statement>>,
        negated: bool,
    },
    /// `expr BETWEEN low AND high` / `NOT BETWEEN`.
    Between {
        expr: Box<Expression>,
        low: Box<Expression>,
        high: Box<Expression>,
        negated: bool,
    },
    /// `expr LIKE pattern [ESCAPE c]` / `NOT LIKE`.
    Like {
        expr: Box<Expression>,
        pattern: Box<Expression>,
        escape: Option<char>,
        negated: bool,
    },
    /// `CASE WHEN cond THEN val ... [ELSE def] END`.
    Case {
        /// Optional operand for the simple `CASE x WHEN v ...` form.
        operand: Option<Box<Expression>>,
        /// `WHEN` arms evaluated top to bottom; the first match wins.
        whens: Vec<(Expression, Expression)>,
        /// `ELSE` default value, if any.
        default: Option<Box<Expression>>,
    },
    /// `COALESCE(a, b, ...)`: first non-NULL argument.
    Coalesce(Vec<Expression>),
    /// `NULLIF(a, b)`: NULL if equal to `b`, else `a`.
    NullIf(Box<Expression>, Box<Expression>),
    /// `EXISTS (SELECT ...)`.
    Exists(Box<Statement>),
    /// `expr IS DISTINCT FROM expr` (NULL-aware not-equal).
    IsDistinctFrom(Box<Expression>, Box<Expression>),
    /// `expr IS [NOT] JSON [VALUE|OBJECT|ARRAY|SCALAR]`.
    IsJson {
        expr: Box<Expression>,
        kind: JsonKind,
        negated: bool,
    },
    /// `expr IS [NOT] TRUE/FALSE/UNKNOWN`.
    IsBoolean {
        expr: Box<Expression>,
        kind: IsBooleanKind,
        negated: bool,
    },
    /// A scalar subquery, which must yield exactly one row and one column.
    ScalarSubquery(Box<Statement>),
    /// A quantified comparison: `expr <op> ANY/ALL/SOME (subquery)`.
    /// SOME is treated as a synonym for ANY.
    QuantifiedComparison {
        left: Box<Expression>,
        operator: ComparisonOperator,
        quantifier: Quantifier,
        subquery: Box<Statement>,
    },
    /// A window function call: `<name>(<args>) OVER (...)`.
    WindowFunction {
        name: String,
        args: Vec<Expression>,
        over: WindowSpec,
    },
    Power(Box<Expression>, Box<Expression>),
    /// `&` — bitwise AND.
    BitAnd(Box<Expression>, Box<Expression>),
    /// `|` — bitwise OR.
    BitOr(Box<Expression>, Box<Expression>),
    /// `#` — bitwise XOR.
    BitXor(Box<Expression>, Box<Expression>),
    /// `<<` — bitwise left shift.
    ShiftLeft(Box<Expression>, Box<Expression>),
    /// `>>` — bitwise right shift.
    ShiftRight(Box<Expression>, Box<Expression>),
    Cast {
        expr: Box<Expression>,
        type_name: String,
    },
    Extract {
        field: String,
        expr: Box<Expression>,
    },
    DateLiteral(String),
    /// `TIMESTAMP '...'` — PostgreSQL timestamp typed literal.
    TimestampLiteral(String),
    /// `TIMESTAMP(p) '...'` — PostgreSQL timestamp typed literal with
    /// fractional-second precision `p`. The executor truncates the parsed
    /// microseconds to `p` fractional digits.
    TypedTimestampLiteral {
        text: String,
        precision: u8,
    },
    /// `TIMESTAMPTZ '...'` — PostgreSQL shorthand for `TIMESTAMP WITH TIME ZONE`.
    /// Stored as a string literal that the executor parses as a timestamp with
    /// time zone. Mirrors `TimestampLiteral` so existing timestamp casting and
    /// JSON conversion paths apply.
    TimestamptzLiteral(String),
    /// `TIMESTAMPTZ(p) '...'` — PostgreSQL timestamp-with-time-zone typed literal
    /// with fractional-second precision `p`. The executor truncates the parsed
    /// microseconds to `p` fractional digits.
    TypedTimestamptzLiteral {
        text: String,
        precision: u8,
    },
    /// `TIME '...'` — PostgreSQL time typed literal.
    TimeLiteral(String),
    /// `TIME(p) '...'` — PostgreSQL time typed literal with fractional-second
    /// precision `p`. The executor truncates the parsed microseconds to `p`
    /// fractional digits.
    TypedTimeLiteral {
        text: String,
        precision: u8,
    },
    FunctionCall {
        name: String,
        args: Vec<Expression>,
        distinct: bool,
        filter: Option<Box<Expression>>,
        /// ORDER BY inside aggregate function call: agg(expr ORDER BY expr).
        order_by: Vec<OrderByItem>,
        /// `RETURNING <type>` clause for SQL/JSON constructors
        /// (JSON, JSON_ARRAY, JSON_OBJECT, JSON_SERIALIZE, ...).
        returning: Option<String>,
        /// `NULL ON NULL` / `ABSENT ON NULL` clause for SQL/JSON constructors
        /// (JSON_ARRAY, JSON_OBJECT, JSON_ARRAYAGG, JSON_OBJECTAGG).
        null_handling: Option<NullHandling>,
        /// `WITH UNIQUE KEYS` / `WITHOUT UNIQUE KEYS` for JSON_OBJECT.
        /// `Some(true)` = WITH UNIQUE KEYS, `Some(false)` = WITHOUT UNIQUE KEYS.
        unique_keys: Option<bool>,
    },
    /// `expr::type_name` — PostgreSQL-style type cast.
    TypeCast {
        expr: Box<Expression>,
        type_name: String,
    },
    /// `left -> right` (JSON) / `left ->> right` (JSON text).
    JsonArrow {
        left: Box<Expression>,
        right: Box<Expression>,
        as_text: bool,
    },
    /// `array[index]` — 1-based array element access.
    ArrayIndex {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    /// `(record).field` — composite/record attribute access, used by PostgreSQL
    /// catalog helpers such as `_pg_expandarray(...).x` returns rows.
    RowField {
        expr: Box<Expression>,
        field: String,
    },
    /// `jsonb['key']` / `jsonb[0]` — JSON subscripting.
    /// On json/jsonb this behaves like `->` for text keys and like `->>` for
    /// integer subscripts depending on runtime type. We model it as a function
    /// call in the parser for simplicity and dispatch in the executor.
    JsonSubscript {
        array: Box<Expression>,
        index: Box<Expression>,
    },
}

impl Expression {
    /// Collects the column references appearing in this expression, in order
    /// of first appearance, without descending into subqueries.
    pub fn column_refs(&self, out: &mut Vec<String>) {
        fn push_unique(out: &mut Vec<String>, name: String) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        match self {
            Expression::ColumnRef(name) => push_unique(out, name.clone()),
            Expression::Equal(a, b)
            | Expression::NotEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::And(a, b)
            | Expression::Or(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b)
            | Expression::Modulo(a, b)
            | Expression::Concat(a, b)
            | Expression::IsDistinctFrom(a, b)
            | Expression::Power(a, b)
            | Expression::BitAnd(a, b)
            | Expression::BitOr(a, b)
            | Expression::BitXor(a, b)
            | Expression::ShiftLeft(a, b)
            | Expression::ShiftRight(a, b)
            | Expression::JsonArrow {
                left: a, right: b, ..
            }
            | Expression::ArrayIndex { array: a, index: b } => {
                a.column_refs(out);
                b.column_refs(out);
            }
            Expression::RowField { expr, .. } => expr.column_refs(out),
            Expression::IsNull(a)
            | Expression::IsNotNull(a)
            | Expression::IsJson { expr: a, .. }
            | Expression::IsBoolean { expr: a, .. }
            | Expression::Not(a)
            | Expression::Negate(a)
            | Expression::NullIf(a, _) => a.column_refs(out),
            Expression::In { expr, list, .. } => {
                expr.column_refs(out);
                for item in list {
                    item.column_refs(out);
                }
            }
            Expression::Between {
                expr, low, high, ..
            } => {
                expr.column_refs(out);
                low.column_refs(out);
                high.column_refs(out);
            }
            Expression::Like { expr, pattern, .. } => {
                expr.column_refs(out);
                pattern.column_refs(out);
            }
            Expression::Case {
                operand,
                whens,
                default,
            } => {
                if let Some(operand) = operand {
                    operand.column_refs(out);
                }
                for (condition, value) in whens {
                    condition.column_refs(out);
                    value.column_refs(out);
                }
                if let Some(default) = default {
                    default.column_refs(out);
                }
            }
            Expression::Coalesce(args) => {
                for arg in args {
                    arg.column_refs(out);
                }
            }
            Expression::WindowFunction { args, over, .. } => {
                for arg in args {
                    arg.column_refs(out);
                }
                for expr in &over.partition_by {
                    expr.column_refs(out);
                }
                for item in &over.order_by {
                    item.expr.column_refs(out);
                }
                frame_column_refs(&over.frame, out);
            }
            Expression::Cast { expr, .. } | Expression::TypeCast { expr, .. } => {
                expr.column_refs(out);
            }
            Expression::Extract { expr, .. } => expr.column_refs(out),
            Expression::FunctionCall {
                args,
                filter,
                order_by,
                ..
            } => {
                for arg in args {
                    arg.column_refs(out);
                }
                // An aggregate's internal `ORDER BY` keys (`agg(expr ORDER BY
                // key)`) are evaluated over the very same scope as its
                // arguments, so they are column references too. Callers use
                // this list to decide which columns a statement reads, so
                // leaving these out would under-report the set.
                for item in order_by {
                    item.expr.column_refs(out);
                }
                if let Some(filter) = filter {
                    filter.column_refs(out);
                }
            }
            Expression::JsonSubscript { array, index } => {
                array.column_refs(out);
                index.column_refs(out);
            }
            Expression::Literal(_)
            | Expression::Star
            | Expression::DateLiteral(_)
            | Expression::TimestampLiteral(_)
            | Expression::TypedTimestampLiteral { .. }
            | Expression::TimestamptzLiteral(_)
            | Expression::TypedTimestamptzLiteral { .. }
            | Expression::TimeLiteral(_)
            | Expression::TypedTimeLiteral { .. } => {}
            // Subquery-bearing forms: column references inside the subquery
            // belong to the subquery's scope, not this one.
            Expression::Exists(_)
            | Expression::ScalarSubquery(_)
            | Expression::QuantifiedComparison { .. } => {}
        }
    }
}

/// Collects column references used by window frame offset expressions.
fn frame_column_refs(frame: &Option<FrameSpec>, out: &mut Vec<String>) {
    match frame {
        Some(FrameSpec::Rows { bounds })
        | Some(FrameSpec::Range { bounds })
        | Some(FrameSpec::Groups { bounds }) => {
            for bound in [&bounds.start, &bounds.end] {
                match bound {
                    FrameBound::Preceding { offset } | FrameBound::Following { offset } => {
                        offset.column_refs(out)
                    }
                    _ => {}
                }
            }
        }
        None => {}
    }
}

/// One bound of a window frame: where the evaluation window starts or ends
/// relative to the current row.
///
/// Each variant carries an offset **expression** (rather than a raw integer)
/// so that numeric offsets can be evaluated through the engine's normal
/// expression machinery and reuse its type system.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameBound {
    /// Everything from the first row of the partition.
    UnboundedPreceding,
    /// `N PRECEDING` — a number of rows/peers strictly before the current row.
    Preceding { offset: Box<Expression> },
    /// The current row.
    CurrentRow,
    /// `N FOLLOWING` — a number of rows/peers strictly after the current row.
    Following { offset: Box<Expression> },
    /// Everything to the last row of the partition.
    UnboundedFollowing,
}

/// An inclusive start/end pair of frame bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameBounds {
    pub start: FrameBound,
    pub end: FrameBound,
}

/// The evaluation frame attached to a window specification.
///
/// Frames are materialised on top of peer groups. `ROWS` moves by physical
/// row offsets, `RANGE` moves by peer-group ranges (offset in rows but peers
/// are kept together), and `GROUPS` moves in whole peer groups. Keeping a
/// dedicated variant for each prevents silently reinterpreting one kind as
/// another.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameSpec {
    Rows { bounds: FrameBounds },
    Range { bounds: FrameBounds },
    Groups { bounds: FrameBounds },
}

/// Window function specification for `OVER (...)` clauses.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowSpec {
    /// `PARTITION BY` expressions.
    pub partition_by: Vec<Expression>,
    /// `ORDER BY` items.
    pub order_by: Vec<OrderByItem>,
    /// Optional `ROWS`/`RANGE`/`GROUPS` frame. When `None`, the engine applies
    /// the PostgreSQL default frame: for specifications with an `ORDER BY`,
    /// `UNBOUNDED PRECEDING .. CURRENT ROW`; without an `ORDER BY`, the whole
    /// partition.
    pub frame: Option<FrameSpec>,
}

/// One `ORDER BY` key.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderByItem {
    pub expr: Expression,
    pub descending: bool,
    pub nulls_first: Option<bool>,
}

/// What to return from a SELECT projection.
///
/// `All` expands to every table column. Bare columns, literals, and arbitrary
/// expressions share the `Expr` variant, while function calls are kept
/// distinct so aggregates (and zero-argument session functions) can be
/// identified and evaluated with dedicated semantics. An explicit or implicit
/// alias wraps any target in `Aliased`.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectTarget {
    All,
    /// A qualified star (`table.*` or `alias.*`), expanded to all columns of
    /// the matching scope at projection time.
    QualifiedStar {
        qualifier: String,
    },
    Function(String),
    FunctionCall {
        name: String,
        args: Vec<Expression>,
    },
    WindowFunction {
        name: String,
        args: Vec<Expression>,
        over: WindowSpec,
    },
    Expr {
        expr: Expression,
        alias: Option<String>,
    },
    Aliased {
        target: Box<SelectTarget>,
        alias: String,
    },
}

impl SelectTarget {
    #[must_use]
    pub fn column(name: impl Into<String>) -> Self {
        Self::Expr {
            expr: Expression::ColumnRef(name.into()),
            alias: None,
        }
    }

    #[must_use]
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}
