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
//! Schema directories: `objects/databases/DB-<db id>/schemas/S-<schema id>/`.
//!
//! A schema directory is the logical home of one schema object. It lives inside
//! exactly one database directory:
//!
//! ```text
//! objects/databases/DB-00000000000000000001/
//! └── schemas/
//!     └── S-00000000000000000001/
//!         ├── META.dat  schema-local identity and validation metadata
//!         └── tables/   tables that belong to this schema
//! ```
//!
//! The catalog owns the *name → SchemaId* mapping within a database; this
//! module owns *where* a schema's directory lives and the metadata record that
//! binds the directory to its logical identity and owning database.

use super::names::{parse_layout_id, render_prefixed_id};
use super::DatabaseLayout;
use crate::codec::{corruption, put_u32, put_u64};
use crate::durable;
use crate::layout::metadata;
use crate::layout::names::META_FILE_NAME;
use plomid_core::{DatabaseId, Result, SchemaId};
use std::path::{Path, PathBuf};

/// Prefix of a schema directory name.
pub const SCHEMA_DIR_PREFIX: &str = "S-";
/// Magic of the schema metadata record.
pub const SCHEMA_META_MAGIC: [u8; 4] = *b"PLSM";
/// Format version of the schema metadata record.
pub const SCHEMA_META_VERSION: u32 = 1;
/// Total length of the schema metadata record.
pub const SCHEMA_META_HEADER_LEN: usize = 40;
/// Offset of the metadata checksum.
const META_CHECKSUM_OFFSET: usize = 28;

/// Renders the directory name of a schema.
#[must_use]
pub fn schema_dir_name(schema_id: SchemaId) -> String {
    render_prefixed_id(SCHEMA_DIR_PREFIX, schema_id.get())
}

/// Parses a schema directory name. Only the deterministic form is accepted.
#[must_use]
pub fn schema_from_dir_name(name: &str) -> Option<SchemaId> {
    parse_layout_id(name, SCHEMA_DIR_PREFIX).map(SchemaId::new)
}

/// Directory holding the schemas of a database (`schemas/`).
pub const SCHEMAS_DIR_NAME: &str = "schemas";

/// Durable identity of one schema directory.
///
/// The record binds the directory to the logical schema object it materializes,
/// recording both the schema identity and the owning database identity so a
/// schema can never be silently re-homed to a different database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaMetadata {
    /// The logical identity of the schema.
    pub schema_id: SchemaId,
    /// The database that owns this schema.
    pub database_id: DatabaseId,
}

impl SchemaMetadata {
    /// Creates a record for a schema identity and its owning database.
    ///
    /// Returns an error for a zero identity, which is never valid.
    pub fn new(schema_id: SchemaId, database_id: DatabaseId) -> Result<Self> {
        if schema_id.is_zero() {
            return Err(corruption("schema metadata has a zero identity"));
        }
        if database_id.is_zero() {
            return Err(corruption("schema metadata has a zero database identity"));
        }
        Ok(Self {
            schema_id,
            database_id,
        })
    }

    /// Encodes the record deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(SCHEMA_META_HEADER_LEN);
        metadata::encode_prefix(
            &mut out,
            SCHEMA_META_MAGIC,
            SCHEMA_META_VERSION,
            SCHEMA_META_HEADER_LEN as u32,
        );
        put_u64(&mut out, self.schema_id.get());
        put_u64(&mut out, self.database_id.get());
        let checksum = crate::checksum::compute_checksum(&out[..META_CHECKSUM_OFFSET]);
        put_u32(&mut out, checksum);
        out.extend_from_slice(&[0_u8; 8]);
        Ok(out)
    }

    /// Decodes and validates a metadata image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (mut cursor, header_len) = metadata::decode_prefix(
            bytes,
            SCHEMA_META_MAGIC,
            SCHEMA_META_VERSION,
            "schema metadata",
        )?;
        metadata::ensure_header_len(bytes, header_len, "schema metadata")?;
        let schema_id = SchemaId::new(cursor.u64("schema metadata")?);
        let database_id = DatabaseId::new(cursor.u64("schema metadata")?);
        let stored = cursor.u32("schema metadata")?;
        crate::checksum::verify_checksum(&bytes[..META_CHECKSUM_OFFSET], stored)?;
        metadata::ensure_reserved_zero(&bytes[cursor.position()..header_len], "schema metadata")?;
        Self::new(schema_id, database_id)
    }

    /// Publishes the record atomically at `path`.
    pub fn publish(&self, path: &Path) -> Result<()> {
        let bytes = self.encode()?;
        // VERIFY: a record that cannot be decoded is never published.
        Self::decode(&bytes)?;
        metadata::publish(path, &bytes)
    }
}

impl DatabaseLayout {
    /// Directory of one schema within one database.
    #[must_use]
    pub fn schema_dir(&self, database_id: DatabaseId, schema_id: SchemaId) -> PathBuf {
        self.database_schemas_dir(database_id)
            .join(schema_dir_name(schema_id))
    }

    /// Schema-local metadata record.
    #[must_use]
    pub fn schema_meta_path(&self, database_id: DatabaseId, schema_id: SchemaId) -> PathBuf {
        self.schema_dir(database_id, schema_id).join(META_FILE_NAME)
    }

    /// Directory holding the tables of one schema.
    ///
    /// This is the namespace container for the schema: tables resolve to it by
    /// catalog name lookup, while table storage objects remain table-local
    /// under `objects/tables/`.
    #[must_use]
    pub fn schema_tables_dir(&self, database_id: DatabaseId, schema_id: SchemaId) -> PathBuf {
        self.schema_dir(database_id, schema_id)
            .join(super::tables::TABLES_DIR_NAME)
    }

    /// Creates the directory of a schema within its database, idempotently.
    ///
    /// The schemas directory of the owning database is created first, so a
    /// schema can never exist without its database. No metadata record is
    /// written here; the caller publishes the schema record through the
    /// catalog path.
    pub fn ensure_schema_dir(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
    ) -> Result<PathBuf> {
        if database_id.is_zero() {
            return Err(plomid_core::PlomidError::with_detail(
                plomid_core::ErrorKind::InvalidArgument,
                "schema identity is zero",
                String::new(),
            ));
        }
        if schema_id.is_zero() {
            return Err(plomid_core::PlomidError::with_detail(
                plomid_core::ErrorKind::InvalidArgument,
                "schema identity is zero",
                String::new(),
            ));
        }
        let dir = self.schema_dir(database_id, schema_id);
        self.ensure_database_dir(database_id)?;
        durable::ensure_dir(&dir)?;
        durable::ensure_dir(&self.schema_tables_dir(database_id, schema_id))?;
        Ok(dir)
    }

    /// Enumerates the schemas of one database in deterministic identity order.
    ///
    /// Only the schemas that actually exist have directories, so this reports
    /// real logical state and never invents a schema name.
    pub fn discover_schema_ids(&self, database_id: DatabaseId) -> Result<Vec<SchemaId>> {
        let dir = self.database_schemas_dir(database_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(plomid_core::PlomidError::from)? {
            let entry = entry.map_err(plomid_core::PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(schema_id) = schema_from_dir_name(&name) {
                ids.push(schema_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Reads and validates the metadata record of a schema directory.
    pub fn read_schema_meta(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
    ) -> Result<SchemaMetadata> {
        let bytes = metadata::read(
            &self.schema_meta_path(database_id, schema_id),
            "schema metadata",
        )?;
        SchemaMetadata::decode(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{schema_dir_name, schema_from_dir_name, SchemaMetadata, SCHEMA_META_HEADER_LEN};
    use plomid_core::{DatabaseId, SchemaId};

    #[test]
    fn directory_names_round_trip_through_identity() {
        assert_eq!(schema_dir_name(SchemaId::new(1)), "S-00000000000000000001");
        assert_eq!(
            schema_from_dir_name("S-00000000000000000042"),
            Some(SchemaId::new(42))
        );
        assert_eq!(schema_from_dir_name("S-42"), None);
        assert_eq!(schema_from_dir_name("DB-00000000000000000042"), None);
    }

    #[test]
    fn metadata_round_trips_and_detects_corruption() {
        let record = SchemaMetadata::new(SchemaId::new(3), DatabaseId::new(1)).expect("record");
        let bytes = record.encode().expect("encode");
        assert_eq!(bytes.len(), SCHEMA_META_HEADER_LEN);
        assert_eq!(SchemaMetadata::decode(&bytes).expect("decode"), record);
        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0xFF;
        assert!(SchemaMetadata::decode(&bad_magic).is_err());
        let mut bad_checksum = bytes.clone();
        bad_checksum[20] ^= 0x01;
        assert!(SchemaMetadata::decode(&bad_checksum).is_err());
        assert!(SchemaMetadata::decode(&bytes[..10]).is_err());
    }

    #[test]
    fn zero_identity_is_rejected() {
        assert!(SchemaMetadata::new(SchemaId::new(0), DatabaseId::new(1)).is_err());
        assert!(SchemaMetadata::new(SchemaId::new(1), DatabaseId::new(0)).is_err());
    }
}
