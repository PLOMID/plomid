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
//! Table directories: `objects/tables/T-<table id>/`.
//!
//! A table directory is the logical home of one table object:
//!
//! ```text
//! objects/tables/T-0000000000000001/
//! ├── META.dat      table-local identity and validation metadata
//! ├── hot/          not yet materialized table state
//! ├── generations/  immutable generations of the table
//! └── indexes/      indexes owned by the table
//! ```
//!
//! A table directory never holds physical pages. The generations of a table
//! reference physical segments, and the storage layer places those segments on
//! devices, so one logical table can span several devices while its directory
//! stays a single logical concept.
//!
//! Database and schema objects live in a parallel hierarchy at
//! `objects/databases/DB-<db id>/schemas/S-<schema id>/`; the SQL catalog
//! resolves database/schema/table names and identities, while this table path
//! preserves generation placement, recovery, validation, and multi-device
//! behavior.
//!
//! # Authority
//!
//! `META.dat` binds a directory name to the logical table object it
//! materializes. It deliberately does not repeat catalog authority (schema,
//! current generation, physical references); those stay with the catalog.

use super::metadata;
use super::names::{parse_layout_id, render_prefixed_id};
use super::{indexes, DatabaseLayout};
use crate::checksum::{compute_checksum, verify_checksum};
use crate::codec::{corruption, put_u32, put_u64};
use crate::durable;
use plomid_core::{DatabaseId, ObjectId, PlomidError, Result, SchemaId, TableId};
use std::path::{Path, PathBuf};

/// Directory holding every table of the database (`objects/tables`).
pub const TABLES_DIR_NAME: &str = "tables";
/// Prefix of a table directory name.
pub const TABLE_DIR_PREFIX: &str = "T-";
/// Directory of a table's not yet materialized state.
pub const TABLE_HOT_DIR_NAME: &str = "hot";
/// Magic of the table metadata record.
pub const TABLE_META_MAGIC: [u8; 4] = *b"PLTB";
/// Format version of the table metadata record.
pub const TABLE_META_VERSION: u32 = 1;
/// Total length of the table metadata record.
pub const TABLE_META_HEADER_LEN: usize = 40;
/// Offset of the metadata checksum.
const META_CHECKSUM_OFFSET: usize = 28;

/// Renders the directory name of a table.
#[must_use]
pub fn table_dir_name(table_id: TableId) -> String {
    render_prefixed_id(TABLE_DIR_PREFIX, table_id.get())
}

/// Parses a table directory name. Only the deterministic form is accepted.
#[must_use]
pub fn table_from_dir_name(name: &str) -> Option<TableId> {
    parse_layout_id(name, TABLE_DIR_PREFIX).map(TableId::new)
}

impl DatabaseLayout {
    /// Directory holding every table (`objects/tables`).
    ///
    /// Tables remain direct children of `objects/` so every existing
    /// generation, placement, validation, recovery, and test path keeps its
    /// current behavior. Database and schema objects have their own parallel
    /// hierarchy under `objects/databases/`; catalog metadata resolves their
    /// identities, while this flat table directory preserves physical
    /// generation placement.
    #[must_use]
    pub fn tables_dir(&self) -> PathBuf {
        self.objects_dir().join(TABLES_DIR_NAME)
    }

    /// Logical directory of one table.
    #[must_use]
    pub fn table_dir(&self, table_id: TableId) -> PathBuf {
        self.tables_dir().join(table_dir_name(table_id))
    }

    /// Table-local metadata record of one table.
    #[must_use]
    pub fn table_meta_path(&self, table_id: TableId) -> PathBuf {
        self.table_dir(table_id).join(super::names::META_FILE_NAME)
    }

    /// Directory of a table's not yet materialized state.
    #[must_use]
    pub fn table_hot_dir(&self, table_id: TableId) -> PathBuf {
        self.table_dir(table_id).join(TABLE_HOT_DIR_NAME)
    }

    /// Directory holding the immutable generations of a table.
    #[must_use]
    pub fn table_generations_dir(&self, table_id: TableId) -> PathBuf {
        self.table_dir(table_id)
            .join(super::generations::GENERATIONS_DIR_NAME)
    }

    /// Directory holding the indexes owned by a table.
    #[must_use]
    pub fn table_indexes_dir(&self, table_id: TableId) -> PathBuf {
        self.table_dir(table_id).join(indexes::INDEXES_DIR_NAME)
    }

    /// Directory of a table inside its schema's logical object tree:
    /// `objects/databases/DB-<db>/schemas/S-<schema>/tables/T-<table>/`.
    ///
    /// This is the SQL-facing logical location of a table object: name
    /// resolution reaches it through the catalog, and it is created only when
    /// the table itself is created. The table's storage objects (generations,
    /// indexes, hot state) keep their table-local paths under
    /// [`Self::table_dir`], so generation management, placement, recovery, and
    /// multi-device behavior are unchanged.
    #[must_use]
    pub fn table_dir_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> PathBuf {
        self.schema_tables_dir(database_id, schema_id)
            .join(table_dir_name(table_id))
    }

    /// Table identity record inside its schema's logical object tree.
    #[must_use]
    pub fn table_meta_path_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> PathBuf {
        self.table_dir_in_schema(database_id, schema_id, table_id)
            .join(super::names::META_FILE_NAME)
    }

    /// Creates a table's logical object directory inside its schema,
    /// idempotently.
    ///
    /// The owning database and schema directories are materialized first, so a
    /// table can never exist without the schema that owns it. The record binds
    /// the directory to the table identity and is created only when absent: an
    /// existing valid record is never rewritten, and a record describing a
    /// different table is reported as corruption rather than replaced.
    pub fn ensure_table_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> Result<PathBuf> {
        if table_id.is_zero() {
            return Err(corruption("table directory identity is zero"));
        }
        self.ensure_schema_dir(database_id, schema_id)?;
        let dir = self.table_dir_in_schema(database_id, schema_id, table_id);
        durable::ensure_dir(&dir)?;
        // The logical table owns its not-yet-materialized state, its immutable
        // generations, and its indexes, so all three directories appear with
        // the table and nowhere else.
        durable::ensure_dir(&self.table_hot_dir_in_schema(database_id, schema_id, table_id))?;
        durable::ensure_dir(&self.table_generations_dir_in_schema(
            database_id,
            schema_id,
            table_id,
        ))?;
        durable::ensure_dir(&self.table_indexes_dir_in_schema(database_id, schema_id, table_id))?;
        let meta_path = self.table_meta_path_in_schema(database_id, schema_id, table_id);
        let record = TableMetadata::new(table_id, ObjectId::new(table_id.get()))?;
        match metadata::read_optional(&meta_path)? {
            Some(bytes) => {
                let existing = TableMetadata::decode(&bytes)?;
                if existing != record {
                    return Err(corruption(
                        "table metadata does not describe this table directory",
                    ));
                }
            }
            None => record.publish(&meta_path)?,
        }
        Ok(dir)
    }

    /// Reads and validates the identity record of a table inside its schema.
    pub fn read_table_meta_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> Result<TableMetadata> {
        let bytes = metadata::read(
            &self.table_meta_path_in_schema(database_id, schema_id, table_id),
            "table metadata",
        )?;
        TableMetadata::decode(&bytes)
    }

    /// Enumerates the tables of one schema in deterministic identity order.
    pub fn discover_table_ids_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
    ) -> Result<Vec<TableId>> {
        let dir = self.schema_tables_dir(database_id, schema_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(table_id) = table_from_dir_name(&name) {
                ids.push(table_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Directory of a table's not yet materialized state, within its schema.
    #[must_use]
    pub fn table_hot_dir_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> PathBuf {
        self.table_dir_in_schema(database_id, schema_id, table_id)
            .join(TABLE_HOT_DIR_NAME)
    }

    /// Directory holding the immutable generations of a table, within its schema.
    #[must_use]
    pub fn table_generations_dir_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> PathBuf {
        self.table_dir_in_schema(database_id, schema_id, table_id)
            .join(super::generations::GENERATIONS_DIR_NAME)
    }

    /// Directory holding the indexes owned by a table, within its schema.
    #[must_use]
    pub fn table_indexes_dir_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> PathBuf {
        self.table_dir_in_schema(database_id, schema_id, table_id)
            .join(indexes::INDEXES_DIR_NAME)
    }

    /// Creates the logical directory of a table, idempotently.
    ///
    /// The table's metadata record is created only when it is absent: an
    /// existing valid record is never rewritten, and a record that describes a
    /// different table is reported as corruption rather than replaced.
    pub fn ensure_table(&self, table_id: TableId) -> Result<PathBuf> {
        if table_id.is_zero() {
            return Err(corruption("table directory identity is zero"));
        }
        let dir = self.table_dir(table_id);
        durable::ensure_dir(&dir)?;
        durable::ensure_dir(&self.table_hot_dir(table_id))?;
        durable::ensure_dir(&self.table_generations_dir(table_id))?;
        durable::ensure_dir(&self.table_indexes_dir(table_id))?;
        let meta_path = self.table_meta_path(table_id);
        let record = TableMetadata::new(table_id, ObjectId::new(table_id.get()))?;
        match metadata::read_optional(&meta_path)? {
            Some(bytes) => {
                let existing = TableMetadata::decode(&bytes)?;
                if existing != record {
                    return Err(corruption(
                        "table metadata does not describe this table directory",
                    ));
                }
            }
            None => record.publish(&meta_path)?,
        }
        Ok(dir)
    }

    /// Reads and validates the metadata record of a table directory.
    pub fn read_table_meta(&self, table_id: TableId) -> Result<TableMetadata> {
        let bytes = metadata::read(&self.table_meta_path(table_id), "table metadata")?;
        TableMetadata::decode(&bytes)
    }

    /// Enumerates table directories in deterministic identity order.
    ///
    /// Discovery scans only `objects/tables/`, the current durable location of
    /// table objects. Database and schema objects are discovered separately
    /// under `objects/databases/`.
    pub fn discover_table_ids(&self) -> Result<Vec<TableId>> {
        let dir = self.tables_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(table_id) = table_from_dir_name(&name) {
                ids.push(table_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }
}

/// Durable identity of one table directory.
///
/// The record binds the directory to the logical table object it materializes.
/// The layout renders a table object as `T-<object id>`, so both identities are
/// the same counter viewed from the directory and from the catalog; a record
/// whose values disagree has been moved, renamed, or tampered with, and is
/// rejected instead of adopted.
///
/// # Format
///
/// ```text
/// magic[4] = PLTB | format_version[u32] = 1 | header_len[u32] = 40
/// table_id[u64]   | object_id[u64]          | checksum[u32] | reserved[8] = 0
/// ```
///
/// The checksum is CRC32C over `record[0..28]`; the checksum field and the
/// reserved bytes are not covered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableMetadata {
    /// Identity of the table directory.
    pub table_id: TableId,
    /// Logical table object this directory materializes.
    pub object_id: ObjectId,
}

impl TableMetadata {
    /// Creates table metadata, validating the layout identity convention.
    pub fn new(table_id: TableId, object_id: ObjectId) -> Result<Self> {
        if table_id.is_zero() {
            return Err(corruption("table metadata has a zero table ID"));
        }
        if object_id.get() != table_id.get() {
            return Err(corruption(
                "table metadata object does not match its directory identity",
            ));
        }
        Ok(Self {
            table_id,
            object_id,
        })
    }

    /// Encodes the record deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(TABLE_META_HEADER_LEN);
        metadata::encode_prefix(
            &mut out,
            TABLE_META_MAGIC,
            TABLE_META_VERSION,
            TABLE_META_HEADER_LEN as u32,
        );
        put_u64(&mut out, self.table_id.get());
        put_u64(&mut out, self.object_id.get());
        let checksum = compute_checksum(&out[..META_CHECKSUM_OFFSET]);
        put_u32(&mut out, checksum);
        out.extend_from_slice(&[0_u8; 8]);
        Ok(out)
    }

    /// Decodes and validates a metadata image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (mut cursor, header_len) = metadata::decode_prefix(
            bytes,
            TABLE_META_MAGIC,
            TABLE_META_VERSION,
            "table metadata",
        )?;
        metadata::ensure_header_len(bytes, header_len, "table metadata")?;
        let table_id = TableId::new(cursor.u64("table metadata")?);
        let object_id = ObjectId::new(cursor.u64("table metadata")?);
        let stored = cursor.u32("table metadata")?;
        verify_checksum(&bytes[..META_CHECKSUM_OFFSET], stored)?;
        metadata::ensure_reserved_zero(&bytes[cursor.position()..header_len], "table metadata")?;
        Self::new(table_id, object_id)
    }

    /// Publishes the record atomically at `path`.
    pub fn publish(&self, path: &Path) -> Result<()> {
        let bytes = self.encode()?;
        // VERIFY: a record that cannot be decoded is never published.
        Self::decode(&bytes)?;
        metadata::publish(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{table_dir_name, table_from_dir_name, TableMetadata, TABLE_META_HEADER_LEN};
    use plomid_core::{ObjectId, TableId};

    #[test]
    fn directory_names_round_trip_through_identity() {
        assert_eq!(table_dir_name(TableId::new(1)), "T-00000000000000000001");
        assert_eq!(
            table_from_dir_name("T-00000000000000000042"),
            Some(TableId::new(42))
        );
        assert_eq!(table_from_dir_name("T-42"), None);
        assert_eq!(table_from_dir_name("I-00000000000000000042"), None);
    }

    #[test]
    fn metadata_round_trips_and_detects_corruption() {
        let record = TableMetadata::new(TableId::new(3), ObjectId::new(3)).expect("record");
        let bytes = record.encode().expect("encode");
        assert_eq!(bytes.len(), TABLE_META_HEADER_LEN);
        assert_eq!(TableMetadata::decode(&bytes).expect("decode"), record);
        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0xFF;
        assert!(TableMetadata::decode(&bad_magic).is_err());
        let mut bad_version = bytes.clone();
        bad_version[4] = 0xFF;
        assert!(TableMetadata::decode(&bad_version).is_err());
        let mut bad_checksum = bytes.clone();
        bad_checksum[12] ^= 0x01;
        assert!(TableMetadata::decode(&bad_checksum).is_err());
        assert!(TableMetadata::decode(&bytes[..10]).is_err());
        assert!(TableMetadata::decode(&[bytes.clone(), vec![0]].concat()).is_err());
    }

    #[test]
    fn mismatched_object_identity_is_rejected() {
        assert!(TableMetadata::new(TableId::new(1), ObjectId::new(2)).is_err());
        assert!(TableMetadata::new(TableId::new(0), ObjectId::new(0)).is_err());
    }
}
