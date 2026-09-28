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
use crate::{
    value_pg_type, ColumnDef, ColumnType, Constraint, ConstraintKind, Expression, Statement, Value,
};
use plomid_core::{ColumnId, ErrorKind, IndexId, PlomidError, Result, SchemaId, TableId};
use plomid_txn::StorageEngineTransaction;

// Deterministic name->OID and qualification helpers live in the type registry.
pub use plomid_types::{bare_type_name, custom_type_oid};
mod encoding;
mod view_types;

const CATALOG_KEY: &str = "__plomid_catalog_meta";

pub trait Catalog {
    fn create_schema(&mut self, name: String) -> Result<()>;
    fn has_schema(&self, name: &str) -> bool;
    /// Session `search_path` used for unqualified name resolution. The default
    /// implementation is stateless (`public`) for catalogs without session
    /// state; `InMemoryCatalog` overrides it with per-session storage.
    /// Named `session_search_path` to avoid clashing with the inherent
    /// `InMemoryCatalog::search_path()` accessor.
    fn session_search_path(&self) -> Vec<String> {
        vec!["public".to_string()]
    }
    fn set_session_search_path(&mut self, _path: Vec<String>) {}
    fn schema_names(&self) -> Vec<String>;
    fn create_table(
        &mut self,
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<Constraint>,
    ) -> Result<()>;
    fn set_constraints(&mut self, table: &str, constraints: Vec<Constraint>) -> Result<()>;
    fn constraints(&self, table: &str) -> Result<&[Constraint]>;
    fn create_sequence(&mut self, name: &str) -> Result<()>;
    fn has_sequence(&self, name: &str) -> bool;
    fn sequence_names(&self) -> Vec<String>;
    fn drop_sequence(&mut self, name: &str) -> Result<()>;
    // Index identity travels as one unit (name/table/columns/expression/
    // uniqueness/operator class); bundling would churn every catalog impl.
    #[allow(clippy::too_many_arguments)]
    fn create_index(
        &mut self,
        name: String,
        table: String,
        column: String,
        // Ordered indexed column list (`[column]` for single-column,
        // the full list for composite, empty for expression indexes).
        columns: Vec<String>,
        expression: Option<crate::Expression>,
        unique: bool,
        operator_class: Option<String>,
    ) -> Result<()>;
    /// Registers the internal backing index for a single-column `PRIMARY KEY`
    /// / `UNIQUE` constraint. Such an index enforces uniqueness but is not a
    /// user-created index object, so it is kept out of
    /// [`Catalog::indexes_for_table`].
    fn create_constraint_index(
        &mut self,
        name: String,
        table: String,
        column: String,
        unique: bool,
    ) -> Result<()>;
    /// Registers a composite (multi-column) `UNIQUE`/`PRIMARY KEY` backing
    /// index. A composite constraint is one tuple index, never one index per
    /// participating column.
    ///
    /// The default implementation refuses: only a catalog that can represent
    /// an ordered column list may back a composite constraint.
    fn create_composite_unique_index(
        &mut self,
        name: String,
        table: String,
        columns: Vec<String>,
    ) -> Result<()> {
        let _ = (name, table, columns);
        Err(PlomidError::new(
            ErrorKind::Unsupported,
            "this catalog cannot represent composite unique indexes",
        ))
    }
    fn drop_index(&mut self, name: &str) -> Result<IndexDefinition>;
    /// User-created indexes of `table`. Constraint backing indexes are not
    /// included; use [`Catalog::all_indexes_for_table`] for those.
    fn indexes_for_table(&self, table: &str) -> Vec<IndexDefinition>;
    /// Every index of `table`, including the internal indexes that back
    /// `PRIMARY KEY` / `UNIQUE` constraints.
    fn all_indexes_for_table(&self, table: &str) -> Vec<IndexDefinition>;
    fn get_table(&self, name: &str) -> Result<&TableSchema>;
    fn has_table(&self, name: &str) -> bool;
    fn table_names(&self) -> Vec<String>;
    fn drop_table(&mut self, name: &str) -> Result<TableSchema>;
    fn rename_table(&mut self, name: &str, new_name: String) -> Result<()>;
    fn rename_column(&mut self, table: &str, old_name: &str, new_name: String) -> Result<()>;
    fn add_column(&mut self, table: &str, column: ColumnDef) -> Result<()>;
    fn drop_column(&mut self, table: &str, column: &str) -> Result<ColumnId>;
    fn drop_schema(&mut self, name: &str) -> Result<()>;
    // Views.
    fn create_view(
        &mut self,
        name: String,
        columns: Vec<String>,
        query: Box<Statement>,
    ) -> Result<()>;
    fn drop_view(&mut self, name: &str) -> Result<()>;
    fn get_view(&self, name: &str) -> Option<&StoredView>;
    fn has_view(&self, name: &str) -> bool;
    fn view_names(&self) -> Vec<String>;
    // Types.
    fn create_type(&mut self, def: TypeDefinition) -> Result<()>;
    fn has_type(&self, name: &str) -> bool;
    fn drop_type(&mut self, name: &str) -> Result<()>;
    /// Names of all user-defined types stored in this catalog.
    fn type_names(&self) -> Vec<String>;
    /// Returns the stored definition for a user-defined type, tolerating
    /// schema qualification and case differences like `user_type_oid` does.
    fn get_type(&self, name: &str) -> Option<StoredType> {
        let _ = name;
        None
    }
    /// Names of all user-defined domains stored in this catalog.
    fn domain_names(&self) -> Vec<String>;
    /// Resolves a user-defined type or domain name to its synthetic type OID
    /// so column definitions can reference `CREATE TYPE`/`CREATE DOMAIN`
    /// objects, matching PostgreSQL's type resolution for columns.
    fn user_type_oid(&self, name: &str) -> Option<u32> {
        let _ = name;
        None
    }
    // Domains.
    fn create_domain(&mut self, def: DomainDefinition) -> Result<()>;
    fn has_domain(&self, name: &str) -> bool;
    fn drop_domain(&mut self, name: &str) -> Result<()>;
    // Functions.
    fn create_function(&mut self, def: FunctionDefinition) -> Result<()>;
    fn has_function(&self, name: &str, args: &[String]) -> bool;
    fn drop_function(&mut self, name: &str, args: &[String]) -> Result<()>;
    /// Finds a user-defined SQL function by (case-insensitive) name and
    /// argument count, used for call-site resolution.
    fn find_function_by_call(&self, name: &str, arg_count: usize) -> Option<StoredFunction> {
        let _ = (name, arg_count);
        None
    }
    // Dependency tracking.
    fn find_table_dependency(&self, name: &str) -> Option<String>;
    fn find_view_dependency(&self, name: &str) -> Option<String>;
    fn find_type_dependency(&self, name: &str) -> Option<String>;
    fn find_domain_dependency(&self, name: &str) -> Option<String>;
    fn find_function_dependency(&self, name: &str, args: &[String]) -> Option<String>;
    fn find_column_dependency(&self, table: &str, column: &str) -> Option<String>;
    fn view_depends_on_table(&self, view: &str, table: &str) -> bool;
    fn find_views_referencing_column(&self, table: &str, column: &str) -> Vec<String>;
}

/// Per-column constraint view used by the executor. The catalog stores the
/// authoritative table-level representation; this is only a derived view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnRule {
    pub not_null: bool,
    pub unique: bool,
    pub primary_key: bool,
    pub default_value: Option<Value>,
    /// Non-literal DEFAULT expression (e.g. `DEFAULT CURRENT_TIMESTAMP`).
    /// Evaluated per inserted row against the statement execution context.
    pub default_expr: Option<crate::Expression>,
    /// `GENERATED ALWAYS AS (expr) STORED` generation expression. Evaluated
    /// against the fully-built row on every INSERT/UPDATE, overriding any
    /// supplied value.
    pub generated_expr: Option<crate::Expression>,
    pub min_integer: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableSchema {
    pub name: String,
    pub table_id: TableId,
    pub column_ids: Vec<ColumnId>,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexDefinition {
    pub name: String,
    pub table: String,
    /// The index's first indexed column. Kept as the canonical single-column
    /// name so every single-column access path (`WHERE col = literal`
    /// discovery, statistics, system catalogs) stays unchanged. For a
    /// composite index this is `columns[0]`; empty for expression indexes.
    pub column: String,
    /// The ordered indexed column list. Exactly one entry for a single-column
    /// index, the constraint's column order for a composite
    /// `UNIQUE (a, b)` / `PRIMARY KEY (a, b)`, and empty for an expression
    /// index. The tuple is the conflict domain: `(a, b)` is unique as a
    /// whole, never `a` and `b` independently.
    pub columns: Vec<String>,
    /// For expression indexes, the index key expression as a parsed AST.
    /// `None` for plain column indexes.
    pub expression: Option<crate::Expression>,
    pub unique: bool,
    pub index_id: IndexId,
    /// PostgreSQL GIN operator class (e.g. `jsonb_path_ops`). This metadata
    /// is distinct from the indexed column/expression: it tells the index
    /// infrastructure how to build the key, not what to index.
    ///
    /// We preserve it as optional metadata rather than silently dropping it
    /// so that future GIN-aware planning can honor the operator class.
    /// For now, PLOMID treats all indexes as B-tree-style unless the
    /// executor explicitly supports GIN.
    pub operator_class: Option<String>,
    /// True when this index exists only to enforce a `PRIMARY KEY` / `UNIQUE`
    /// constraint (`register_constraint_indexes`). It is the durable
    /// uniqueness authority, but it is *not* a user-created index object: the
    /// planner, statistics, derived ART, and columnar index generations only
    /// consider user indexes, and the system catalog presents the constraint's
    /// index from the table's constraint metadata instead.
    pub constraint: bool,
}

/// A stored view: its optional column alias list and defining query.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredView {
    pub name: String,
    pub columns: Vec<String>,
    pub query: Box<Statement>,
    /// Canonical SQL used to restore the defining query after restart.
    pub definition: String,
    /// Resolved output type of each SELECT target, aligned with `columns`.
    ///
    /// Computed once at `CREATE VIEW` time from the defining query so that
    /// `information_schema.columns` / `pg_attribute` report the real view
    /// column types (not `TEXT`). Entries may be `None` when the type could
    /// not be statically resolved; consumers must fall back to `TEXT`.
    /// Empty for views persisted before this metadata existed (or when the
    /// definition has more/fewer columns than types due to `SELECT *`).
    pub column_types: Vec<Option<ColumnType>>,
}

impl StoredView {
    /// Resolved output type for the view column at `index`, if known.
    #[must_use]
    pub fn column_type(&self, index: usize) -> Option<ColumnType> {
        self.column_types.get(index).copied().flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleDefinition {
    pub name: String,
    pub superuser: bool,
    pub inherit: bool,
    pub create_role: bool,
    pub create_database: bool,
    pub can_login: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    pub connection_limit: i32,
    pub password: Option<String>,
    pub members: Vec<String>,
}

impl TableSchema {
    pub fn column_index(&self, name: &str) -> Result<usize> {
        self.columns
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| {
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    format!(
                        "column \"{name}\" does not exist in table \"{}\"",
                        self.name
                    ),
                    format!("table={} column={name}", self.name),
                )
            })
    }

    /// True when the column at `index` may not hold NULL.
    pub fn is_not_null(&self, index: usize) -> bool {
        self.constraints
            .iter()
            .chain(
                self.columns
                    .iter()
                    .flat_map(|column| column.constraints.iter()),
            )
            .any(|constraint| {
                matches!(
                    constraint.kind,
                    ConstraintKind::NotNull | ConstraintKind::PrimaryKey
                ) && constraint.columns.contains(&self.columns[index].name)
            })
    }

    /// True when the column at `index` participates in a UNIQUE or PRIMARY KEY
    /// constraint (either column-level or table-level).
    pub fn column_is_unique(&self, index: usize) -> bool {
        self.constraints
            .iter()
            .chain(
                self.columns
                    .iter()
                    .flat_map(|column| column.constraints.iter()),
            )
            .any(|constraint| {
                // Only a *single-column* UNIQUE/PK makes its column
                // individually unique. A composite constraint's conflict
                // domain is the whole tuple, so none of its columns is
                // individually unique here.
                matches!(
                    constraint.kind,
                    ConstraintKind::Unique | ConstraintKind::PrimaryKey
                ) && constraint.columns == [self.columns[index].name.clone()]
            })
    }

    /// The CHECK constraints that apply to this table.
    pub fn check_constraints(&self) -> Vec<&Constraint> {
        self.constraints
            .iter()
            .filter(|constraint| constraint.kind == ConstraintKind::Check)
            .collect()
    }

    /// The FOREIGN KEY constraints declared on this table.
    pub fn foreign_keys(&self) -> Vec<&Constraint> {
        self.constraints
            .iter()
            .filter(|c| matches!(c.kind, ConstraintKind::ForeignKey { .. }))
            .collect()
    }

    /// The DEFAULT expression for the column at `index`, if declared.
    pub fn default_expr(&self, index: usize) -> Option<&crate::Expression> {
        self.constraints
            .iter()
            .chain(
                self.columns
                    .iter()
                    .flat_map(|column| column.constraints.iter()),
            )
            .find_map(|constraint| {
                if constraint.kind == ConstraintKind::Default
                    && constraint.columns.contains(&self.columns[index].name)
                {
                    constraint.expr.as_ref()
                } else {
                    None
                }
            })
    }

    /// Returns the tuples of column indexes that form each UNIQUE/PRIMARY KEY
    /// constraint.
    pub fn unique_column_sets(&self) -> Vec<Vec<usize>> {
        self.constraints
            .iter()
            .filter(|constraint| {
                matches!(
                    constraint.kind,
                    ConstraintKind::Unique | ConstraintKind::PrimaryKey
                )
            })
            .map(|constraint| {
                constraint
                    .columns
                    .iter()
                    .filter_map(|name| self.columns.iter().position(|col| col.name == *name))
                    .collect()
            })
            .filter(|set: &Vec<usize>| !set.is_empty())
            .collect()
    }

    pub fn validate_row(&self, values: &[crate::Value]) -> Result<()> {
        if values.len() != self.columns.len() {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "column count mismatch: expected {}, got {}",
                    self.columns.len(),
                    values.len()
                ),
            ));
        }
        for (col, value) in self.columns.iter().zip(values.iter()) {
            if !value_matches_column(&col.col_type, value) {
                let expected = col
                    .col_type
                    .pg_type()
                    .map(|ty| ty.name().to_string())
                    .unwrap_or_else(|| format!("oid {}", col.col_type.type_oid));
                return Err(PlomidError::with_detail(
                    ErrorKind::InvalidArgument,
                    format!(
                        "column \"{}\" expects {} but received {}",
                        col.name,
                        expected,
                        plomid_types::format_text(value),
                    ),
                    format!(
                        "table={} column={} expected_type={} actual_type={}",
                        self.name,
                        col.name,
                        expected,
                        crate::value_pg_type(value)
                            .map(|ty| ty.name().to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// A stored user-defined type (e.g. enum, composite).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredType {
    pub name: String,
    pub labels: Vec<String>,
    /// Composite type attributes: [(column_name, type_name), ...].
    /// Empty for enums; for composites, the list of (column_name, type_name) pairs.
    pub attributes: Vec<(String, String)>,
}

/// A stored domain definition.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDomain {
    pub name: String,
    pub base_type: String,
    pub constraints: Vec<StoredDomainConstraint>,
}

/// A domain constraint.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDomainConstraint {
    pub name: Option<String>,
    pub check: String,
}

/// A stored function definition.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFunction {
    pub name: String,
    pub args: Vec<StoredFunctionArg>,
    pub returns: String,
    pub language: String,
    pub body: String,
}

/// A function argument.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFunctionArg {
    pub name: String,
    pub data_type: String,
}

/// Public type definition for `create_type`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDefinition {
    pub name: String,
    pub labels: Vec<String>,
    /// Composite type attributes: [(column_name, type_name), ...].
    /// Empty for enums; `Some([])` for composites without columns.
    pub attributes: Vec<(String, String)>,
}

/// Public domain definition for `create_domain`.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainDefinition {
    pub name: String,
    pub base_type: String,
    pub constraints: Vec<DomainConstraintDefinition>,
}

/// Public domain constraint definition.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainConstraintDefinition {
    pub name: Option<String>,
    pub check: String,
}

/// Public function definition for `create_function`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDefinition {
    pub name: String,
    pub args: Vec<FunctionArgDefinition>,
    pub returns: String,
    pub language: String,
    pub body: String,
}

/// Public function argument definition.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionArgDefinition {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryCatalog {
    pub(crate) tables: std::collections::HashMap<String, TableSchema>,
    pub(crate) schemas: std::collections::HashMap<String, SchemaId>,
    pub(crate) next_schema_id: u64,
    pub(crate) next_table_id: u64,
    pub(crate) next_column_id: u64,
    pub(crate) sequences: std::collections::BTreeMap<String, u64>,
    pub(crate) indexes: std::collections::HashMap<String, IndexDefinition>,
    pub(crate) next_index_id: u64,
    pub(crate) views: std::collections::HashMap<String, StoredView>,
    pub(crate) roles: std::collections::BTreeMap<String, RoleDefinition>,
    pub(crate) database_names: Vec<String>,
    /// Connection-local relation lookup path. This is deliberately not
    /// persisted: the catalog is shared database metadata, while search_path
    /// belongs to the PostgreSQL session.
    pub(crate) search_path: Vec<String>,
    pub(crate) types: std::collections::HashMap<String, StoredType>,
    pub(crate) domains: std::collections::HashMap<String, StoredDomain>,
    pub(crate) functions: std::collections::HashMap<String, StoredFunction>,
}

impl InMemoryCatalog {
    pub fn new() -> Self {
        let mut catalog = Self::default();
        catalog
            .schemas
            .insert("public".to_string(), SchemaId::new(1));
        catalog.next_schema_id = 2;
        catalog.next_table_id = 1;
        catalog.next_column_id = 1;
        catalog.next_index_id = 1;
        catalog.views = std::collections::HashMap::new();
        catalog.search_path = vec!["public".to_string()];
        catalog.ensure_role("plomid");
        catalog.database_names.push("plomid".to_string());
        catalog
    }

    pub fn set_database_names(&mut self, names: Vec<String>) {
        self.database_names = names;
    }

    pub fn database_names(&self) -> &[String] {
        &self.database_names
    }

    pub fn set_search_path(&mut self, path: Vec<String>) {
        self.search_path = if path.is_empty() {
            vec!["public".to_string()]
        } else {
            path
        };
    }

    pub fn search_path(&self) -> &[String] {
        &self.search_path
    }

    fn relation_candidates(&self, name: &str) -> Vec<String> {
        if name.contains('.') {
            return vec![name.to_string()];
        }
        self.search_path
            .iter()
            .filter(|schema| !schema.is_empty() && *schema != "$user")
            .map(|schema| format!("{schema}.{name}"))
            .collect()
    }

    /// Resolves a relation reference (qualified or unqualified) to its stored
    /// catalog identity, honoring the session `search_path` for unqualified
    /// names. Errors when the table does not exist.
    pub fn resolve_table_name(&self, name: &str) -> Result<String> {
        self.get_table(name).map(|schema| schema.name.clone())
    }

    /// Resolves a relation reference for lookup/mutation. Unlike
    /// [`resolve_table_name`], existence is not required: the resolved key is
    /// returned even when the target does not exist yet (e.g. to build the
    /// namespace for a `RENAME`). Unqualified names are mapped through the
    /// first existing schema in the search path.
    pub fn resolve_table_namespace(&self, name: &str) -> String {
        if name.contains('.') {
            let (schema, rest) = name.split_once('.').unwrap_or(("public", name));
            if self.schemas.contains_key(schema) {
                return name.to_string();
            }
            return format!("{schema}.{rest}");
        }
        match self.resolve_create_schema() {
            Some(schema) => format!("{schema}.{name}"),
            None => name.to_string(),
        }
    }

    /// Resolves the schema in which an unqualified `CREATE TABLE`/relation
    /// should be placed. PostgreSQL creates the object in the first existing
    /// schema of the session `search_path`; it never auto-creates a schema.
    fn resolve_create_schema(&self) -> Option<String> {
        self.search_path
            .iter()
            .find(|schema| {
                !schema.is_empty() && *schema != "$user" && self.schemas.contains_key(*schema)
            })
            .cloned()
    }

    /// Qualifies a relation name for creation. Explicitly schema-qualified
    /// names are validated and returned unchanged; unqualified names are
    /// prefixed with the first existing schema in the session search path
    /// (PostgreSQL `CREATE TABLE` semantics).
    pub fn resolve_create_name(&self, name: &str) -> Result<String> {
        if name.contains('.') {
            let schema = name.split('.').next().unwrap_or("");
            if !self.schemas.contains_key(schema) {
                return Err(PlomidError::new(
                    ErrorKind::NotFound,
                    format!("schema \"{schema}\" does not exist"),
                ));
            }
            return Ok(name.to_string());
        }
        match self.resolve_create_schema() {
            Some(schema) => Ok(format!("{schema}.{name}")),
            None => Err(PlomidError::new(
                ErrorKind::NotFound,
                "no schema has been selected to create in",
            )),
        }
    }

    pub fn ensure_role(&mut self, name: &str) {
        self.roles
            .entry(name.to_string())
            .or_insert_with(|| RoleDefinition {
                name: name.to_string(),
                superuser: true,
                inherit: true,
                create_role: true,
                create_database: true,
                can_login: true,
                replication: false,
                bypass_rls: true,
                connection_limit: -1,
                password: None,
                members: Vec::new(),
            });
    }

    pub fn roles(&self) -> Vec<&RoleDefinition> {
        self.roles.values().collect()
    }

    pub fn create_role(&mut self, role: RoleDefinition) -> Result<()> {
        if self.roles.contains_key(&role.name) {
            return Err(PlomidError::new(
                ErrorKind::AlreadyExists,
                format!("role \"{}\" already exists", role.name),
            ));
        }
        self.roles.insert(role.name.clone(), role);
        Ok(())
    }

    pub fn grant_role(&mut self, role: &str, member: &str) -> Result<()> {
        if !self.roles.contains_key(role) {
            return Err(PlomidError::new(
                ErrorKind::NotFound,
                format!("role \"{role}\" does not exist"),
            ));
        }
        let member_role = self.roles.get_mut(member).ok_or_else(|| {
            PlomidError::new(
                ErrorKind::NotFound,
                format!("role \"{member}\" does not exist"),
            )
        })?;
        if !member_role.members.iter().any(|name| name == role) {
            member_role.members.push(role.to_string());
        }
        Ok(())
    }

    pub fn schema_id(&self, name: &str) -> Option<SchemaId> {
        self.schemas.get(name).copied()
    }

    pub fn tables(&self) -> Vec<&TableSchema> {
        let mut tables: Vec<&TableSchema> = self.tables.values().collect();
        tables.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        tables
    }

    pub fn indexes(&self) -> Vec<IndexDefinition> {
        let mut indexes: Vec<IndexDefinition> = self.indexes.values().cloned().collect();
        indexes.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        indexes
    }

    pub fn index(&self, name: &str) -> Option<IndexDefinition> {
        self.indexes.get(name).cloned()
    }

    pub fn encode(&self) -> Vec<u8> {
        encoding::encode(self)
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Result<()> {
        encoding::decode(self, bytes)
    }

    pub fn column_rules(&self, table: &str) -> Result<Vec<ColumnRule>> {
        let schema = self.get_table(table)?;
        let mut rules = vec![ColumnRule::default(); schema.columns.len()];
        let all_constraints = schema
            .constraints
            .iter()
            .chain(schema.columns.iter().flat_map(|col| col.constraints.iter()))
            .collect::<Vec<_>>();

        for constraint in all_constraints {
            for name in &constraint.columns {
                if let Some(index) = schema.columns.iter().position(|c| c.name == *name) {
                    match constraint.kind {
                        ConstraintKind::NotNull => {
                            rules[index].not_null = true;
                        }
                        ConstraintKind::PrimaryKey => {
                            // The tuple is the key. A composite PRIMARY KEY
                            // keys on `(a, b)` as a whole, so no participating
                            // column is individually unique; every member is
                            // still NOT NULL.
                            rules[index].not_null = true;
                            if constraint.columns.len() == 1 {
                                rules[index].primary_key = true;
                            }
                        }
                        ConstraintKind::Unique => {
                            // Same rule for UNIQUE: `UNIQUE (a, b)` must not be
                            // degraded into `UNIQUE (a)` + `UNIQUE (b)`.
                            if constraint.columns.len() == 1 {
                                rules[index].unique = true;
                            }
                        }
                        ConstraintKind::ForeignKey { .. } => {}
                        ConstraintKind::Default => match constraint.expr.as_ref() {
                            Some(Expression::Literal(value)) => {
                                rules[index].default_value = Some(value.clone());
                            }
                            Some(expr) => {
                                rules[index].default_expr = Some(expr.clone());
                            }
                            None => {}
                        },
                        ConstraintKind::Check => {
                            if let Some(expr) = constraint.expr.as_ref() {
                                if let Some(min) = extract_min_integer(name, expr) {
                                    rules[index].min_integer = Some(min);
                                }
                            }
                        }
                        ConstraintKind::GeneratedAlways => {
                            rules[index].generated_expr = constraint.expr.clone();
                        }
                    }
                }
            }
        }
        Ok(rules)
    }

    /// Finds the first table column whose type is the user-defined type or
    /// domain `name` (OID-based, matching how columns resolve custom types).
    fn find_custom_type_dependency(&self, name: &str, domains: bool) -> Option<String> {
        let registered = if domains {
            self.domains
                .keys()
                .any(|stored| type_name_matches(stored, name))
        } else {
            self.types
                .keys()
                .any(|stored| type_name_matches(stored, name))
        };
        if !registered {
            return None;
        }
        let oid = custom_type_oid(name);
        for (table_name, table) in &self.tables {
            for col in &table.columns {
                if col.col_type.type_oid.raw() == oid {
                    return Some(format!("table \"{table_name}\" column \"{}\"", col.name));
                }
            }
        }
        None
    }
}

impl Catalog for InMemoryCatalog {
    fn session_search_path(&self) -> Vec<String> {
        self.search_path.clone()
    }

    fn set_session_search_path(&mut self, path: Vec<String>) {
        self.set_search_path(path);
    }

    fn create_schema(&mut self, name: String) -> Result<()> {
        if self.schemas.contains_key(&name) {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("schema \"{}\" already exists", name),
                format!("schema={}", name),
            ));
        }
        let id = SchemaId::new(self.next_schema_id);
        self.schemas.insert(name, id);
        self.next_schema_id += 1;
        Ok(())
    }

    fn has_schema(&self, name: &str) -> bool {
        self.schemas.contains_key(name)
    }

    fn schema_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.schemas.keys().cloned().collect();
        names.sort_unstable();
        names
    }

    fn create_table(
        &mut self,
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<Constraint>,
    ) -> Result<()> {
        if self.tables.contains_key(&name) {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("table \"{}\" already exists", name),
                format!("table={}", name),
            ));
        }
        let table_id = TableId::new(self.next_table_id);
        self.next_table_id += 1;

        let mut column_ids = Vec::with_capacity(columns.len());
        for _ in 0..columns.len() {
            column_ids.push(ColumnId::new(self.next_column_id));
            self.next_column_id += 1;
        }

        let table = TableSchema {
            name: name.clone(),
            table_id,
            column_ids,
            columns,
            constraints,
        };
        self.tables.insert(name, table);
        Ok(())
    }

    fn set_constraints(&mut self, table: &str, constraints: Vec<Constraint>) -> Result<()> {
        let table = self.resolve_table_name(table)?;
        let table_schema = self.tables.get_mut(&table).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{}\" not found", table),
                format!("table={}", table),
            )
        })?;
        table_schema.constraints = constraints;
        Ok(())
    }

    fn constraints(&self, table: &str) -> Result<&[Constraint]> {
        let table = self.resolve_table_name(table)?;
        self.tables
            .get(&table)
            .map(|ts| ts.constraints.as_slice())
            .ok_or_else(|| {
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    format!("table \"{}\" not found", table),
                    format!("table={}", table),
                )
            })
    }

    fn create_sequence(&mut self, name: &str) -> Result<()> {
        if self.sequences.contains_key(name) {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("sequence \"{}\" already exists", name),
                format!("sequence={}", name),
            ));
        }
        self.sequences.insert(name.to_string(), 0);
        Ok(())
    }

    fn has_sequence(&self, name: &str) -> bool {
        self.sequences.contains_key(name)
    }

    fn sequence_names(&self) -> Vec<String> {
        self.sequences.keys().cloned().collect()
    }

    fn drop_sequence(&mut self, name: &str) -> Result<()> {
        if !self.sequences.contains_key(name) {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("sequence \"{}\" not found", name),
                format!("sequence={}", name),
            ));
        }
        self.sequences.remove(name);
        Ok(())
    }

    fn create_index(
        &mut self,
        name: String,
        table: String,
        column: String,
        columns: Vec<String>,
        expression: Option<crate::Expression>,
        unique: bool,
        operator_class: Option<String>,
    ) -> Result<()> {
        if self.indexes.contains_key(&name) {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("index \"{}\" already exists", name),
                format!("index={}", name),
            ));
        }
        // Normalize the ordered column list: `[column]` for single-column,
        // the full list for composite, empty for expression indexes. Reject
        // unknown columns up front rather than registering an index that can
        // never be maintained.
        let schema = self.get_table(&table)?.clone();
        let columns = if expression.is_none() {
            let columns = if columns.is_empty() {
                vec![column.clone()]
            } else {
                columns
            };
            if columns.is_empty() {
                return Err(PlomidError::new(
                    ErrorKind::InvalidArgument,
                    "an index needs at least one column",
                ));
            }
            for indexed in &columns {
                schema.column_index(indexed)?;
            }
            columns
        } else {
            Vec::new()
        };
        let column = columns.first().cloned().unwrap_or(column);
        let index_id = IndexId::new(self.next_index_id);
        self.next_index_id += 1;

        let index = IndexDefinition {
            name: name.clone(),
            table,
            columns,
            column,
            expression,
            unique,
            index_id,
            operator_class,
            constraint: false,
        };
        self.indexes.insert(name, index);
        Ok(())
    }

    /// Registers the internal backing index for a single-column `PRIMARY KEY`
    /// / `UNIQUE` constraint. Unlike [`Catalog::create_index`] this marks the
    /// index as constraint-owned so it stays out of the derived index views.
    fn create_constraint_index(
        &mut self,
        name: String,
        table: String,
        column: String,
        unique: bool,
    ) -> Result<()> {
        if self.indexes.contains_key(&name) {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("index \"{}\" already exists", name),
                format!("index={}", name),
            ));
        }
        let index_id = IndexId::new(self.next_index_id);
        self.next_index_id += 1;
        let index = IndexDefinition {
            name: name.clone(),
            table,
            column: column.clone(),
            columns: vec![column],
            expression: None,
            unique,
            index_id,
            operator_class: None,
            constraint: true,
        };
        self.indexes.insert(name, index);
        Ok(())
    }

    fn create_composite_unique_index(
        &mut self,
        name: String,
        table: String,
        columns: Vec<String>,
    ) -> Result<()> {
        if columns.len() < 2 {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "a composite index needs at least two columns",
            ));
        }
        if self.indexes.contains_key(&name) {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("index \"{}\" already exists", name),
                format!("index={}", name),
            ));
        }
        // Reject an unknown column rather than registering an index that can
        // never be maintained (its key values would not be computable).
        let schema = self.get_table(&table)?;
        for column in &columns {
            schema.column_index(column)?;
        }
        let index_id = IndexId::new(self.next_index_id);
        self.next_index_id += 1;
        let index = IndexDefinition {
            name: name.clone(),
            table,
            column: columns[0].clone(),
            columns,
            expression: None,
            unique: true,
            index_id,
            operator_class: None,
            constraint: true,
        };
        self.indexes.insert(name, index);
        Ok(())
    }

    fn drop_index(&mut self, name: &str) -> Result<IndexDefinition> {
        self.indexes.remove(name).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("index \"{}\" not found", name),
                format!("index={}", name),
            )
        })
    }

    fn indexes_for_table(&self, table: &str) -> Vec<IndexDefinition> {
        self.indexes
            .values()
            .filter(|idx| idx.table == table && !idx.constraint)
            .cloned()
            .collect()
    }

    fn all_indexes_for_table(&self, table: &str) -> Vec<IndexDefinition> {
        self.indexes
            .values()
            .filter(|idx| idx.table == table)
            .cloned()
            .collect()
    }

    fn create_view(
        &mut self,
        name: String,
        mut columns: Vec<String>,
        query: Box<Statement>,
    ) -> Result<()> {
        if self.views.contains_key(&name)
            || self.tables.contains_key(&name)
            || self.get_table(&name).is_ok()
        {
            return Err(PlomidError::with_detail(
                ErrorKind::AlreadyExists,
                format!("relation \"{name}\" already exists"),
                format!("view={name}"),
            ));
        }
        if columns.is_empty() {
            columns = infer_view_columns(&query);
        }
        // `SELECT *` views: the legacy name inference yields no names.
        // Expand to the real output names so stored columns/types stay aligned.
        if columns.is_empty() {
            let names = view_types::infer_query_output_names(self, &query);
            if !names.is_empty() {
                columns = names;
            }
        }
        let mut column_types = view_types::infer_query_output_types(self, &query);
        if column_types.len() != columns.len() {
            if column_types.len() > columns.len() {
                column_types.truncate(columns.len());
            } else {
                while column_types.len() < columns.len() {
                    column_types.push(None);
                }
            }
        }
        let definition = render_view_definition(&query);
        self.views.insert(
            name.clone(),
            StoredView {
                name,
                columns,
                query,
                definition,
                column_types,
            },
        );
        Ok(())
    }

    fn drop_view(&mut self, name: &str) -> Result<()> {
        // Resolve the view name through the session search_path so that
        // unqualified names match the stored schema-qualified key, matching
        // the lookup behavior of `get_view` and `has_view`.
        let name = match self.get_view(name) {
            Some(_) => self
                .relation_candidates(name)
                .iter()
                .find(|candidate| self.views.contains_key(candidate.as_str()))
                .cloned()
                .unwrap_or_else(|| name.to_string()),
            None => name.to_string(),
        };
        self.views
            .remove(&name)
            .ok_or_else(|| {
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    format!("view \"{name}\" does not exist"),
                    format!("view={name}"),
                )
            })
            .map(|_| ())
    }

    fn get_view(&self, name: &str) -> Option<&StoredView> {
        self.views.get(name).or_else(|| {
            self.relation_candidates(name)
                .iter()
                .find_map(|candidate| self.views.get(candidate))
        })
    }

    fn has_view(&self, name: &str) -> bool {
        self.get_view(name).is_some()
    }

    fn view_names(&self) -> Vec<String> {
        self.views.keys().cloned().collect()
    }

    fn get_table(&self, name: &str) -> Result<&TableSchema> {
        self.tables
            .get(name)
            .or_else(|| {
                self.relation_candidates(name)
                    .iter()
                    .find_map(|candidate| self.tables.get(candidate))
            })
            .ok_or_else(|| {
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    format!("table \"{}\" not found", name),
                    format!("table={}", name),
                )
            })
    }

    fn has_table(&self, name: &str) -> bool {
        self.tables.contains_key(name) || self.get_table(name).is_ok()
    }

    fn table_names(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }

    fn drop_table(&mut self, name: &str) -> Result<TableSchema> {
        let name = self.resolve_table_name(name)?;
        self.tables.remove(&name).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{}\" not found", name),
                format!("table={}", name),
            )
        })
    }

    fn rename_table(&mut self, name: &str, new_name: String) -> Result<()> {
        // Resolve the source table and keep the renamed table in the same schema.
        let name = self.resolve_table_name(name)?;
        let (schema, _) = name.split_once('.').unwrap_or(("public", &name));
        let resolved_new = if new_name.contains('.') {
            new_name.clone()
        } else {
            format!("{schema}.{new_name}")
        };
        let mut table_schema = self.tables.remove(&name).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{}\" not found", name),
                format!("table={}", name),
            )
        })?;
        table_schema.name = resolved_new.clone();
        self.tables.insert(resolved_new, table_schema);
        Ok(())
    }

    fn rename_column(&mut self, table: &str, old_name: &str, new_name: String) -> Result<()> {
        let table = self.resolve_table_name(table)?;
        let table_schema = self.tables.get_mut(&table).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{}\" not found", table),
                format!("table={}", table),
            )
        })?;
        let col_index = table_schema
            .columns
            .iter()
            .position(|c| c.name == old_name)
            .ok_or_else(|| {
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    format!("column \"{}\" not found in table \"{}\"", old_name, table),
                    format!("table={} column={}", table, old_name),
                )
            })?;
        table_schema.columns[col_index].name = new_name.clone();
        for constraint in &mut table_schema.constraints {
            if let Some(pos) = constraint.columns.iter().position(|c| c == old_name) {
                constraint.columns[pos] = new_name.clone();
            }
        }
        Ok(())
    }

    fn add_column(&mut self, table: &str, column: ColumnDef) -> Result<()> {
        let table = self.resolve_table_name(table)?;
        let table_schema = self.tables.get_mut(&table).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{}\" not found", table),
                format!("table={}", table),
            )
        })?;
        let column_id = ColumnId::new(self.next_column_id);
        self.next_column_id += 1;
        table_schema.columns.push(column);
        table_schema.column_ids.push(column_id);
        Ok(())
    }

    fn drop_column(&mut self, table: &str, column: &str) -> Result<ColumnId> {
        let table = self.resolve_table_name(table)?;
        let table_schema = self.tables.get_mut(&table).ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("table \"{}\" not found", table),
                format!("table={}", table),
            )
        })?;
        let col_index = table_schema
            .columns
            .iter()
            .position(|c| c.name == column)
            .ok_or_else(|| {
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    format!("column \"{}\" not found in table \"{}\"", column, table),
                    format!("table={} column={}", table, column),
                )
            })?;
        let column_id = table_schema.column_ids.remove(col_index);
        table_schema.columns.remove(col_index);
        for constraint in &mut table_schema.constraints {
            constraint.columns.retain(|c| c != column);
        }
        Ok(column_id)
    }

    fn drop_schema(&mut self, name: &str) -> Result<()> {
        if self.schemas.remove(name).is_none() {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("schema \"{}\" not found", name),
                format!("schema={}", name),
            ));
        }
        Ok(())
    }

    fn create_type(&mut self, def: TypeDefinition) -> Result<()> {
        if self.types.contains_key(&def.name) {
            return Err(PlomidError::new(
                ErrorKind::AlreadyExists,
                format!("type \"{}\" already exists", def.name),
            ));
        }
        self.types.insert(
            def.name.clone(),
            StoredType {
                name: def.name,
                labels: def.labels,
                attributes: def.attributes,
            },
        );
        Ok(())
    }

    fn has_type(&self, name: &str) -> bool {
        self.get_type(name).is_some()
    }

    fn get_type(&self, name: &str) -> Option<StoredType> {
        if let Some(found) = self.types.get(name) {
            return Some(found.clone());
        }
        self.types
            .iter()
            .find(|(stored, _)| type_name_matches(stored, name))
            .map(|(_, stored)| stored.clone())
    }

    fn type_names(&self) -> Vec<String> {
        self.types.keys().cloned().collect()
    }

    fn domain_names(&self) -> Vec<String> {
        self.domains.keys().cloned().collect()
    }

    fn user_type_oid(&self, name: &str) -> Option<u32> {
        let matches = |stored: &String| type_name_matches(stored, name);
        if self.types.keys().any(matches) || self.domains.keys().any(matches) {
            Some(custom_type_oid(name))
        } else {
            None
        }
    }

    fn find_function_by_call(&self, name: &str, arg_count: usize) -> Option<StoredFunction> {
        self.functions
            .values()
            .find(|func| func.args.len() == arg_count && type_name_matches(&func.name, name))
            .cloned()
    }

    fn drop_type(&mut self, name: &str) -> Result<()> {
        if self.types.remove(name).is_none() {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("type \"{}\" not found", name),
                format!("type={}", name),
            ));
        }
        Ok(())
    }

    fn create_domain(&mut self, def: DomainDefinition) -> Result<()> {
        if self.domains.contains_key(&def.name) {
            return Err(PlomidError::new(
                ErrorKind::AlreadyExists,
                format!("domain \"{}\" already exists", def.name),
            ));
        }
        self.domains.insert(
            def.name.clone(),
            StoredDomain {
                name: def.name,
                base_type: def.base_type,
                constraints: def
                    .constraints
                    .into_iter()
                    .map(|c| StoredDomainConstraint {
                        name: c.name,
                        check: c.check,
                    })
                    .collect(),
            },
        );
        Ok(())
    }

    fn has_domain(&self, name: &str) -> bool {
        self.domains.contains_key(name)
    }

    fn drop_domain(&mut self, name: &str) -> Result<()> {
        if self.domains.remove(name).is_none() {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("domain \"{}\" not found", name),
                format!("domain={}", name),
            ));
        }
        Ok(())
    }

    fn create_function(&mut self, def: FunctionDefinition) -> Result<()> {
        let key = function_key_from_definition(&def.name, &def.args);
        if self.functions.contains_key(&key) {
            return Err(PlomidError::new(
                ErrorKind::AlreadyExists,
                format!("function \"{}\" already exists", def.name),
            ));
        }
        self.functions.insert(
            key,
            StoredFunction {
                name: def.name,
                args: def
                    .args
                    .into_iter()
                    .map(|a| StoredFunctionArg {
                        name: a.name,
                        data_type: a.data_type,
                    })
                    .collect(),
                returns: def.returns,
                language: def.language,
                body: def.body,
            },
        );
        Ok(())
    }

    fn has_function(&self, name: &str, args: &[String]) -> bool {
        self.functions
            .contains_key(&function_key_from_args(name, args))
    }

    fn drop_function(&mut self, name: &str, args: &[String]) -> Result<()> {
        let key = function_key_from_args(name, args);
        if self.functions.remove(&key).is_none() {
            return Err(PlomidError::with_detail(
                ErrorKind::NotFound,
                format!("function \"{}\" not found", name),
                format!("function={}", name),
            ));
        }
        Ok(())
    }

    fn find_table_dependency(&self, name: &str) -> Option<String> {
        for (view_name, view) in &self.views {
            if view_refers_to_table(&view.query, name) {
                return Some(format!("view \"{view_name}\""));
            }
        }
        None
    }

    fn find_view_dependency(&self, _name: &str) -> Option<String> {
        None
    }

    fn find_type_dependency(&self, name: &str) -> Option<String> {
        self.find_custom_type_dependency(name, /*domains*/ false)
    }

    fn find_domain_dependency(&self, name: &str) -> Option<String> {
        self.find_custom_type_dependency(name, /*domains*/ true)
    }

    fn find_function_dependency(&self, _name: &str, _args: &[String]) -> Option<String> {
        None
    }

    fn find_column_dependency(&self, table: &str, column: &str) -> Option<String> {
        for (view_name, view) in &self.views {
            if view_refers_to_column(&view.query, table, column) {
                return Some(format!("view \"{view_name}\""));
            }
        }
        None
    }

    fn view_depends_on_table(&self, view: &str, table: &str) -> bool {
        if let Some(v) = self.views.get(view) {
            view_refers_to_table(&v.query, table)
        } else {
            false
        }
    }

    fn find_views_referencing_column(&self, table: &str, column: &str) -> Vec<String> {
        self.views
            .iter()
            .filter(|(_, view)| view_refers_to_column(&view.query, table, column))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

fn render_view_definition(statement: &Statement) -> String {
    match statement {
        Statement::Select {
            targets,
            from,
            where_expr,
            ..
        } => {
            let mut sql = String::from("SELECT ");
            sql.push_str(
                &targets
                    .iter()
                    .map(render_view_target)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            if let Some(from) = from {
                sql.push_str(" FROM ");
                sql.push_str(&render_view_from(from));
            }
            if let Some(predicate) = where_expr {
                sql.push_str(" WHERE ");
                sql.push_str(&render_view_expression(predicate));
            }
            sql
        }
        _ => String::new(),
    }
}

fn render_view_target(target: &crate::SelectTarget) -> String {
    match target {
        crate::SelectTarget::All => "*".into(),
        crate::SelectTarget::QualifiedStar { qualifier } => format!("{qualifier}.*"),
        crate::SelectTarget::Expr { expr, alias } => {
            let mut text = render_view_expression(expr);
            if let Some(alias) = alias {
                text.push_str(" AS ");
                text.push_str(alias);
            }
            text
        }
        crate::SelectTarget::Aliased { target, alias } => {
            format!("{} AS {alias}", render_view_target(target))
        }
        crate::SelectTarget::Function(name) => format!("{name}()"),
        crate::SelectTarget::FunctionCall { name, args } => format!(
            "{}({})",
            name,
            args.iter()
                .map(render_view_expression)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        crate::SelectTarget::WindowFunction { name, .. } => name.clone(),
    }
}

fn render_view_from(from: &crate::FromClause) -> String {
    match from {
        crate::FromClause::Table { name, alias } => alias
            .as_ref()
            .map_or_else(|| name.clone(), |alias| format!("{name} AS {alias}")),
        crate::FromClause::TableFunction {
            name, args, alias, ..
        } => {
            let text = format!(
                "{}({})",
                name,
                args.iter()
                    .map(render_view_expression)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            alias
                .as_ref()
                .map_or(text.clone(), |alias| format!("{text} AS {alias}"))
        }
        crate::FromClause::Subquery { .. } => "(SELECT 1)".into(),
        crate::FromClause::Join {
            left,
            right,
            kind,
            on,
        } => {
            let join = match kind {
                crate::JoinKind::Inner => " JOIN ",
                crate::JoinKind::Left => " LEFT JOIN ",
                crate::JoinKind::Right => " RIGHT JOIN ",
                crate::JoinKind::Full => " FULL JOIN ",
                crate::JoinKind::Cross => " CROSS JOIN ",
            };
            let mut text = format!(
                "{}{}{}",
                render_view_from(left),
                join,
                render_view_from(right)
            );
            if let Some(on) = on {
                text.push_str(" ON ");
                text.push_str(&render_view_expression(on));
            }
            text
        }
    }
}

fn render_view_expression(expression: &crate::Expression) -> String {
    match expression {
        crate::Expression::ColumnRef(name) => name.clone(),
        crate::Expression::Literal(value) => render_view_literal(value),
        crate::Expression::Star => "*".into(),
        crate::Expression::Equal(left, right) => format!(
            "{} = {}",
            render_view_expression(left),
            render_view_expression(right)
        ),
        crate::Expression::Greater(left, right) => format!(
            "{} > {}",
            render_view_expression(left),
            render_view_expression(right)
        ),
        crate::Expression::Less(left, right) => format!(
            "{} < {}",
            render_view_expression(left),
            render_view_expression(right)
        ),
        crate::Expression::And(left, right) => format!(
            "{} AND {}",
            render_view_expression(left),
            render_view_expression(right)
        ),
        crate::Expression::FunctionCall { name, args, .. } => format!(
            "{}({})",
            name,
            args.iter()
                .map(render_view_expression)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => "NULL".into(),
    }
}

fn render_view_literal(value: &Value) -> String {
    match value {
        Value::Text(text)
        | Value::VarChar(text)
        | Value::BpChar(text)
        | Value::Name(text)
        | Value::Json(text) => format!("'{}'", text.replace('\'', "''")),
        _ => value.to_sql_text(),
    }
}

fn infer_view_columns(statement: &Statement) -> Vec<String> {
    let Statement::Select { targets, .. } = statement else {
        return Vec::new();
    };
    targets
        .iter()
        .filter_map(|target| match target {
            crate::ast::SelectTarget::Expr { expr, alias } => {
                alias.clone().or_else(|| match expr {
                    Expression::ColumnRef(name) => name.rsplit('.').next().map(str::to_string),
                    _ => None,
                })
            }
            crate::ast::SelectTarget::Aliased { alias, .. } => Some(alias.clone()),
            crate::ast::SelectTarget::Function(name)
            | crate::ast::SelectTarget::FunctionCall { name, .. }
            | crate::ast::SelectTarget::WindowFunction { name, .. } => Some(name.clone()),
            crate::ast::SelectTarget::All | crate::ast::SelectTarget::QualifiedStar { .. } => None,
        })
        .collect()
}

fn extract_min_integer(column: &str, expr: &Expression) -> Option<i64> {
    match expr {
        Expression::Greater(left, right) => {
            let (column_expr, value_expr) = (left, right);
            extract_min_integer_from_comparison(column, column_expr, value_expr, true)
        }
        Expression::GreaterOrEqual(left, right) => {
            let (column_expr, value_expr) = (left, right);
            extract_min_integer_from_comparison(column, column_expr, value_expr, false)
        }
        Expression::Less(left, right) => {
            let (column_expr, value_expr) = (right, left);
            extract_min_integer_from_comparison(column, column_expr, value_expr, true)
        }
        Expression::LessOrEqual(left, right) => {
            let (column_expr, value_expr) = (right, left);
            extract_min_integer_from_comparison(column, column_expr, value_expr, false)
        }
        _ => None,
    }
}

fn extract_min_integer_from_comparison(
    column: &str,
    column_expr: &Expression,
    value_expr: &Expression,
    is_strict: bool,
) -> Option<i64> {
    // Check if column_expr is a reference to our column
    if !matches!(column_expr, Expression::ColumnRef(name) if name == column) {
        return None;
    }

    // Extract the integer value from value_expr
    let value = match value_expr {
        Expression::Literal(Value::Int8(v)) => *v,
        Expression::Literal(Value::Int4(v)) => *v as i64,
        Expression::Literal(Value::Int2(v)) => *v as i64,
        Expression::Negate(inner) => match &**inner {
            Expression::Literal(Value::Int8(v)) => -*v,
            Expression::Literal(Value::Int4(v)) => -(*v as i64),
            Expression::Literal(Value::Int2(v)) => -(*v as i64),
            _ => return None,
        },
        _ => return None,
    };

    // For strict inequalities (> or <), we need value + 1
    // For non-strict (>= or <=), we use value as-is
    if is_strict {
        value.checked_add(1)
    } else {
        Some(value)
    }
}

fn value_matches_column(col_type: &ColumnType, value: &Value) -> bool {
    if value.is_null() || matches!(value, Value::Unknown(_)) {
        return true;
    }
    let Some(column_pg) = col_type.pg_type() else {
        return true;
    };
    value_pg_type(value) == Some(column_pg)
}

// OIDs for user-defined types/domains start above the builtin range and are
// derived deterministically from the (lowercased, unqualified) type name so
// they are stable across restarts without persisting an OID mapping.

/// Case-insensitive match that also tolerates one side being schema-qualified.
fn type_name_matches(stored: &str, asked: &str) -> bool {
    stored.eq_ignore_ascii_case(asked)
        || bare_type_name(stored).eq_ignore_ascii_case(bare_type_name(asked))
}

pub fn load_catalog<E: plomid_txn::StorageEngine>(engine: &mut E) -> Result<InMemoryCatalog> {
    tracing::trace!(target: "catalog", "load_start");
    let mut catalog = InMemoryCatalog::new();
    match engine.get(CATALOG_KEY.as_bytes()) {
        Ok(Some(bytes)) => {
            tracing::trace!(target: "catalog", "load_decode bytes={}", bytes.len());
            catalog.decode(&bytes)?;
        }
        Ok(None) => {
            tracing::trace!(target: "catalog", "load_empty");
        }
        Err(err) => {
            tracing::error!(target: "catalog", "load_failed error={}", err);
            return Err(err);
        }
    }
    tracing::trace!(target: "catalog", "load_complete table_count={}", catalog.tables.len());
    Ok(catalog)
}

pub fn save_catalog<E: plomid_txn::StorageEngine>(
    catalog: &InMemoryCatalog,
    engine: &mut E,
) -> Result<()> {
    tracing::trace!(target: "catalog", "persist_start table_count={}", catalog.tables.len());
    let bytes = catalog.encode();
    let mut txn = engine.begin()?;
    txn.put(CATALOG_KEY.as_bytes(), &bytes)?;
    txn.commit()?;
    tracing::trace!(target: "catalog", "persist_complete bytes={}", bytes.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_lookup_table() {
        let mut catalog = InMemoryCatalog::new();
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
        let table = catalog.get_table("users").unwrap();
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].name, "id");
        assert_eq!(table.columns[1].name, "name");
    }

    #[test]
    fn unqualified_lookup_uses_session_search_path() {
        let mut catalog = InMemoryCatalog::new();
        catalog.create_schema("plomid_compat".to_string()).unwrap();
        catalog
            .create_table(
                "plomid_compat.users".to_string(),
                vec![ColumnDef {
                    name: "id".to_string(),
                    col_type: ColumnType::int4(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .unwrap();
        catalog.set_search_path(vec!["plomid_compat".to_string(), "public".to_string()]);
        assert_eq!(
            catalog.get_table("users").unwrap().name,
            "plomid_compat.users"
        );
    }

    #[test]
    fn same_name_tables_coexist_across_schemas() {
        let mut catalog = InMemoryCatalog::new();
        catalog.create_schema("plomid_test".to_string()).unwrap();
        // `public.customers` exists; an unqualified create must still be
        // allowed in `plomid_test` (the first schema on the search_path).
        catalog
            .create_table("public.customers".to_string(), vec![], Vec::new())
            .unwrap();
        catalog.set_search_path(vec!["plomid_test".to_string(), "public".to_string()]);
        let resolved = catalog.resolve_create_name("customers").unwrap();
        assert_eq!(resolved, "plomid_test.customers");
        catalog.create_table(resolved, vec![], Vec::new()).unwrap();
        // Both coexist and unqualified lookup prefers the first schema.
        assert!(catalog.has_table("public.customers"));
        assert!(catalog.has_table("plomid_test.customers"));
        assert_eq!(
            catalog.resolve_table_name("customers").unwrap(),
            "plomid_test.customers"
        );
        assert_eq!(
            catalog.get_table("plomid_test.customers").unwrap().name,
            "plomid_test.customers"
        );
    }

    #[test]
    fn create_table_does_not_reject_due_to_other_schema() {
        let mut catalog = InMemoryCatalog::new();
        catalog.create_schema("plomid_test".to_string()).unwrap();
        catalog
            .create_table("public.customers".to_string(), vec![], Vec::new())
            .unwrap();
        catalog.set_search_path(vec!["plomid_test".to_string(), "public".to_string()]);
        let resolved = catalog.resolve_create_name("customers").unwrap();
        catalog.create_table(resolved, vec![], Vec::new()).unwrap();
        // Recreating the same qualified table must still be rejected.
        assert!(catalog
            .create_table("plomid_test.customers".to_string(), vec![], Vec::new())
            .is_err());
    }

    #[test]
    fn duplicate_table_rejected() {
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table("users".to_string(), vec![], Vec::new())
            .unwrap();
        let err = catalog
            .create_table("users".to_string(), vec![], Vec::new())
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn schema_is_persistent_in_catalog_encoding() {
        let mut catalog = InMemoryCatalog::new();
        catalog.create_schema("analytics".to_string()).unwrap();
        let bytes = catalog.encode();
        let mut decoded = InMemoryCatalog::new();
        decoded.decode(&bytes).unwrap();
        assert!(decoded.has_schema("public"));
        assert!(decoded.has_schema("analytics"));
        assert_eq!(decoded.schema_names(), vec!["analytics", "public"]);
    }

    #[test]
    fn stable_object_ids_survive_catalog_round_trip() {
        let mut catalog = InMemoryCatalog::new();
        catalog.create_schema("analytics".to_string()).unwrap();
        catalog
            .create_table(
                "analytics.events".to_string(),
                vec![ColumnDef {
                    name: "id".to_string(),
                    col_type: ColumnType::int4(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .unwrap();
        let schema_id = catalog.schema_id("analytics").unwrap();
        let table = catalog.get_table("analytics.events").unwrap();
        let table_id = table.table_id;
        let column_id = table.column_ids[0];

        let mut decoded = InMemoryCatalog::new();
        decoded.decode(&catalog.encode()).unwrap();
        assert_eq!(decoded.schema_id("analytics"), Some(schema_id));
        let decoded_table = decoded.get_table("analytics.events").unwrap();
        assert_eq!(decoded_table.table_id, table_id);
        assert_eq!(decoded_table.column_ids, vec![column_id]);
    }

    #[test]
    fn index_metadata_round_trips_with_stable_identity() {
        let mut catalog = InMemoryCatalog::new();
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
                        name: "email".to_string(),
                        col_type: ColumnType::text(),
                        constraints: Vec::new(),
                    },
                ],
                Vec::new(),
            )
            .unwrap();
        catalog
            .create_index(
                "users_email_idx".to_string(),
                "users".to_string(),
                "email".to_string(),
                vec!["email".to_string()],
                None,
                true,
                None,
            )
            .unwrap();
        let expected = catalog.index("users_email_idx").unwrap();
        let mut restored = InMemoryCatalog::new();
        restored.decode(&catalog.encode()).unwrap();
        assert_eq!(restored.index("users_email_idx"), Some(expected));
    }

    #[test]
    fn catalog_round_trips() {
        let mut catalog = InMemoryCatalog::new();
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

        let bytes = catalog.encode();
        let mut decoded = InMemoryCatalog::new();
        decoded.decode(&bytes).unwrap();

        assert_eq!(decoded.table_names(), vec!["users"]);
        let table = decoded.get_table("users").unwrap();
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].name, "id");
        assert_eq!(table.columns[0].col_type, ColumnType::int4());
        assert_eq!(table.columns[1].name, "name");
        assert_eq!(table.columns[1].col_type, ColumnType::text());
    }

    #[test]
    fn unknown_column_error_has_context() {
        let mut catalog = InMemoryCatalog::new();
        catalog
            .create_table(
                "users".to_string(),
                vec![ColumnDef {
                    name: "id".to_string(),
                    col_type: ColumnType::int4(),
                    constraints: Vec::new(),
                }],
                Vec::new(),
            )
            .unwrap();
        let table = catalog.get_table("users").unwrap();
        let err = table.column_index("missing").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(err.to_string().contains("missing"));
        assert!(err.to_string().contains("users"));
        assert_eq!(err.detail(), Some("table=users column=missing"));
    }

    #[test]
    fn type_mismatch_error_has_context() {
        let mut catalog = InMemoryCatalog::new();
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
        let table = catalog.get_table("users").unwrap();
        let err = table
            .validate_row(&[crate::Value::Text("bad".to_string()), crate::Value::Null])
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.to_string().contains("expects int4"));
        assert!(err.detail().unwrap().contains("table=users"));
    }

    #[test]
    fn corrupted_catalog_metadata_returns_catalog_error() {
        let mut catalog = InMemoryCatalog::new();
        let err = catalog.decode(&[0xFF]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Catalog);
        assert!(err.to_string().contains("incompatible engine version"));
    }
}

fn function_key_from_definition(name: &str, args: &[FunctionArgDefinition]) -> String {
    let types: Vec<String> = args.iter().map(|a| a.data_type.clone()).collect();
    format!("{}[{}]", name, types.join(","))
}

fn function_key_from_args(name: &str, args: &[String]) -> String {
    format!("{}[{}]", name, args.join(","))
}

fn view_refers_to_table(query: &Statement, table: &str) -> bool {
    match query {
        Statement::Select {
            from, where_expr, ..
        } => {
            if let Some(from) = from {
                if from_refers_to_table(from, table) {
                    return true;
                }
            }
            if let Some(where_expr) = where_expr {
                if expr_refers_to_table(where_expr, table) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

fn from_refers_to_table(from: &crate::FromClause, table: &str) -> bool {
    match from {
        crate::FromClause::Table { name, .. } => name == table,
        crate::FromClause::Subquery { statement, .. } => view_refers_to_table(statement, table),
        crate::FromClause::Join { left, right, .. } => {
            from_refers_to_table(left, table) || from_refers_to_table(right, table)
        }
        crate::FromClause::TableFunction { .. } => false,
    }
}

fn expr_refers_to_table(expr: &Expression, table: &str) -> bool {
    match expr {
        Expression::ColumnRef(name) => {
            if let Some((qualifier, _)) = name.rsplit_once('.') {
                qualifier == table
            } else {
                false
            }
        }
        Expression::FunctionCall { args, .. } => {
            args.iter().any(|a| expr_refers_to_table(a, table))
        }
        Expression::And(left, right) | Expression::Or(left, right) => {
            expr_refers_to_table(left, table) || expr_refers_to_table(right, table)
        }
        Expression::Equal(left, right)
        | Expression::Greater(left, right)
        | Expression::Less(left, right)
        | Expression::GreaterOrEqual(left, right)
        | Expression::LessOrEqual(left, right)
        | Expression::NotEqual(left, right) => {
            expr_refers_to_table(left, table) || expr_refers_to_table(right, table)
        }
        Expression::Cast { expr, .. } | Expression::TypeCast { expr, .. } => {
            expr_refers_to_table(expr, table)
        }
        Expression::ScalarSubquery(query) => view_refers_to_table(query, table),
        _ => false,
    }
}

fn view_refers_to_column(query: &Statement, table: &str, column: &str) -> bool {
    match query {
        Statement::Select {
            targets,
            from,
            where_expr,
            ..
        } => {
            for target in targets {
                if target_refers_to_column(target, table, column) {
                    return true;
                }
            }
            if let Some(from) = from {
                if from_refers_to_table(from, table) {
                    for target in targets {
                        if target_refers_to_column(target, table, column) {
                            return true;
                        }
                    }
                    if let Some(where_expr) = where_expr {
                        if expr_refers_to_column(where_expr, table, column) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn target_refers_to_column(target: &crate::SelectTarget, table: &str, column: &str) -> bool {
    match target {
        crate::SelectTarget::All => false,
        crate::SelectTarget::QualifiedStar { qualifier } => qualifier == table,
        crate::SelectTarget::Expr { expr, .. } => expr_refers_to_column(expr, table, column),
        crate::SelectTarget::Function(_) => false,
        crate::SelectTarget::FunctionCall { name: _, args } => {
            args.iter().any(|a| expr_refers_to_column(a, table, column))
        }
        crate::SelectTarget::WindowFunction { args, .. } => {
            args.iter().any(|a| expr_refers_to_column(a, table, column))
        }
        crate::SelectTarget::Aliased { target, .. } => {
            target_refers_to_column(target, table, column)
        }
    }
}

fn expr_refers_to_column(expr: &Expression, table: &str, column: &str) -> bool {
    match expr {
        Expression::ColumnRef(name) => {
            if name == column {
                return true;
            }
            if let Some((qualifier, col)) = name.rsplit_once('.') {
                return qualifier == table && col == column;
            }
            false
        }
        Expression::FunctionCall { args, .. } => {
            args.iter().any(|a| expr_refers_to_column(a, table, column))
        }
        Expression::And(left, right) | Expression::Or(left, right) => {
            expr_refers_to_column(left, table, column)
                || expr_refers_to_column(right, table, column)
        }
        Expression::Equal(left, right)
        | Expression::Greater(left, right)
        | Expression::Less(left, right)
        | Expression::GreaterOrEqual(left, right)
        | Expression::LessOrEqual(left, right)
        | Expression::NotEqual(left, right) => {
            expr_refers_to_column(left, table, column)
                || expr_refers_to_column(right, table, column)
        }
        Expression::Cast { expr, .. } | Expression::TypeCast { expr, .. } => {
            expr_refers_to_column(expr, table, column)
        }
        Expression::ScalarSubquery(query) => view_refers_to_column(query, table, column),
        _ => false,
    }
}
