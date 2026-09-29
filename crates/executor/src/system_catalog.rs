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
//! Live PostgreSQL system relations.
//!
//! The provider returns relation schemas and rows derived from the authoritative
//! PLOMID catalog.  It does not inspect or execute SQL.  The normal relational
//! executor consumes these relations alongside user tables.

use plomid_core::{ColumnId, TableId};
use plomid_sql::{
    Catalog, ColumnDef, ColumnType, ConstraintKind, ForeignKeyAction, ForeignKeyMatch,
    InMemoryCatalog, TableSchema, Value,
};
use plomid_types::{PgType, TypeOid, NO_TYPEMOD};

pub struct SystemCatalog<'a> {
    pub catalog: &'a InMemoryCatalog,
    pub current_database: &'a str,
    pub current_user: &'a str,
    pub database_names: &'a [String],
}

pub struct CatalogRelation {
    pub schema: TableSchema,
    pub rows: Vec<Vec<Value>>,
}

impl<'a> SystemCatalog<'a> {
    pub fn new(
        catalog: &'a InMemoryCatalog,
        database: &'a str,
        user: &'a str,
        databases: &'a [String],
    ) -> Self {
        Self {
            catalog,
            current_database: database,
            current_user: user,
            database_names: databases,
        }
    }

    pub fn is_relation(name: &str) -> bool {
        let name = normalize_relation(name);
        matches!(
            name.as_str(),
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
                | "pg_auth_members"
                | "pg_extension"
                | "pg_replication_slots"
                | "pg_replication_origin_status"
                | "pg_stat_activity"
                | "pg_stat_database"
                | "pg_stat_user_tables"
                | "pg_stat_all_tables"
                | "pg_foreign_server"
                | "pg_foreign_data_wrapper"
                | "pg_user_mapping"
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
                | "schemata"
                | "tables"
                | "columns"
                | "views"
                | "table_constraints"
                | "key_column_usage"
                | "referential_constraints"
                | "constraint_column_usage"
                | "routines"
                | "parameters"
                | "element_types"
                | "domains"
                | "sequences"
                | "pg_default_acl"
        )
    }

    pub fn relation(&self, name: &str) -> Option<CatalogRelation> {
        let name = normalize_relation(name);
        Some(match name.as_str() {
            "pg_am" => self.pg_am(),
            "pg_database" => self.pg_database(),
            "pg_namespace" => self.pg_namespace(),
            "pg_collation" => self.pg_collation(),
            "pg_class" => self.pg_class(),
            "pg_attribute" => self.pg_attribute(),
            "pg_attrdef" => self.empty(
                "pg_attrdef",
                &[
                    ("adrelid", TypeOid::OID),
                    ("adnum", TypeOid::INT2),
                    ("adbin", TypeOid::TEXT),
                    ("adsrc", TypeOid::TEXT),
                ],
            ),
            "pg_type" => self.pg_type(),
            "pg_aggregate" => self.empty(
                "pg_aggregate",
                &[
                    ("aggfnoid", TypeOid::OID),
                    ("aggkind", TypeOid::CHAR),
                    ("aggnumdirectargs", TypeOid::INT2),
                    ("aggtransfn", TypeOid::REGPROC),
                    ("aggfinalfn", TypeOid::REGPROC),
                    ("aggcombinefn", TypeOid::REGPROC),
                    ("aggserialfn", TypeOid::REGPROC),
                    ("aggdeserialfn", TypeOid::REGPROC),
                    ("aggmtransfn", TypeOid::REGPROC),
                    ("aggminvtransfn", TypeOid::REGPROC),
                    ("aggmfinalfn", TypeOid::REGPROC),
                    ("aggfinalextra", TypeOid::BOOL),
                    ("aggmfinalextra", TypeOid::BOOL),
                    ("aggtranstype", TypeOid::OID),
                    ("aggtransspace", TypeOid::INT4),
                    ("aggmtranstype", TypeOid::OID),
                    ("aggmtransspace", TypeOid::INT4),
                    ("agginitval", TypeOid::TEXT),
                    ("aggminitval", TypeOid::TEXT),
                ],
            ),
            "pg_operator" => self.empty(
                "pg_operator",
                &[
                    ("oid", TypeOid::OID),
                    ("oprname", TypeOid::NAME),
                    ("oprnamespace", TypeOid::OID),
                    ("oprowner", TypeOid::OID),
                    ("oprkind", TypeOid::CHAR),
                    ("oprcanmerge", TypeOid::BOOL),
                    ("oprcanhash", TypeOid::BOOL),
                    ("oprleft", TypeOid::OID),
                    ("oprright", TypeOid::OID),
                    ("oprresult", TypeOid::OID),
                    ("oprcom", TypeOid::OID),
                    ("oprnegate", TypeOid::OID),
                    ("oprcode", TypeOid::REGPROC),
                    ("oprrest", TypeOid::REGPROC),
                    ("oprjoin", TypeOid::REGPROC),
                ],
            ),
            "pg_opclass" => self.empty(
                "pg_opclass",
                &[
                    ("oid", TypeOid::OID),
                    ("opcmethod", TypeOid::OID),
                    ("opcname", TypeOid::NAME),
                    ("opcnamespace", TypeOid::OID),
                    ("opcowner", TypeOid::OID),
                    ("opcfamily", TypeOid::OID),
                    ("opcintype", TypeOid::OID),
                    ("opcdefault", TypeOid::BOOL),
                    ("opckeytype", TypeOid::OID),
                ],
            ),
            "pg_amop" => self.empty(
                "pg_amop",
                &[
                    ("oid", TypeOid::OID),
                    ("amopfamily", TypeOid::OID),
                    ("amoplefttype", TypeOid::OID),
                    ("amoprighttype", TypeOid::OID),
                    ("amopstrategy", TypeOid::INT2),
                    ("amoppurpose", TypeOid::CHAR),
                    ("amopopr", TypeOid::OID),
                    ("amopmethod", TypeOid::OID),
                    ("amopsortfamily", TypeOid::OID),
                ],
            ),
            "pg_amproc" => self.empty(
                "pg_amproc",
                &[
                    ("oid", TypeOid::OID),
                    ("amprocfamily", TypeOid::OID),
                    ("amproclefttype", TypeOid::OID),
                    ("amprocrighttype", TypeOid::OID),
                    ("amprocnum", TypeOid::INT2),
                    ("amproc", TypeOid::REGPROC),
                ],
            ),
            "pg_cast" => self.empty(
                "pg_cast",
                &[
                    ("oid", TypeOid::OID),
                    ("castsource", TypeOid::OID),
                    ("casttarget", TypeOid::OID),
                    ("castfunc", TypeOid::OID),
                    ("castcontext", TypeOid::CHAR),
                    ("castmethod", TypeOid::CHAR),
                ],
            ),
            "pg_statistic" => self.empty(
                "pg_statistic",
                &[
                    ("starelid", TypeOid::OID),
                    ("staattnum", TypeOid::INT2),
                    ("stainherit", TypeOid::BOOL),
                    ("stanullfrac", TypeOid::FLOAT4),
                    ("stawidth", TypeOid::INT4),
                    ("stadistinct", TypeOid::FLOAT4),
                ],
            ),
            "pg_conversion" => self.empty(
                "pg_conversion",
                &[
                    ("oid", TypeOid::OID),
                    ("conname", TypeOid::NAME),
                    ("connamespace", TypeOid::OID),
                    ("conowner", TypeOid::OID),
                    ("conforencoding", TypeOid::INT4),
                    ("contoencoding", TypeOid::INT4),
                    ("conproc", TypeOid::REGPROC),
                    ("condefault", TypeOid::BOOL),
                ],
            ),
            // pg_default_acl: PLOMID does not currently implement default ACLs.
            // Expose an empty, structurally compatible relation so that
            // PostgreSQL GUI clients (DBeaver, pgAdmin, etc.) can complete
            // metadata discovery without error 42P01.
            "pg_default_acl" => self.empty(
                "pg_default_acl",
                &[
                    ("oid", TypeOid::OID),
                    ("defaclrole", TypeOid::OID),
                    ("defaclnamespace", TypeOid::OID),
                    ("defaclobjtype", TypeOid::CHAR),
                    ("defaclacl", TypeOid::ACLITEM_ARRAY),
                ],
            ),
            "pg_language" => self.empty(
                "pg_language",
                &[
                    ("oid", TypeOid::OID),
                    ("lanname", TypeOid::NAME),
                    ("lanowner", TypeOid::OID),
                    ("lanispl", TypeOid::BOOL),
                    ("lanpltrusted", TypeOid::BOOL),
                    ("lanplcallfoid", TypeOid::OID),
                    ("laninline", TypeOid::OID),
                    ("lanvalidator", TypeOid::OID),
                    ("lanacl", TypeOid::ACLITEM_ARRAY),
                ],
            ),
            // Enum rows are derived from registered user enum definitions.  The
            // relation must exist even when the database currently has none;
            // clients probe it during ordinary catalog discovery.
            "pg_enum" => self.empty(
                "pg_enum",
                &[
                    ("oid", TypeOid::OID),
                    ("enumtypid", TypeOid::OID),
                    ("enumsortorder", TypeOid::FLOAT4),
                    ("enumlabel", TypeOid::NAME),
                ],
            ),
            "pg_shdescription" => self.empty(
                "pg_shdescription",
                &[
                    ("objoid", TypeOid::OID),
                    ("classoid", TypeOid::OID),
                    ("description", TypeOid::TEXT),
                ],
            ),
            "pg_index" => self.pg_index(),
            "pg_constraint" => self.pg_constraint(),
            "pg_roles" => self.pg_roles(),
            "pg_user" => self.pg_user(),
            "pg_group" => self.pg_group(),
            "pg_tablespace" => self.pg_tablespace(),
            "pg_settings" => self.pg_settings(),
            "pg_tables" => self.pg_tables(),
            "pg_views" => self.pg_views(),
            "pg_indexes" => self.pg_indexes(),
            "schemata" => self.schemata(),
            "tables" => self.info_tables(),
            "columns" => self.info_columns(),
            "views" => self.info_views(),
            "table_constraints" => self.table_constraints(),
            "key_column_usage" => self.key_columns(),
            "referential_constraints" => self.referential_constraints(),
            "constraint_column_usage" => self.constraint_column_usage(),
            "sequences" => self.sequences(),
            "pg_auth_members" => self.empty(
                "pg_auth_members",
                &[
                    ("roleid", TypeOid::OID),
                    ("member", TypeOid::OID),
                    ("grantor", TypeOid::OID),
                    ("admin_option", TypeOid::BOOL),
                ],
            ),
            "pg_extension" => self.empty(
                "pg_extension",
                &[
                    ("oid", TypeOid::OID),
                    ("extname", TypeOid::NAME),
                    ("extowner", TypeOid::OID),
                ],
            ),
            "pg_replication_slots" => self.empty(
                "pg_replication_slots",
                &[
                    ("slot_name", TypeOid::NAME),
                    ("plugin", TypeOid::NAME),
                    ("slot_type", TypeOid::NAME),
                ],
            ),
            "pg_replication_origin_status" => self.empty(
                "pg_replication_origin_status",
                &[("local_id", TypeOid::OID), ("external_id", TypeOid::TEXT)],
            ),
            "pg_stat_activity" => self.empty(
                "pg_stat_activity",
                &[
                    ("datid", TypeOid::OID),
                    ("datname", TypeOid::NAME),
                    ("pid", TypeOid::INT4),
                    ("usename", TypeOid::NAME),
                    ("state", TypeOid::TEXT),
                ],
            ),
            "pg_stat_database" => self.empty(
                "pg_stat_database",
                &[
                    ("datid", TypeOid::OID),
                    ("datname", TypeOid::NAME),
                    ("numbackends", TypeOid::INT4),
                ],
            ),
            "pg_stat_user_tables" | "pg_stat_all_tables" => self.empty(
                "pg_stat_user_tables",
                &[
                    ("relid", TypeOid::OID),
                    ("schemaname", TypeOid::NAME),
                    ("relname", TypeOid::NAME),
                ],
            ),
            "pg_foreign_server" => self.empty(
                "pg_foreign_server",
                &[
                    ("oid", TypeOid::OID),
                    ("srvname", TypeOid::NAME),
                    ("srvowner", TypeOid::OID),
                ],
            ),
            "pg_foreign_data_wrapper" => self.empty(
                "pg_foreign_data_wrapper",
                &[("oid", TypeOid::OID), ("fdwname", TypeOid::NAME)],
            ),
            "pg_user_mapping" => self.empty(
                "pg_user_mapping",
                &[
                    ("oid", TypeOid::OID),
                    ("umuser", TypeOid::OID),
                    ("umserver", TypeOid::OID),
                ],
            ),
            "pg_depend" | "pg_shdepend" => self.empty(
                "pg_depend",
                &[
                    ("classid", TypeOid::OID),
                    ("objid", TypeOid::OID),
                    ("objsubid", TypeOid::INT4),
                    ("refclassid", TypeOid::OID),
                    ("refobjid", TypeOid::OID),
                    ("refobjsubid", TypeOid::INT4),
                    ("deptype", TypeOid::CHAR),
                ],
            ),
            "pg_trigger" => self.empty(
                "pg_trigger",
                &[
                    ("oid", TypeOid::OID),
                    ("tgrelid", TypeOid::OID),
                    ("tgparentid", TypeOid::OID),
                    ("tgname", TypeOid::NAME),
                    ("tgfoid", TypeOid::OID),
                    ("tgtype", TypeOid::INT2),
                    ("tgenabled", TypeOid::CHAR),
                    ("tgisinternal", TypeOid::BOOL),
                    ("tgconstrrelid", TypeOid::OID),
                    ("tgconstrindid", TypeOid::OID),
                    ("tgconstraint", TypeOid::OID),
                    ("tgdeferrable", TypeOid::BOOL),
                    ("tginitdeferred", TypeOid::BOOL),
                    ("tgnargs", TypeOid::INT2),
                    ("tgattr", TypeOid::INT2VECTOR),
                    ("tgargs", TypeOid::BYTEA),
                    ("tgqual", TypeOid::TEXT),
                    ("tgoldtable", TypeOid::NAME),
                    ("tgnewtable", TypeOid::NAME),
                ],
            ),
            "pg_policy" => self.empty(
                "pg_policy",
                &[
                    ("oid", TypeOid::OID),
                    ("polname", TypeOid::NAME),
                    ("polrelid", TypeOid::OID),
                    ("polcmd", TypeOid::CHAR),
                    ("polpermissive", TypeOid::BOOL),
                    ("polroles", TypeOid(1028)),
                    ("polqual", TypeOid::TEXT),
                    ("polwithcheck", TypeOid::TEXT),
                ],
            ),
            // DBeaver probes pg_policies for row-level-security metadata.
            // Plomid has no RLS policies, so expose the real
            // PostgreSQL-shaped view with no rows rather than failing with
            // "table pg_catalog.pg_policies not found".
            "pg_policies" => self.empty(
                "pg_policies",
                &[
                    ("schemaname", TypeOid::NAME),
                    ("tablename", TypeOid::NAME),
                    ("policyname", TypeOid::NAME),
                    ("permissive", TypeOid::TEXT),
                    ("roles", TypeOid::NAME_ARRAY),
                    ("cmd", TypeOid::TEXT),
                    ("qual", TypeOid::TEXT),
                    ("with_check", TypeOid::TEXT),
                ],
            ),
            "pg_statistic_ext" => self.empty(
                "pg_statistic_ext",
                &[
                    ("oid", TypeOid::OID),
                    ("stxrelid", TypeOid::OID),
                    ("stxnamespace", TypeOid::OID),
                    ("stxname", TypeOid::NAME),
                    ("stxowner", TypeOid::OID),
                    ("stxstattarget", TypeOid::INT2),
                    ("stxkeys", TypeOid(1007)),
                    ("stxkind", TypeOid(1002)),
                ],
            ),
            "pg_publication" => self.empty(
                "pg_publication",
                &[
                    ("oid", TypeOid::OID),
                    ("pubname", TypeOid::NAME),
                    ("pubowner", TypeOid::OID),
                    ("puballtables", TypeOid::BOOL),
                    ("pubinsert", TypeOid::BOOL),
                    ("pubupdate", TypeOid::BOOL),
                    ("pubdelete", TypeOid::BOOL),
                    ("pubtruncate", TypeOid::BOOL),
                    ("pubviaroot", TypeOid::BOOL),
                ],
            ),
            "pg_publication_rel" => self.empty(
                "pg_publication_rel",
                &[
                    ("oid", TypeOid::OID),
                    ("prpubid", TypeOid::OID),
                    ("prrelid", TypeOid::OID),
                    ("prqual", TypeOid::TEXT),
                    ("prattrs", TypeOid(1007)),
                ],
            ),
            "pg_inherits" => self.empty(
                "pg_inherits",
                &[
                    ("inhrelid", TypeOid::OID),
                    ("inhparent", TypeOid::OID),
                    ("inhseqno", TypeOid::INT4),
                    ("inhdetachpending", TypeOid::BOOL),
                ],
            ),
            "pg_rewrite" => self.empty(
                "pg_rewrite",
                &[
                    ("oid", TypeOid::OID),
                    ("rulename", TypeOid::NAME),
                    ("ev_class", TypeOid::OID),
                    ("ev_type", TypeOid::CHAR),
                    ("ev_enabled", TypeOid::CHAR),
                    ("is_instead", TypeOid::BOOL),
                    ("ev_qual", TypeOid::TEXT),
                    ("ev_action", TypeOid::TEXT),
                ],
            ),
            "pg_sequence" => self.pg_sequence(),
            "pg_sequences" => self.pg_sequences(),
            // Materialized views are not implemented by the SQL/catalog
            // layer.  Exposing the real PostgreSQL-shaped relation with no
            // rows lets generic catalog queries distinguish "no supported
            // objects" from a missing catalog relation without inventing
            // metadata.
            "pg_matviews" => self.empty(
                "pg_matviews",
                &[
                    ("schemaname", TypeOid::NAME),
                    ("matviewname", TypeOid::NAME),
                    ("matviewowner", TypeOid::NAME),
                    ("tablespace", TypeOid::NAME),
                    ("hasindexes", TypeOid::BOOL),
                    ("ispopulated", TypeOid::BOOL),
                    ("definition", TypeOid::TEXT),
                ],
            ),
            "pg_show_all_settings" => self.settings_function(),
            "pg_description" => self.empty(
                "pg_description",
                &[
                    ("objoid", TypeOid::OID),
                    ("classoid", TypeOid::OID),
                    ("objsubid", TypeOid::INT4),
                    ("description", TypeOid::TEXT),
                ],
            ),
            "pg_proc" => self.empty(
                "pg_proc",
                &[
                    ("oid", TypeOid::OID),
                    ("proname", TypeOid::NAME),
                    ("pronamespace", TypeOid::OID),
                    ("proowner", TypeOid::OID),
                    ("prorettype", TypeOid::OID),
                ],
            ),
            "routines" => self.routines(),
            "parameters" => self.empty(
                "parameters",
                &[
                    ("specific_catalog", TypeOid::NAME),
                    ("specific_schema", TypeOid::NAME),
                    ("specific_name", TypeOid::NAME),
                    ("ordinal_position", TypeOid::INT4),
                    ("parameter_mode", TypeOid::VARCHAR),
                    ("parameter_name", TypeOid::NAME),
                    ("data_type", TypeOid::TEXT),
                    ("udt_catalog", TypeOid::NAME),
                    ("udt_schema", TypeOid::NAME),
                    ("udt_name", TypeOid::NAME),
                ],
            ),
            "element_types" => self.empty(
                "element_types",
                &[
                    ("object_catalog", TypeOid::NAME),
                    ("object_schema", TypeOid::NAME),
                    ("object_name", TypeOid::NAME),
                    ("collection_type_identifier", TypeOid::TEXT),
                    ("data_type", TypeOid::TEXT),
                    ("udt_catalog", TypeOid::NAME),
                    ("udt_schema", TypeOid::NAME),
                    ("udt_name", TypeOid::NAME),
                ],
            ),
            "domains" => self.empty(
                "domains",
                &[
                    ("domain_catalog", TypeOid::NAME),
                    ("domain_schema", TypeOid::NAME),
                    ("domain_name", TypeOid::NAME),
                    ("data_type", TypeOid::TEXT),
                    ("udt_catalog", TypeOid::NAME),
                    ("udt_schema", TypeOid::NAME),
                    ("udt_name", TypeOid::NAME),
                ],
            ),
            _ => return None,
        })
    }

    fn empty(&self, name: &str, cols: &[(&str, TypeOid)]) -> CatalogRelation {
        make_relation(name, cols, Vec::new())
    }
    fn pg_database(&self) -> CatalogRelation {
        let rows = self
            .database_names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                vec![
                    Value::Oid((i + 1) as u32),
                    Value::Text(n.clone()),
                    Value::Oid(10),
                    Value::Int4(6),
                    Value::Text("C".into()),
                    Value::Text("C".into()),
                    Value::Null,
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Int2(0),
                    Value::Oid(1663),
                ]
            })
            .collect();
        make_relation(
            "pg_database",
            &[
                ("oid", TypeOid::OID),
                ("datname", TypeOid::NAME),
                ("datdba", TypeOid::OID),
                ("encoding", TypeOid::INT4),
                ("datcollate", TypeOid::NAME),
                ("datctype", TypeOid::NAME),
                ("datacl", TypeOid(1034)),
                ("datallowconn", TypeOid::BOOL),
                ("datistemplate", TypeOid::BOOL),
                ("datconnlimit", TypeOid::INT4),
                ("dattablespace", TypeOid::OID),
            ],
            rows,
        )
    }
    fn pg_namespace(&self) -> CatalogRelation {
        let mut r = vec![
            vec![
                Value::Oid(11),
                Value::Text("pg_catalog".into()),
                Value::Oid(10),
                Value::Null,
            ],
            vec![
                Value::Oid(13207),
                Value::Text("information_schema".into()),
                Value::Oid(10),
                Value::Null,
            ],
        ];
        r.extend(self.all_schema_names().into_iter().map(|n| {
            vec![
                Value::Oid(self.namespace_oid(&n)),
                Value::Text(n.to_string()),
                Value::Oid(10),
                Value::Null,
            ]
        }));
        make_relation(
            "pg_namespace",
            &[
                ("oid", TypeOid::OID),
                ("nspname", TypeOid::NAME),
                ("nspowner", TypeOid::OID),
                ("nspacl", TypeOid(1034)),
            ],
            r,
        )
    }

    /// Include schemas represented by qualified relations even if an older
    /// catalog payload did not persist a separate schema-registry entry.
    fn all_schema_names(&self) -> Vec<String> {
        let mut names = self.catalog.schema_names();
        names.extend(
            self.catalog
                .tables()
                .into_iter()
                .map(|table| split_relation(&table.name).0.to_string()),
        );
        names.extend(
            self.catalog
                .view_names()
                .into_iter()
                .map(|view| split_relation(&view).0.to_string()),
        );
        names.extend(
            self.catalog
                .sequence_names()
                .into_iter()
                .map(|sequence| split_relation(&sequence).0.to_string()),
        );
        names.sort_unstable();
        names.dedup();
        names
    }

    fn pg_am(&self) -> CatalogRelation {
        // PLOMID currently has a heap-like table access method and a B-tree
        // index access method.  These are the access methods represented by
        // the live catalog; no placeholder rows are exposed for unsupported
        // PostgreSQL methods.
        let rows = vec![
            vec![
                Value::Oid(2),
                Value::Text("heap".into()),
                Value::Oid(0),
                Value::BpChar("t".into()),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(true),
            ],
            vec![
                Value::Oid(403),
                Value::Text("btree".into()),
                Value::Oid(0),
                Value::BpChar("i".into()),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
            ],
        ];
        make_relation(
            "pg_am",
            &[
                ("oid", TypeOid::OID),
                ("amname", TypeOid::NAME),
                ("amhandler", TypeOid::REGPROC),
                ("amtype", TypeOid::CHAR),
                ("amcanorder", TypeOid::BOOL),
                ("amcanunique", TypeOid::BOOL),
                ("amcanmulticol", TypeOid::BOOL),
                ("amcaninclude", TypeOid::BOOL),
                ("amcanexclude", TypeOid::BOOL),
                ("amcanbackward", TypeOid::BOOL),
                ("amcanparallel", TypeOid::BOOL),
                ("amcanbuild", TypeOid::BOOL),
            ],
            rows,
        )
    }
    fn pg_collation(&self) -> CatalogRelation {
        // The SQL layer currently uses deterministic C/UTF-8 ordering. Keep
        // that fact visible to catalog clients (including psql) through the
        // real pg_collation relation.
        make_relation(
            "pg_collation",
            &[
                ("oid", TypeOid::OID),
                ("collname", TypeOid::NAME),
                ("collnamespace", TypeOid::OID),
                ("collowner", TypeOid::OID),
                ("collprovider", TypeOid::CHAR),
                ("collisdeterministic", TypeOid::BOOL),
                ("collencoding", TypeOid::INT4),
                ("collcollate", TypeOid::NAME),
                ("collctype", TypeOid::NAME),
                ("colliculocale", TypeOid::NAME),
                ("collversion", TypeOid::TEXT),
            ],
            vec![
                vec![
                    Value::Oid(100),
                    Value::Name("default".into()),
                    Value::Oid(11),
                    Value::Oid(10),
                    Value::BpChar("c".into()),
                    Value::Bool(true),
                    Value::Int4(-1),
                    Value::Name("C".into()),
                    Value::Name("C".into()),
                    Value::Null,
                    Value::Null,
                ],
                vec![
                    Value::Oid(950),
                    Value::Name("C".into()),
                    Value::Oid(11),
                    Value::Oid(10),
                    Value::BpChar("c".into()),
                    Value::Bool(true),
                    Value::Int4(-1),
                    Value::Name("C".into()),
                    Value::Name("C".into()),
                    Value::Null,
                    Value::Null,
                ],
            ],
        )
    }

    fn pg_class(&self) -> CatalogRelation {
        let mut r = Vec::new();
        for t in self.catalog.tables() {
            let (s, n) = split_relation(&t.name);
            let has_constraint_index = t.constraints.iter().any(|constraint| {
                matches!(
                    constraint.kind,
                    ConstraintKind::PrimaryKey | ConstraintKind::Unique
                )
            });
            r.push(self.pg_class_row(
                t.table_id.get() as u32,
                n,
                self.namespace_oid(s),
                "r",
                t.columns.len() as i16,
                t.check_constraints().len() as i16,
                !self.catalog.indexes_for_table(&t.name).is_empty() || has_constraint_index,
            ));
        }
        for v in self.catalog.view_names() {
            let (s, n) = split_relation(&v);
            r.push(
                self.pg_class_row(
                    self.view_oid(&v),
                    n,
                    self.namespace_oid(s),
                    "v",
                    self.catalog
                        .get_view(&v)
                        .map_or(0, |view| view.columns.len()) as i16,
                    0,
                    false,
                ),
            );
        }
        for i in self.catalog.indexes() {
            let (s, _) = split_relation(&i.table);
            r.push(self.pg_class_row(
                i.index_id.get() as u32,
                &i.name,
                self.namespace_oid(s),
                "i",
                0,
                0,
                false,
            ));
        }
        for t in self.catalog.tables() {
            let (schema, _) = split_relation(&t.name);
            for (constraint_index, constraint) in t.constraints.iter().enumerate() {
                if !matches!(
                    constraint.kind,
                    ConstraintKind::PrimaryKey | ConstraintKind::Unique
                ) {
                    continue;
                }
                r.push(self.pg_class_row(
                    constraint_index_oid(t.table_id.get() as u32, constraint_index),
                    &constraint_index_name(&t.name, constraint),
                    self.namespace_oid(schema),
                    "i",
                    0,
                    0,
                    false,
                ));
            }
        }
        for sequence in self.catalog.sequence_names() {
            let (s, n) = split_relation(&sequence);
            r.push(self.pg_class_row(
                self.relation_oid(&sequence),
                n,
                self.namespace_oid(s),
                "S",
                0,
                0,
                false,
            ));
        }
        make_relation(
            "pg_class",
            &[
                ("oid", TypeOid::OID),
                ("relname", TypeOid::NAME),
                ("relnamespace", TypeOid::OID),
                ("reltype", TypeOid::OID),
                ("reloftype", TypeOid::OID),
                ("relowner", TypeOid::OID),
                ("relam", TypeOid::OID),
                ("relfilenode", TypeOid::OID),
                ("reltablespace", TypeOid::OID),
                ("relpages", TypeOid::INT4),
                ("reltuples", TypeOid::FLOAT4),
                ("relallvisible", TypeOid::INT4),
                ("reltoastrelid", TypeOid::OID),
                ("relhasindex", TypeOid::BOOL),
                ("relhasoids", TypeOid::BOOL),
                ("relisshared", TypeOid::BOOL),
                ("relpersistence", TypeOid::CHAR),
                ("relkind", TypeOid::CHAR),
                ("relnatts", TypeOid::INT2),
                ("relchecks", TypeOid::INT2),
                ("relhasrules", TypeOid::BOOL),
                ("relhastriggers", TypeOid::BOOL),
                ("relhassubclass", TypeOid::BOOL),
                ("relrowsecurity", TypeOid::BOOL),
                ("relforcerowsecurity", TypeOid::BOOL),
                ("relispopulated", TypeOid::BOOL),
                ("relreplident", TypeOid::CHAR),
                ("relispartition", TypeOid::BOOL),
                ("relrewrite", TypeOid::OID),
                ("relfrozenxid", TypeOid::XID),
                ("relminmxid", TypeOid::XID),
                ("relacl", TypeOid::ACLITEM_ARRAY),
                ("reloptions", TypeOid::TEXT_ARRAY),
                ("relpartbound", TypeOid::TEXT),
            ],
            r,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn pg_class_row(
        &self,
        oid: u32,
        name: &str,
        namespace: u32,
        kind: &str,
        natts: i16,
        checks: i16,
        has_index: bool,
    ) -> Vec<Value> {
        vec![
            Value::Oid(oid),
            Value::Text(name.into()),
            Value::Oid(namespace),
            Value::Oid(if kind == "i" {
                403
            } else if kind == "r" {
                2
            } else {
                0
            }),
            Value::Oid(0),
            Value::Oid(10),
            Value::Oid(0),
            Value::Oid(oid),
            Value::Oid(0),
            Value::Int4(0),
            Value::Float4(0.0),
            Value::Int4(0),
            Value::Oid(0),
            Value::Bool(has_index),
            Value::Bool(false),
            Value::Bool(false),
            Value::BpChar("p".into()),
            Value::BpChar(kind.into()),
            Value::Int2(natts),
            Value::Int2(checks),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(true),
            Value::BpChar("d".into()),
            Value::Bool(false),
            Value::Oid(0),
            Value::Int4(0),
            Value::Int4(0),
            Value::Null,
            Value::Null,
            Value::Null,
        ]
    }
    fn pg_attribute(&self) -> CatalogRelation {
        let mut r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                t.columns.iter().enumerate().map(move |(i, c)| {
                    vec![
                        Value::Oid(t.table_id.get() as u32),
                        Value::Text(c.name.clone()),
                        Value::Oid(c.col_type.type_oid.0),
                        Value::Int4(-1),
                        Value::Int2(type_length(c.col_type.type_oid)),
                        Value::Int2((i + 1) as i16),
                        Value::Int4(0),
                        Value::Int4(-1),
                        Value::Int4(c.col_type.typmod),
                        Value::Bool(type_by_value(c.col_type.type_oid)),
                        Value::BpChar(type_alignment(c.col_type.type_oid).into()),
                        Value::BpChar("x".into()),
                        Value::BpChar("".into()),
                        Value::Bool(t.is_not_null(i)),
                        Value::Bool(t.default_expr(i).is_some()),
                        Value::Bool(false),
                        Value::BpChar("".into()),
                        Value::BpChar("".into()),
                        Value::Bool(false),
                        Value::Bool(true),
                        Value::Int4(0),
                        Value::Oid(0),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                    ]
                })
            })
            .collect::<Vec<_>>();
        for view_name in self.catalog.view_names() {
            let relid = self.view_oid(&view_name);
            if let Some(view) = self.catalog.get_view(&view_name) {
                for (index, column) in view.columns.iter().enumerate() {
                    let col_type = view
                        .column_type(index)
                        .map(|ct| ct.type_oid)
                        .unwrap_or(TypeOid::TEXT);
                    r.push(vec![
                        Value::Oid(relid),
                        Value::Text(column.clone()),
                        Value::Oid(col_type.0),
                        Value::Int4(-1),
                        Value::Int2(type_length(col_type)),
                        Value::Int2((index + 1) as i16),
                        Value::Int4(0),
                        Value::Int4(-1),
                        Value::Int4(NO_TYPEMOD),
                        Value::Bool(false),
                        Value::BpChar("i".into()),
                        Value::BpChar("x".into()),
                        Value::BpChar("".into()),
                        Value::Bool(false),
                        Value::Bool(false),
                        Value::Bool(false),
                        Value::BpChar("".into()),
                        Value::BpChar("".into()),
                        Value::Bool(false),
                        Value::Bool(true),
                        Value::Int4(0),
                        Value::Oid(0),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                    ]);
                }
            }
        }
        make_relation(
            "pg_attribute",
            &[
                ("attrelid", TypeOid::OID),
                ("attname", TypeOid::NAME),
                ("atttypid", TypeOid::OID),
                ("attstattarget", TypeOid::INT4),
                ("attlen", TypeOid::INT2),
                ("attnum", TypeOid::INT2),
                ("attndims", TypeOid::INT4),
                ("attcacheoff", TypeOid::INT4),
                ("atttypmod", TypeOid::INT4),
                ("attbyval", TypeOid::BOOL),
                ("attalign", TypeOid::CHAR),
                ("attstorage", TypeOid::CHAR),
                ("attcompression", TypeOid::CHAR),
                ("attnotnull", TypeOid::BOOL),
                ("atthasdef", TypeOid::BOOL),
                ("atthasmissing", TypeOid::BOOL),
                ("attidentity", TypeOid::CHAR),
                ("attgenerated", TypeOid::CHAR),
                ("attisdropped", TypeOid::BOOL),
                ("attislocal", TypeOid::BOOL),
                ("attinhcount", TypeOid::INT4),
                ("attcollation", TypeOid::OID),
                ("attacl", TypeOid::ACLITEM_ARRAY),
                ("attoptions", TypeOid::TEXT_ARRAY),
                ("attfdwoptions", TypeOid::TEXT_ARRAY),
                ("attmissingval", TypeOid::TEXT_ARRAY),
            ],
            r,
        )
    }
    fn pg_type(&self) -> CatalogRelation {
        let r = PgType::all()
            .filter(|t| !t.is_pseudo())
            .map(|t| {
                vec![
                    Value::Oid(t.oid().0),
                    Value::Text(t.name().into()),
                    Value::Oid(11),
                    Value::Oid(10),
                    Value::Int2(-1),
                    Value::Bool(false),
                    Value::BpChar("b".into()),
                    Value::Text(t.category().code().into()),
                    Value::Bool(false),
                    Value::Bool(true),
                    Value::BpChar(",".into()),
                    Value::Oid(0),
                    Value::Oid(0),
                    Value::Oid(t.array_oid().map_or(0, |o| o.0)),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::BpChar("i".into()),
                    Value::BpChar("p".into()),
                    Value::Bool(false),
                    Value::Oid(0),
                    Value::Int4(NO_TYPEMOD),
                    Value::Int2(0),
                    Value::Oid(0),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ]
            })
            .collect();
        make_relation(
            "pg_type",
            &[
                ("oid", TypeOid::OID),
                ("typname", TypeOid::NAME),
                ("typnamespace", TypeOid::OID),
                ("typowner", TypeOid::OID),
                ("typlen", TypeOid::INT2),
                ("typbyval", TypeOid::BOOL),
                ("typtype", TypeOid::CHAR),
                ("typcategory", TypeOid::CHAR),
                ("typispreferred", TypeOid::BOOL),
                ("typisdefined", TypeOid::BOOL),
                ("typdelim", TypeOid::CHAR),
                ("typrelid", TypeOid::OID),
                ("typelem", TypeOid::OID),
                ("typarray", TypeOid::OID),
                ("typinput", TypeOid::REGPROC),
                ("typoutput", TypeOid::REGPROC),
                ("typreceive", TypeOid::REGPROC),
                ("typsend", TypeOid::REGPROC),
                ("typmodin", TypeOid::REGPROC),
                ("typmodout", TypeOid::REGPROC),
                ("typanalyze", TypeOid::REGPROC),
                ("typalign", TypeOid::CHAR),
                ("typstorage", TypeOid::CHAR),
                ("typnotnull", TypeOid::BOOL),
                ("typbasetype", TypeOid::OID),
                ("typtypmod", TypeOid::INT4),
                ("typndims", TypeOid::INT2),
                ("typcollation", TypeOid::OID),
                ("typdefaultbin", TypeOid::TEXT),
                ("typdefault", TypeOid::TEXT),
                ("typacl", TypeOid::ACLITEM_ARRAY),
            ],
            r,
        )
    }
    fn pg_index(&self) -> CatalogRelation {
        let mut r = self
            .catalog
            .indexes()
            .into_iter()
            .filter_map(|i| {
                let t = self.catalog.get_table(&i.table).ok()?;
                let c = t.column_index(&i.column).ok()?;
                Some(vec![
                    Value::Oid(i.index_id.get() as u32),
                    Value::Oid(t.table_id.get() as u32),
                    Value::Bool(i.unique),
                    Value::Int2Vector(vec![(c + 1) as i16]),
                    Value::Bool(t.constraints.iter().any(|x| {
                        x.kind == ConstraintKind::PrimaryKey && x.columns.contains(&i.column)
                    })),
                    Value::Null,
                    Value::Null,
                ])
            })
            .collect::<Vec<_>>();
        for t in self.catalog.tables() {
            for (constraint_index, constraint) in t.constraints.iter().enumerate() {
                if !matches!(
                    constraint.kind,
                    ConstraintKind::PrimaryKey | ConstraintKind::Unique
                ) {
                    continue;
                }
                let indkey = constraint
                    .columns
                    .iter()
                    .filter_map(|column| t.columns.iter().position(|c| c.name == *column))
                    .map(|position| Value::Int2((position + 1) as i16))
                    .collect::<Vec<_>>();
                // PostgreSQL exposes pg_index.indkey as int2vector (OID 22),
                // including for a single-column index.  Do not collapse the
                // one-column case to int2: clients use array/vector
                // functions such as array_upper() while inspecting indexes.
                let indkey = Value::Int2Vector(
                    indkey
                        .into_iter()
                        .filter_map(|value| match value {
                            Value::Int2(value) => Some(value),
                            _ => None,
                        })
                        .collect(),
                );
                r.push(vec![
                    Value::Oid(constraint_index_oid(
                        t.table_id.get() as u32,
                        constraint_index,
                    )),
                    Value::Oid(t.table_id.get() as u32),
                    Value::Bool(true),
                    indkey,
                    Value::Bool(constraint.kind == ConstraintKind::PrimaryKey),
                    Value::Null,
                    Value::Null,
                ]);
            }
        }
        make_relation(
            "pg_index",
            &[
                ("indexrelid", TypeOid::OID),
                ("indrelid", TypeOid::OID),
                ("indisunique", TypeOid::BOOL),
                ("indkey", TypeOid::INT2VECTOR),
                ("indisprimary", TypeOid::BOOL),
                ("indexprs", TypeOid::TEXT),
                ("indpred", TypeOid::TEXT),
            ],
            r,
        )
    }
    fn pg_constraint(&self) -> CatalogRelation {
        let r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                let namespace = self.namespace_oid(split_relation(&t.name).0);
                t.constraints.iter().enumerate().filter_map(move |(ci, c)| {
                    let k = match &c.kind {
                        ConstraintKind::PrimaryKey => "p",
                        ConstraintKind::Unique => "u",
                        ConstraintKind::Check => "c",
                        ConstraintKind::ForeignKey { .. } => "f",
                        _ => return None,
                    };
                    let conrelid = t.table_id.get() as u32;
                    let conkey: Vec<Value> = c
                        .columns
                        .iter()
                        .filter_map(|name| {
                            t.columns
                                .iter()
                                .position(|col| col.name == *name)
                                .map(|i| Value::Int2((i + 1) as i16))
                        })
                        .collect();
                    // FK extras: referenced table OID, referenced column
                    // numbers, and NO ACTION / MATCH SIMPLE markers.
                    let (confrelid, confkey_elements, confupdtype, confdeltype, confmatch) =
                        match &c.kind {
                            ConstraintKind::ForeignKey {
                                ref_table,
                                ref_columns,
                                on_delete,
                                on_update,
                                match_type,
                            } => {
                                let reloid = self
                                    .catalog
                                    .get_table(ref_table)
                                    .map(|rt| rt.table_id.get() as u32)
                                    .unwrap_or(0);
                                let ref_schema = self.catalog.get_table(ref_table).ok();
                                let nums: Vec<Value> = ref_columns
                                    .iter()
                                    .filter_map(|name| {
                                        ref_schema.and_then(|rs| {
                                            rs.columns
                                                .iter()
                                                .position(|col| {
                                                    col.name.eq_ignore_ascii_case(
                                                        crate::util::unqualify(name),
                                                    )
                                                })
                                                .map(|i| Value::Int2((i + 1) as i16))
                                        })
                                    })
                                    .collect();
                                (
                                    Value::Oid(reloid),
                                    nums,
                                    Value::BpChar(fk_action_char(*on_delete).to_string()),
                                    Value::BpChar(fk_action_char(*on_update).to_string()),
                                    Value::BpChar(fk_match_char(*match_type).to_string()),
                                )
                            }
                            _ => (
                                Value::Oid(0),
                                Vec::new(),
                                Value::BpChar(' '.into()),
                                Value::BpChar(' '.into()),
                                Value::BpChar(' '.into()),
                            ),
                        };
                    Some(vec![
                        Value::Oid(constraint_oid(conrelid, ci)),
                        Value::Text(
                            c.name
                                .clone()
                                .unwrap_or_else(|| format!("{}_constraint", t.name)),
                        ),
                        Value::Oid(namespace),
                        Value::Oid(conrelid),
                        Value::Text(k.into()),
                        Value::Bool(false),
                        Value::Bool(false),
                        Value::Bool(true),
                        Value::Bool(true),
                        Value::Int4(0),
                        Value::Array {
                            element_oid: TypeOid::INT2,
                            elements: conkey,
                        },
                        confrelid,
                        Value::Array {
                            element_oid: TypeOid::INT2,
                            elements: confkey_elements,
                        },
                        confupdtype,
                        confdeltype,
                        confmatch,
                    ])
                })
            })
            .collect();
        make_relation(
            "pg_constraint",
            &[
                ("oid", TypeOid::OID),
                ("conname", TypeOid::NAME),
                ("connamespace", TypeOid::OID),
                ("conrelid", TypeOid::OID),
                ("contype", TypeOid::CHAR),
                ("condeferrable", TypeOid::BOOL),
                ("condeferred", TypeOid::BOOL),
                ("convalidated", TypeOid::BOOL),
                ("conislocal", TypeOid::BOOL),
                ("coninhcount", TypeOid::INT4),
                ("conkey", TypeOid(1007)),
                ("confrelid", TypeOid::OID),
                ("confkey", TypeOid(1007)),
                ("confupdtype", TypeOid::CHAR),
                ("confdeltype", TypeOid::CHAR),
                ("confmatchtype", TypeOid::CHAR),
            ],
            r,
        )
    }

    fn pg_roles(&self) -> CatalogRelation {
        let r = self
            .catalog
            .roles()
            .into_iter()
            .map(|x| {
                vec![
                    Value::Oid(role_oid(&x.name)),
                    Value::Text(x.name.clone()),
                    Value::Bool(x.superuser),
                    Value::Bool(x.inherit),
                    Value::Bool(x.create_role),
                    Value::Bool(x.create_database),
                    Value::Bool(x.can_login),
                    Value::Bool(x.replication),
                    Value::Int4(x.connection_limit),
                    Value::Bool(x.superuser),
                ]
            })
            .collect();
        make_relation(
            "pg_roles",
            &[
                ("oid", TypeOid::OID),
                ("rolname", TypeOid::NAME),
                ("rolsuper", TypeOid::BOOL),
                ("rolinherit", TypeOid::BOOL),
                ("rolcreaterole", TypeOid::BOOL),
                ("rolcreatedb", TypeOid::BOOL),
                ("rolcanlogin", TypeOid::BOOL),
                ("rolreplication", TypeOid::BOOL),
                ("rolconnlimit", TypeOid::INT4),
                ("is_superuser", TypeOid::BOOL),
            ],
            r,
        )
    }
    fn pg_user(&self) -> CatalogRelation {
        let x = self
            .catalog
            .roles()
            .into_iter()
            .find(|x| x.name == self.current_user);
        let r = x
            .map(|x| {
                vec![
                    Value::Text(x.name.clone()),
                    Value::Oid(role_oid(&x.name)),
                    Value::Bool(x.create_database),
                    Value::Bool(x.superuser),
                    Value::Bool(x.can_login),
                ]
            })
            .unwrap_or_else(|| {
                vec![
                    Value::Text(self.current_user.into()),
                    Value::Oid(0),
                    Value::Bool(false),
                    Value::Bool(false),
                    Value::Bool(false),
                ]
            });
        make_relation(
            "pg_user",
            &[
                ("usename", TypeOid::NAME),
                ("usesysid", TypeOid::OID),
                ("usecreatedb", TypeOid::BOOL),
                ("usesuper", TypeOid::BOOL),
                ("userepl", TypeOid::BOOL),
            ],
            vec![r],
        )
    }
    fn pg_group(&self) -> CatalogRelation {
        let r = self
            .catalog
            .roles()
            .into_iter()
            .filter(|x| !x.members.is_empty())
            .map(|x| {
                vec![
                    Value::Text(x.name.clone()),
                    Value::Oid(role_oid(&x.name)),
                    Value::Null,
                ]
            })
            .collect();
        make_relation(
            "pg_group",
            &[
                ("groname", TypeOid::NAME),
                ("grosysid", TypeOid::OID),
                ("grolist", TypeOid(1028)),
            ],
            r,
        )
    }
    fn pg_tablespace(&self) -> CatalogRelation {
        make_relation(
            "pg_tablespace",
            &[
                ("oid", TypeOid::OID),
                ("spcname", TypeOid::NAME),
                ("spcowner", TypeOid::OID),
                ("spcpath", TypeOid::TEXT),
            ],
            vec![vec![
                Value::Oid(1663),
                Value::Text("pg_default".into()),
                Value::Oid(10),
                Value::Text("/plomid".into()),
            ]],
        )
    }
    fn pg_settings(&self) -> CatalogRelation {
        let rows = [
            ("server_version", "14.0"),
            ("server_version_num", "140000"),
            ("server_encoding", "UTF8"),
            ("client_encoding", "UTF8"),
            ("standard_conforming_strings", "on"),
            ("TimeZone", "UTC"),
            ("search_path", "\"$user\", public"),
        ]
        .into_iter()
        .map(|(name, value)| {
            vec![
                Value::Text(name.into()),
                Value::Text(value.into()),
                Value::Null,
            ]
        })
        .collect();
        make_relation(
            "pg_settings",
            &[
                ("name", TypeOid::NAME),
                ("setting", TypeOid::TEXT),
                ("unit", TypeOid::TEXT),
            ],
            rows,
        )
    }
    fn settings_function(&self) -> CatalogRelation {
        let rows = [
            ("server_version", "string"),
            ("server_version_num", "integer"),
            ("server_encoding", "string"),
            ("client_encoding", "string"),
            ("standard_conforming_strings", "bool"),
            ("TimeZone", "string"),
            ("search_path", "string"),
        ]
        .into_iter()
        .map(|(name, vartype)| {
            vec![
                Value::Text(name.into()),
                Value::Text(vartype.into()),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Text("user".into()),
            ]
        })
        .collect();
        make_relation(
            "pg_show_all_settings",
            &[
                ("name", TypeOid::TEXT),
                ("vartype", TypeOid::TEXT),
                ("min_val", TypeOid::NUMERIC),
                ("max_val", TypeOid::NUMERIC),
                ("enumvals", TypeOid(1009)),
                ("context", TypeOid::TEXT),
            ],
            rows,
        )
    }
    fn pg_tables(&self) -> CatalogRelation {
        let r = self
            .catalog
            .tables()
            .into_iter()
            .map(|t| {
                let (s, n) = split_relation(&t.name);
                vec![
                    Value::Text(s.into()),
                    Value::Text(n.into()),
                    Value::Text(self.current_user.into()),
                ]
            })
            .collect();
        make_relation(
            "pg_tables",
            &[
                ("schemaname", TypeOid::NAME),
                ("tablename", TypeOid::NAME),
                ("tableowner", TypeOid::NAME),
            ],
            r,
        )
    }
    fn pg_views(&self) -> CatalogRelation {
        let r = self
            .catalog
            .view_names()
            .into_iter()
            .map(|v| {
                let (s, n) = split_relation(&v);
                vec![
                    Value::Text(s.into()),
                    Value::Text(n.into()),
                    Value::Text(self.current_user.into()),
                    Value::Text(
                        self.catalog
                            .get_view(&v)
                            .map_or(String::new(), |view| view.definition.clone()),
                    ),
                    Value::Text("NONE".into()),
                    Value::Text("NO".into()),
                    Value::Text("NO".into()),
                ]
            })
            .collect();
        make_relation(
            "pg_views",
            &[
                ("schemaname", TypeOid::NAME),
                ("viewname", TypeOid::NAME),
                ("viewowner", TypeOid::NAME),
                ("definition", TypeOid::TEXT),
                ("check_option", TypeOid::NAME),
                ("is_updatable", TypeOid::NAME),
                ("is_insertable_into", TypeOid::NAME),
            ],
            r,
        )
    }
    fn pg_indexes(&self) -> CatalogRelation {
        let mut r = self
            .catalog
            .indexes()
            .into_iter()
            .map(|i| {
                let (s, n) = split_relation(&i.table);
                vec![
                    Value::Text(s.into()),
                    Value::Text(n.into()),
                    Value::Text(i.name),
                    Value::Null,
                    Value::Text(
                        index_definition(self.catalog, i.index_id.get() as u32).unwrap_or_default(),
                    ),
                ]
            })
            .collect::<Vec<_>>();
        for t in self.catalog.tables() {
            let (schema, table_name) = split_relation(&t.name);
            for constraint in &t.constraints {
                if !matches!(
                    constraint.kind,
                    ConstraintKind::PrimaryKey | ConstraintKind::Unique
                ) {
                    continue;
                }
                let index_name = constraint_index_name(&t.name, constraint);
                let columns = constraint.columns.join(", ");
                r.push(vec![
                    Value::Text(schema.into()),
                    Value::Text(table_name.into()),
                    Value::Text(index_name.clone()),
                    Value::Null,
                    Value::Text(format!(
                        "CREATE {}INDEX {} ON {} USING btree ({})",
                        if constraint.kind == ConstraintKind::Unique {
                            "UNIQUE "
                        } else {
                            ""
                        },
                        index_name,
                        t.name,
                        columns
                    )),
                ]);
            }
        }
        make_relation(
            "pg_indexes",
            &[
                ("schemaname", TypeOid::NAME),
                ("tablename", TypeOid::NAME),
                ("indexname", TypeOid::NAME),
                ("tablespace", TypeOid::NAME),
                ("indexdef", TypeOid::TEXT),
            ],
            r,
        )
    }
    fn schemata(&self) -> CatalogRelation {
        let r = self
            .all_schema_names()
            .into_iter()
            .map(|s| {
                vec![
                    Value::Text(self.current_database.into()),
                    Value::Text(s),
                    Value::Text(self.current_user.into()),
                ]
            })
            .collect();
        make_relation(
            "schemata",
            &[
                ("catalog_name", TypeOid::NAME),
                ("schema_name", TypeOid::NAME),
                ("schema_owner", TypeOid::NAME),
            ],
            r,
        )
    }
    fn info_tables(&self) -> CatalogRelation {
        let mut r = self
            .catalog
            .tables()
            .into_iter()
            .map(|t| {
                let (s, n) = split_relation(&t.name);
                vec![
                    Value::Text(self.current_database.into()),
                    Value::Text(s.into()),
                    Value::Text(n.into()),
                    Value::Text("BASE TABLE".into()),
                ]
            })
            .collect::<Vec<_>>();
        r.extend(self.catalog.view_names().into_iter().map(|v| {
            let (s, n) = split_relation(&v);
            vec![
                Value::Text(self.current_database.into()),
                Value::Text(s.into()),
                Value::Text(n.into()),
                Value::Text("VIEW".into()),
            ]
        }));
        make_relation(
            "tables",
            &[
                ("table_catalog", TypeOid::NAME),
                ("table_schema", TypeOid::NAME),
                ("table_name", TypeOid::NAME),
                ("table_type", TypeOid::TEXT),
            ],
            r,
        )
    }
    fn info_columns(&self) -> CatalogRelation {
        let mut r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                let (s, n) = split_relation(&t.name);
                t.columns.iter().enumerate().map(move |(i, c)| {
                    vec![
                        Value::Text(self.current_database.into()),
                        Value::Text(s.into()),
                        Value::Text(n.into()),
                        Value::Text(c.name.clone()),
                        Value::Int4((i + 1) as i32),
                        t.default_expr(i)
                            .map_or(Value::Null, expression_catalog_text),
                        Value::Text(if t.is_not_null(i) { "NO" } else { "YES" }.into()),
                        Value::Text(column_type_name(c.col_type.type_oid)),
                        nullable_i32(character_length(c.col_type)),
                        nullable_i32(character_length(c.col_type).map(|n| n.saturating_mul(4))),
                        nullable_i32(numeric_precision(&c.col_type)),
                        nullable_i32(numeric_radix(&c.col_type)),
                        nullable_i32(numeric_scale(&c.col_type)),
                        nullable_i32(datetime_precision(c.col_type.type_oid)),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Text(self.current_database.into()),
                        Value::Text("pg_catalog".into()),
                        Value::Text(column_type_name(c.col_type.type_oid)),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Text((i + 1).to_string()),
                        Value::Text("NO".into()),
                        Value::Text("NO".into()),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Text("NO".into()),
                        Value::Text("NEVER".into()),
                        Value::Null,
                        Value::Text("YES".into()),
                    ]
                })
            })
            .collect::<Vec<_>>();
        for view_name in self.catalog.view_names() {
            let (schema, relation) = split_relation(&view_name);
            if let Some(view) = self.catalog.get_view(&view_name) {
                for (index, column) in view.columns.iter().enumerate() {
                    let col_type = view
                        .column_type(index)
                        .map(|ct| ct.type_oid)
                        .unwrap_or(TypeOid::TEXT);
                    r.push(info_schema_column_row(
                        self.current_database,
                        schema,
                        relation,
                        column,
                        index,
                        Value::Null,
                        false,
                        col_type,
                    ));
                }
            }
        }
        make_relation(
            "columns",
            &[
                ("table_catalog", TypeOid::NAME),
                ("table_schema", TypeOid::NAME),
                ("table_name", TypeOid::NAME),
                ("column_name", TypeOid::NAME),
                ("ordinal_position", TypeOid::INT4),
                ("column_default", TypeOid::TEXT),
                ("is_nullable", TypeOid::VARCHAR),
                ("data_type", TypeOid::TEXT),
                ("character_maximum_length", TypeOid::INT4),
                ("character_octet_length", TypeOid::INT4),
                ("numeric_precision", TypeOid::INT4),
                ("numeric_precision_radix", TypeOid::INT4),
                ("numeric_scale", TypeOid::INT4),
                ("datetime_precision", TypeOid::INT4),
                ("interval_type", TypeOid::TEXT),
                ("interval_precision", TypeOid::INT4),
                ("character_set_catalog", TypeOid::NAME),
                ("character_set_schema", TypeOid::NAME),
                ("character_set_name", TypeOid::NAME),
                ("collation_catalog", TypeOid::NAME),
                ("collation_schema", TypeOid::NAME),
                ("collation_name", TypeOid::NAME),
                ("domain_catalog", TypeOid::NAME),
                ("domain_schema", TypeOid::NAME),
                ("domain_name", TypeOid::NAME),
                ("udt_catalog", TypeOid::NAME),
                ("udt_schema", TypeOid::NAME),
                ("udt_name", TypeOid::NAME),
                ("scope_catalog", TypeOid::NAME),
                ("scope_schema", TypeOid::NAME),
                ("scope_name", TypeOid::NAME),
                ("maximum_cardinality", TypeOid::INT4),
                ("dtd_identifier", TypeOid::TEXT),
                ("is_self_referencing", TypeOid::VARCHAR),
                ("is_identity", TypeOid::VARCHAR),
                ("identity_generation", TypeOid::VARCHAR),
                ("identity_start", TypeOid::VARCHAR),
                ("identity_increment", TypeOid::VARCHAR),
                ("identity_maximum", TypeOid::VARCHAR),
                ("identity_minimum", TypeOid::VARCHAR),
                ("identity_cycle", TypeOid::VARCHAR),
                ("is_generated", TypeOid::VARCHAR),
                ("generation_expression", TypeOid::TEXT),
                ("is_updatable", TypeOid::VARCHAR),
            ],
            r,
        )
    }
    fn info_views(&self) -> CatalogRelation {
        let r = self
            .catalog
            .view_names()
            .into_iter()
            .map(|v| {
                let (s, n) = split_relation(&v);
                vec![
                    Value::Text(self.current_database.into()),
                    Value::Text(s.into()),
                    Value::Text(n.into()),
                    Value::Text(
                        self.catalog
                            .get_view(&v)
                            .map_or(String::new(), |view| view.definition.clone()),
                    ),
                    Value::Text("NONE".into()),
                    Value::Text("NO".into()),
                    Value::Text("NO".into()),
                    Value::Text("NO".into()),
                    Value::Text("NO".into()),
                ]
            })
            .collect();
        make_relation(
            "views",
            &[
                ("table_catalog", TypeOid::NAME),
                ("table_schema", TypeOid::NAME),
                ("table_name", TypeOid::NAME),
                ("view_definition", TypeOid::TEXT),
                ("check_option", TypeOid::VARCHAR),
                ("is_updatable", TypeOid::VARCHAR),
                ("insertable_into", TypeOid::VARCHAR),
                ("is_trigger_updatable", TypeOid::VARCHAR),
                ("is_trigger_deletable", TypeOid::VARCHAR),
                ("is_trigger_insertable_into", TypeOid::VARCHAR),
            ],
            r,
        )
    }
    fn table_constraints(&self) -> CatalogRelation {
        let r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                let (s, n) = split_relation(&t.name);
                t.constraints.iter().filter_map(move |c| {
                    let k = match &c.kind {
                        ConstraintKind::PrimaryKey => "PRIMARY KEY",
                        ConstraintKind::Unique => "UNIQUE",
                        ConstraintKind::Check => "CHECK",
                        ConstraintKind::ForeignKey { .. } => "FOREIGN KEY",
                        _ => return None,
                    };
                    Some(vec![
                        Value::Text(self.current_database.into()),
                        Value::Text(s.into()),
                        Value::Text(
                            c.name
                                .clone()
                                .unwrap_or_else(|| format!("{}_constraint", n)),
                        ),
                        Value::Text(self.current_database.into()),
                        Value::Text(s.into()),
                        Value::Text(n.into()),
                        Value::Text(k.into()),
                        Value::Text("NO".into()),
                        Value::Text("NO".into()),
                        Value::Text("YES".into()),
                    ])
                })
            })
            .collect();
        make_relation(
            "table_constraints",
            &[
                ("constraint_catalog", TypeOid::NAME),
                ("constraint_schema", TypeOid::NAME),
                ("constraint_name", TypeOid::NAME),
                ("table_catalog", TypeOid::NAME),
                ("table_schema", TypeOid::NAME),
                ("table_name", TypeOid::NAME),
                ("constraint_type", TypeOid::TEXT),
                ("is_deferrable", TypeOid::VARCHAR),
                ("initially_deferred", TypeOid::VARCHAR),
                ("enforced", TypeOid::VARCHAR),
            ],
            r,
        )
    }
    fn key_columns(&self) -> CatalogRelation {
        let r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                let (s, n) = split_relation(&t.name);
                t.constraints
                    .iter()
                    .filter(|c| {
                        matches!(c.kind, ConstraintKind::PrimaryKey | ConstraintKind::Unique)
                    })
                    .flat_map(move |c| {
                        c.columns.iter().enumerate().map(move |(i, col)| {
                            vec![
                                Value::Text(self.current_database.into()),
                                Value::Text(s.into()),
                                Value::Text(
                                    c.name
                                        .clone()
                                        .unwrap_or_else(|| format!("{}_constraint", n)),
                                ),
                                Value::Text(self.current_database.into()),
                                Value::Text(s.into()),
                                Value::Text(n.into()),
                                Value::Text(col.clone()),
                                Value::Int4((i + 1) as i32),
                                Value::Null,
                            ]
                        })
                    })
            })
            .collect();
        make_relation(
            "key_column_usage",
            &[
                ("constraint_catalog", TypeOid::NAME),
                ("constraint_schema", TypeOid::NAME),
                ("constraint_name", TypeOid::NAME),
                ("table_catalog", TypeOid::NAME),
                ("table_schema", TypeOid::NAME),
                ("table_name", TypeOid::NAME),
                ("column_name", TypeOid::NAME),
                ("ordinal_position", TypeOid::INT4),
                ("position_in_unique_constraint", TypeOid::INT4),
            ],
            r,
        )
    }
    fn referential_constraints(&self) -> CatalogRelation {
        let r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                let (s, _n) = split_relation(&t.name);
                t.constraints.iter().filter_map(move |c| {
                    let (ref_table, on_update, on_delete, mtype) = match &c.kind {
                        ConstraintKind::ForeignKey {
                            ref_table,
                            on_update,
                            on_delete,
                            match_type,
                            ..
                        } => (ref_table, *on_update, *on_delete, *match_type),
                        _ => return None,
                    };
                    let (rs, rn) = split_relation(ref_table);
                    let cname = c.name.clone().unwrap_or_else(|| format!("{}_fk", t.name));
                    Some(vec![
                        Value::Text(self.current_database.into()),
                        Value::Text(s.into()),
                        Value::Text(cname),
                        Value::Text(self.current_database.into()),
                        Value::Text(rs.into()),
                        Value::Text(rn.into()),
                        // match_option: PostgreSQL uses FULL, PARTIAL, SIMPLE (SIMPLE shown as NONE)
                        Value::Text(
                            match mtype {
                                ForeignKeyMatch::Full => "FULL",
                                ForeignKeyMatch::Partial => "PARTIAL",
                                ForeignKeyMatch::Simple => "NONE",
                            }
                            .into(),
                        ),
                        // update_rule
                        Value::Text(
                            match on_update {
                                ForeignKeyAction::NoAction => "NO ACTION",
                                ForeignKeyAction::Restrict => "RESTRICT",
                                ForeignKeyAction::Cascade => "CASCADE",
                                ForeignKeyAction::SetNull => "SET NULL",
                                ForeignKeyAction::SetDefault => "SET DEFAULT",
                            }
                            .into(),
                        ),
                        // delete_rule
                        Value::Text(
                            match on_delete {
                                ForeignKeyAction::NoAction => "NO ACTION",
                                ForeignKeyAction::Restrict => "RESTRICT",
                                ForeignKeyAction::Cascade => "CASCADE",
                                ForeignKeyAction::SetNull => "SET NULL",
                                ForeignKeyAction::SetDefault => "SET DEFAULT",
                            }
                            .into(),
                        ),
                    ])
                })
            })
            .collect();
        make_relation(
            "referential_constraints",
            &[
                ("constraint_catalog", TypeOid::NAME),
                ("constraint_schema", TypeOid::NAME),
                ("constraint_name", TypeOid::NAME),
                ("unique_constraint_catalog", TypeOid::NAME),
                ("unique_constraint_schema", TypeOid::NAME),
                ("unique_constraint_name", TypeOid::NAME),
                ("match_option", TypeOid::VARCHAR),
                ("update_rule", TypeOid::VARCHAR),
                ("delete_rule", TypeOid::VARCHAR),
            ],
            r,
        )
    }
    fn constraint_column_usage(&self) -> CatalogRelation {
        let r = self
            .catalog
            .tables()
            .into_iter()
            .flat_map(|t| {
                let (s, _n) = split_relation(&t.name);
                t.constraints.iter().filter_map(move |c| {
                    let (ref_table, ref_columns) = match &c.kind {
                        ConstraintKind::ForeignKey {
                            ref_table,
                            ref_columns,
                            ..
                        } => (ref_table, ref_columns),
                        _ => return None,
                    };
                    let (rs, rn) = split_relation(ref_table);
                    let cname = c.name.clone().unwrap_or_else(|| format!("{}_fk", t.name));
                    let rows: Vec<Vec<Value>> = ref_columns
                        .iter()
                        .map(|col| {
                            vec![
                                Value::Text(self.current_database.into()),
                                Value::Text(s.into()),
                                Value::Text(cname.clone()),
                                Value::Text(self.current_database.into()),
                                Value::Text(rs.into()),
                                Value::Text(rn.into()),
                                Value::Text(col.clone()),
                            ]
                        })
                        .collect();
                    Some(rows)
                })
            })
            .flat_map(|v: Vec<Vec<Value>>| v)
            .collect();
        make_relation(
            "constraint_column_usage",
            &[
                ("constraint_catalog", TypeOid::NAME),
                ("constraint_schema", TypeOid::NAME),
                ("constraint_name", TypeOid::NAME),
                ("table_catalog", TypeOid::NAME),
                ("table_schema", TypeOid::NAME),
                ("table_name", TypeOid::NAME),
                ("column_name", TypeOid::NAME),
            ],
            r,
        )
    }
    fn sequences(&self) -> CatalogRelation {
        let r = self
            .catalog
            .sequence_names()
            .into_iter()
            .map(|x| {
                let (s, n) = split_relation(&x);
                vec![
                    Value::Text(self.current_database.into()),
                    Value::Text(s.into()),
                    Value::Text(n.into()),
                ]
            })
            .collect();
        make_relation(
            "sequences",
            &[
                ("sequence_catalog", TypeOid::NAME),
                ("sequence_schema", TypeOid::NAME),
                ("sequence_name", TypeOid::NAME),
            ],
            r,
        )
    }
    fn pg_sequence(&self) -> CatalogRelation {
        let rows = self
            .catalog
            .sequence_names()
            .into_iter()
            .map(|name| {
                let relid = self.relation_oid(&name);
                vec![
                    Value::Oid(relid),
                    Value::Int8(1),
                    Value::Int8(1),
                    Value::Int8(i64::MAX),
                    Value::Int8(1),
                    Value::Int8(1),
                    Value::Bool(false),
                    Value::Oid(TypeOid::INT8.0),
                    Value::Oid(role_oid(self.current_user)),
                ]
            })
            .collect();
        make_relation(
            "pg_sequence",
            &[
                ("seqrelid", TypeOid::OID),
                ("seqstart", TypeOid::INT8),
                ("seqincrement", TypeOid::INT8),
                ("seqmax", TypeOid::INT8),
                ("seqmin", TypeOid::INT8),
                ("seqcache", TypeOid::INT8),
                ("seqcycle", TypeOid::BOOL),
                ("seqtypid", TypeOid::OID),
                ("seqowner", TypeOid::OID),
            ],
            rows,
        )
    }
    fn pg_sequences(&self) -> CatalogRelation {
        let rows = self
            .catalog
            .sequence_names()
            .into_iter()
            .map(|name| {
                let (schema, relation) = split_relation(&name);
                vec![
                    Value::Text(schema.into()),
                    Value::Text(relation.into()),
                    Value::Text(self.current_user.into()),
                    Value::Oid(TypeOid::INT8.0),
                    Value::Int8(1),
                    Value::Int8(1),
                    Value::Int8(i64::MAX),
                    Value::Int8(1),
                    Value::Bool(false),
                    Value::Int8(1),
                    Value::Int8(0),
                ]
            })
            .collect();
        make_relation(
            "pg_sequences",
            &[
                ("schemaname", TypeOid::NAME),
                ("sequencename", TypeOid::NAME),
                ("sequenceowner", TypeOid::NAME),
                ("data_type", TypeOid::REGTYPE),
                ("start_value", TypeOid::INT8),
                ("min_value", TypeOid::INT8),
                ("max_value", TypeOid::INT8),
                ("increment_by", TypeOid::INT8),
                ("cycle", TypeOid::BOOL),
                ("cache_size", TypeOid::INT8),
                ("last_value", TypeOid::INT8),
            ],
            rows,
        )
    }
    fn routines(&self) -> CatalogRelation {
        const BUILTINS: &[(&str, &str)] = &[
            ("format", "text"),
            ("col_description", "text"),
            ("obj_description", "text"),
            ("pg_total_relation_size", "bigint"),
            ("row_to_json", "json"),
            ("nextval", "bigint"),
            ("currval", "bigint"),
            ("pg_show_all_settings", "record"),
        ];
        let rows = BUILTINS
            .iter()
            .map(|(name, data_type)| {
                vec![
                    Value::Text(self.current_database.into()),
                    Value::Text("pg_catalog".into()),
                    Value::Text((*name).into()),
                    Value::Text(format!("{}_0", name)),
                    Value::Text("FUNCTION".into()),
                    Value::Text((*data_type).into()),
                    Value::Text(self.current_database.into()),
                    Value::Text("pg_catalog".into()),
                    Value::Text((*data_type).into()),
                ]
            })
            .collect();
        make_relation(
            "routines",
            &[
                ("routine_catalog", TypeOid::NAME),
                ("routine_schema", TypeOid::NAME),
                ("routine_name", TypeOid::NAME),
                ("specific_name", TypeOid::NAME),
                ("routine_type", TypeOid::VARCHAR),
                ("data_type", TypeOid::TEXT),
                ("type_udt_catalog", TypeOid::NAME),
                ("type_udt_schema", TypeOid::NAME),
                ("type_udt_name", TypeOid::NAME),
            ],
            rows,
        )
    }
    fn namespace_oid(&self, n: &str) -> u32 {
        if n == "pg_catalog" {
            11
        } else if n == "information_schema" {
            13207
        } else if n == "public" {
            2200
        } else {
            if let Some(id) = self.catalog.schema_id(n) {
                return id.get() as u32;
            }
            let mut hash = 3_000_000u32;
            for byte in n.as_bytes() {
                hash = hash.wrapping_mul(16777619) ^ u32::from(*byte);
            }
            hash | 0x0300_0000
        }
    }
    fn relation_oid(&self, name: &str) -> u32 {
        if let Ok(table) = self.catalog.get_table(name) {
            return table.table_id.get() as u32;
        }
        if let Some(position) = self.catalog.view_names().iter().position(|x| x == name) {
            return 2_000_000 + position as u32;
        }
        // Sequence identifiers are not currently persisted as separate
        // objects, so derive a stable non-overlapping identifier from the
        // fully-qualified name.  This is deterministic across catalog reads
        // and does not depend on query order.
        let mut hash = 2_500_000u32;
        for byte in name.as_bytes() {
            hash = hash.wrapping_mul(16777619) ^ u32::from(*byte);
        }
        hash | 0x0100_0000
    }
    fn view_oid(&self, n: &str) -> u32 {
        let mut hash = 2_000_000u32;
        for byte in n.as_bytes() {
            hash = hash.wrapping_mul(16777619) ^ u32::from(*byte);
        }
        hash | 0x0200_0000
    }
}

fn fk_action_char(action: plomid_sql::ForeignKeyAction) -> char {
    match action {
        plomid_sql::ForeignKeyAction::NoAction => 'a',
        plomid_sql::ForeignKeyAction::Restrict => 'r',
        plomid_sql::ForeignKeyAction::Cascade => 'c',
        plomid_sql::ForeignKeyAction::SetNull => 'n',
        plomid_sql::ForeignKeyAction::SetDefault => 'd',
    }
}

fn fk_match_char(kind: plomid_sql::ForeignKeyMatch) -> char {
    match kind {
        plomid_sql::ForeignKeyMatch::Simple => 's',
        plomid_sql::ForeignKeyMatch::Full => 'f',
        plomid_sql::ForeignKeyMatch::Partial => 'p',
    }
}

fn make_relation(
    name: &str,
    columns: &[(&str, TypeOid)],
    rows: Vec<Vec<Value>>,
) -> CatalogRelation {
    CatalogRelation {
        schema: TableSchema {
            name: name.into(),
            table_id: TableId::new(0),
            column_ids: (1..=columns.len())
                .map(|x| ColumnId::new(x as u64))
                .collect(),
            columns: columns
                .iter()
                .map(|(n, o)| ColumnDef {
                    name: (*n).into(),
                    col_type: ColumnType::new(*o, NO_TYPEMOD),
                    constraints: Vec::new(),
                })
                .collect(),
            constraints: Vec::new(),
        },
        rows,
    }
}

#[allow(clippy::too_many_arguments)]
fn info_schema_column_row(
    database: &str,
    schema: &str,
    table: &str,
    column: &str,
    index: usize,
    default_value: Value,
    nullable: bool,
    type_oid: TypeOid,
) -> Vec<Value> {
    vec![
        Value::Text(database.into()),
        Value::Text(schema.into()),
        Value::Text(table.into()),
        Value::Text(column.into()),
        Value::Int4((index + 1) as i32),
        default_value,
        Value::Text(if nullable { "YES" } else { "NO" }.into()),
        Value::Text(column_type_name(type_oid)),
        nullable_i32(character_length(ColumnType::new(type_oid, NO_TYPEMOD))),
        nullable_i32(
            character_length(ColumnType::new(type_oid, NO_TYPEMOD)).map(|n| n.saturating_mul(4)),
        ),
        nullable_i32(numeric_precision(&ColumnType::new(type_oid, NO_TYPEMOD))),
        nullable_i32(numeric_radix(&ColumnType::new(type_oid, NO_TYPEMOD))),
        nullable_i32(numeric_scale(&ColumnType::new(type_oid, NO_TYPEMOD))),
        nullable_i32(datetime_precision(type_oid)),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Text(database.into()),
        Value::Text("pg_catalog".into()),
        Value::Text(column_type_name(type_oid)),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Text((index + 1).to_string()),
        Value::Text("NO".into()),
        Value::Text("NO".into()),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Text("NO".into()),
        Value::Text("NEVER".into()),
        Value::Null,
        Value::Text("YES".into()),
    ]
}

/// Normalizes a schema-qualified relation reference for case-insensitive
/// catalog lookup.  PostgreSQL folds unquoted identifiers to lowercase, so
/// `INFORMATION_SCHEMA.ROUTINES` and `information_schema.routines` must
/// resolve to the same relation.  System and information-schema views are
/// matched by their bare (lowercased) table name, so the leading schema
/// qualifier is stripped: `pg_catalog.pg_class`, `public.pg_tables`, and
/// `INFORMATION_SCHEMA.ROUTINES` all resolve to `pg_class`, `pg_tables`,
/// and `routines`.
fn normalize_relation(name: &str) -> String {
    rsplit_once_to_table(name).to_ascii_lowercase()
}

/// Returns the portion of `name` after the last `.` (or the whole string
/// when unqualified), preserving the original case.
fn rsplit_once_to_table(name: &str) -> &str {
    name.rsplit_once('.')
        .map(|(_, table)| table)
        .unwrap_or(name)
}

fn column_type_name(oid: TypeOid) -> String {
    if let Some(ty) = PgType::by_oid(oid) {
        return ty.name().to_string();
    }
    PgType::all()
        .find(|ty| ty.array_oid() == Some(oid))
        .map(|ty| format!("{}[]", ty.name()))
        .unwrap_or_else(|| "unknown".to_string())
}
fn type_length(oid: TypeOid) -> i16 {
    match oid {
        TypeOid::BOOL | TypeOid::CHAR => 1,
        TypeOid::INT2 => 2,
        TypeOid::INT4 | TypeOid::OID | TypeOid::FLOAT4 | TypeOid::DATE => 4,
        TypeOid::INT8
        | TypeOid::FLOAT8
        | TypeOid::TIMESTAMP
        | TypeOid::TIMESTAMPTZ
        | TypeOid::TIME => 8,
        TypeOid::UUID => 16,
        _ => -1,
    }
}
fn type_by_value(oid: TypeOid) -> bool {
    type_length(oid) > 0 && type_length(oid) <= 8
}
fn type_alignment(oid: TypeOid) -> char {
    match type_length(oid) {
        2 => 's',
        4 => 'i',
        8 => 'd',
        _ => 'i',
    }
}
fn expression_catalog_text(expression: &plomid_sql::Expression) -> Value {
    match expression {
        plomid_sql::Expression::Literal(value) => Value::Text(value.to_sql_text()),
        _ => Value::Null,
    }
}
fn nullable_i32(value: Option<i32>) -> Value {
    value.map_or(Value::Null, Value::Int4)
}

/// Deterministic synthetic OID for the constraint at `index` of `conrelid`,
/// shared by the `pg_constraint` view and `pg_get_constraintdef()`.
#[must_use]
pub fn constraint_oid(conrelid: u32, index: usize) -> u32 {
    conrelid.wrapping_mul(1000).wrapping_add(index as u32 + 1)
}

/// Stable relation OID for the index PostgreSQL would create for a primary
/// key or unique constraint. Constraint indexes are catalog metadata even
/// when the storage engine does not expose them as user-created indexes.
#[must_use]
fn constraint_index_oid(relid: u32, index: usize) -> u32 {
    0x4000_0000 | relid.wrapping_mul(1000).wrapping_add(index as u32 + 1)
}

fn constraint_index_name(table: &str, constraint: &plomid_sql::Constraint) -> String {
    constraint.name.clone().unwrap_or_else(|| {
        let relation = table.rsplit('.').next().unwrap_or(table);
        format!(
            "{}_{}",
            relation,
            if constraint.kind == ConstraintKind::PrimaryKey {
                "pkey"
            } else {
                "key"
            }
        )
    })
}

/// `pg_get_constraintdef(constraint_oid) -> text`.
#[must_use]
pub fn constraint_definition(catalog: &InMemoryCatalog, oid: u32) -> Option<String> {
    for t in catalog.tables() {
        let conrelid = t.table_id.get() as u32;
        for (ci, c) in t.constraints.iter().enumerate() {
            if constraint_oid(conrelid, ci) != oid {
                continue;
            }
            return match &c.kind {
                ConstraintKind::PrimaryKey => {
                    Some(format!("PRIMARY KEY ({})", c.columns.join(", ")))
                }
                ConstraintKind::Unique => Some(format!("UNIQUE ({})", c.columns.join(", "))),
                ConstraintKind::Check => c
                    .expr
                    .as_ref()
                    .map(|expr| format!("CHECK ({})", render_expression_sql(expr))),
                ConstraintKind::ForeignKey {
                    ref_table,
                    ref_columns,
                    ..
                } => Some(format!(
                    "FOREIGN KEY ({}) REFERENCES {}({})",
                    c.columns.join(", "),
                    ref_table,
                    ref_columns.join(", ")
                )),
                _ => None,
            };
        }
    }
    None
}

/// `pg_get_indexdef(index_oid) -> text`.
#[must_use]
pub fn index_definition(catalog: &InMemoryCatalog, oid: u32) -> Option<String> {
    if let Some(definition) = catalog.indexes().into_iter().find_map(|i| {
        if i.index_id.get() as u32 == oid {
            Some(format!(
                "CREATE {}INDEX {} ON {} USING btree ({})",
                if i.unique { "UNIQUE " } else { "" },
                i.name,
                i.table,
                i.column
            ))
        } else {
            None
        }
    }) {
        return Some(definition);
    }
    for table in catalog.tables() {
        for (index, constraint) in table.constraints.iter().enumerate() {
            if constraint_index_oid(table.table_id.get() as u32, index) == oid
                && matches!(
                    constraint.kind,
                    ConstraintKind::PrimaryKey | ConstraintKind::Unique
                )
            {
                return Some(format!(
                    "CREATE {}INDEX {} ON {} USING btree ({})",
                    if constraint.kind == ConstraintKind::Unique {
                        "UNIQUE "
                    } else {
                        ""
                    },
                    constraint_index_name(&table.name, constraint),
                    table.name,
                    constraint.columns.join(", ")
                ));
            }
        }
    }
    None
}

/// Renders a parsed expression back to PostgreSQL SQL text for catalog
/// definitions (`pg_get_constraintdef`). Handles the common constraint
/// expression forms; exotic nodes render as `?`.
fn render_expression_sql(e: &plomid_sql::Expression) -> String {
    use plomid_sql::Expression as E;
    let atom = |x: &E| -> String {
        match x {
            E::Literal(_) | E::ColumnRef(_) => render_expression_sql(x),
            other => format!("({})", render_expression_sql(other)),
        }
    };
    let pair = |a: &E, op: &str, b: &E| format!("{} {} {}", atom(a), op, atom(b));
    match e {
        E::Literal(v) => v.to_sql_text(),
        E::ColumnRef(n) => n.clone(),
        E::Equal(a, b) => pair(a, "=", b),
        E::NotEqual(a, b) => pair(a, "<>", b),
        E::Less(a, b) => pair(a, "<", b),
        E::LessOrEqual(a, b) => pair(a, "<=", b),
        E::Greater(a, b) => pair(a, ">", b),
        E::GreaterOrEqual(a, b) => pair(a, ">=", b),
        E::And(a, b) => pair(a, "AND", b),
        E::Or(a, b) => pair(a, "OR", b),
        E::Add(a, b) => pair(a, "+", b),
        E::Subtract(a, b) => pair(a, "-", b),
        E::Multiply(a, b) => pair(a, "*", b),
        E::Divide(a, b) => pair(a, "/", b),
        E::Modulo(a, b) => pair(a, "%", b),
        E::Concat(a, b) => pair(a, "||", b),
        E::Power(a, b) => pair(a, "^", b),
        E::IsDistinctFrom(a, b) => pair(a, "IS DISTINCT FROM", b),
        E::IsNull(x) => format!("{} IS NULL", atom(x)),
        E::IsNotNull(x) => format!("{} IS NOT NULL", atom(x)),
        E::Not(x) => format!("NOT {}", atom(x)),
        E::Negate(x) => format!("-{}", atom(x)),
        E::In {
            expr,
            list,
            negated,
            ..
        } => format!(
            "{} {}IN ({})",
            atom(expr),
            if *negated { "NOT " } else { "" },
            list.iter()
                .map(render_expression_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        E::Between {
            expr,
            low,
            high,
            negated,
        } => format!(
            "{} {}BETWEEN {} AND {}",
            atom(expr),
            if *negated { "NOT " } else { "" },
            atom(low),
            atom(high)
        ),
        E::Like {
            expr,
            pattern,
            negated,
            ..
        } => format!(
            "{} {}LIKE {}",
            atom(expr),
            if *negated { "NOT " } else { "" },
            atom(pattern)
        ),
        E::Cast { expr, type_name } | E::TypeCast { expr, type_name } => {
            format!("{}::{}", atom(expr), type_name)
        }
        E::FunctionCall { name, args, .. } => format!(
            "{}({})",
            name,
            args.iter()
                .map(render_expression_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        E::Coalesce(args) => format!(
            "COALESCE({})",
            args.iter()
                .map(render_expression_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        E::NullIf(a, b) => format!(
            "NULLIF({}, {})",
            render_expression_sql(a),
            render_expression_sql(b)
        ),
        E::Extract { field, expr } => {
            format!("EXTRACT({field} FROM {})", render_expression_sql(expr))
        }
        _ => "?".to_string(),
    }
}
fn character_length(column: ColumnType) -> Option<i32> {
    match column.type_oid {
        TypeOid::VARCHAR | TypeOid::BPCHAR if column.typmod >= 4 => Some(column.typmod - 4),
        _ => None,
    }
}
fn numeric_precision(col: &ColumnType) -> Option<i32> {
    match col.type_oid {
        TypeOid::INT2 => Some(16),
        TypeOid::INT4 => Some(32),
        TypeOid::INT8 => Some(64),
        TypeOid::FLOAT4 => Some(24),
        TypeOid::FLOAT8 => Some(53),
        TypeOid::NUMERIC => {
            // typmod = (precision << 16) | scale; NO_TYPEMOD means unconstrained.
            if col.typmod == plomid_types::typmod::NO_TYPEMOD {
                None
            } else {
                Some(col.typmod >> 16)
            }
        }
        _ => None,
    }
}
fn numeric_scale(col: &ColumnType) -> Option<i32> {
    match col.type_oid {
        TypeOid::NUMERIC if col.typmod != plomid_types::typmod::NO_TYPEMOD => {
            Some(col.typmod & 0xFFFF)
        }
        _ => None,
    }
}
fn numeric_radix(col: &ColumnType) -> Option<i32> {
    numeric_precision(col).map(|_| match col.type_oid {
        TypeOid::NUMERIC => 10,
        _ => 2,
    })
}
fn datetime_precision(oid: TypeOid) -> Option<i32> {
    matches!(
        oid,
        TypeOid::TIME | TypeOid::TIMETZ | TypeOid::TIMESTAMP | TypeOid::TIMESTAMPTZ
    )
    .then_some(6)
}
fn split_relation(n: &str) -> (&str, &str) {
    n.split_once('.').unwrap_or(("public", n))
}
fn role_oid(n: &str) -> u32 {
    if n.eq_ignore_ascii_case("plomid") {
        10
    } else {
        10_000
            + n.bytes()
                .fold(0u32, |a, b| a.wrapping_mul(33).wrapping_add(b as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_relation_handles_case_insensitive_schema() {
        assert!(SystemCatalog::is_relation("information_schema.tables"));
        assert!(SystemCatalog::is_relation("INFORMATION_SCHEMA.TABLES"));
        assert!(SystemCatalog::is_relation("Information_Schema.Tables"));
        assert!(SystemCatalog::is_relation("pg_catalog.pg_class"));
        assert!(SystemCatalog::is_relation("PG_CATALOG.PG_CLASS"));
        assert!(SystemCatalog::is_relation("INFORMATION_SCHEMA.ROUTINES"));
        // pg_default_acl should be recognized as a system catalog relation
        assert!(SystemCatalog::is_relation("pg_default_acl"));
        assert!(SystemCatalog::is_relation("PG_DEFAULT_ACL"));
        assert!(SystemCatalog::is_relation("pg_catalog.pg_default_acl"));
    }

    #[test]
    fn normalize_relation_folds_case_and_strips_schema() {
        // System / information-schema views match by bare lowercase table name.
        assert_eq!(normalize_relation("INFORMATION_SCHEMA.TABLES"), "tables");
        assert_eq!(normalize_relation("PG_CATALOG.PG_CLASS"), "pg_class");
        assert_eq!(normalize_relation("routines"), "routines");
        assert_eq!(
            normalize_relation("INFORMATION_SCHEMA.ROUTINES"),
            "routines"
        );
    }
}
