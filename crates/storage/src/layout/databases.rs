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
//! Database directories: `objects/databases/DB-<database id>/`.
//!
//! A database directory is the logical home of one database object:
//!
//! ```text
//! objects/databases/DB-00000000000000000001/
//! ├── META.dat      database-local identity and validation metadata
//! └── schemas/      schemas that belong to this database
//! ```
//!
//! The catalog owns the *name → DatabaseId* mapping; this module owns *where*
//! a database's directory lives and the metadata record that binds the
//! directory to its logical identity. Physical bytes are never stored here —
//! devices live under `devices/` and are referenced through the placement
//! layer.
//!
//! `objects/databases/` is created during root initialization so that the
//! logical tree always has its top-level container available; individual
//! `DB-` directories appear only when a database is actually created through
//! the DDL/catalog path.

use super::names::{parse_layout_id, render_prefixed_id};
use super::DatabaseLayout;
use crate::codec::{corruption, put_u32, put_u64};
use crate::durable;
use crate::layout::metadata;
use crate::layout::names::META_FILE_NAME;
use plomid_core::{DatabaseId, Result};
use std::path::{Path, PathBuf};

/// Prefix of a database directory name.
pub const DATABASE_DIR_PREFIX: &str = "DB-";
/// Magic of the database metadata record.
pub const DATABASE_META_MAGIC: [u8; 4] = *b"PLDB";
/// Format version of the database metadata record.
pub const DATABASE_META_VERSION: u32 = 1;
/// Total length of the database metadata record.
pub const DATABASE_META_HEADER_LEN: usize = 28;
/// Offset of the metadata checksum.
const META_CHECKSUM_OFFSET: usize = 20;
/// Renders the directory name of a database.
#[must_use]
pub fn database_dir_name(database_id: DatabaseId) -> String {
    render_prefixed_id(DATABASE_DIR_PREFIX, database_id.get())
}

/// Parses a database directory name. Only the deterministic form is accepted.
#[must_use]
pub fn database_from_dir_name(name: &str) -> Option<DatabaseId> {
    parse_layout_id(name, DATABASE_DIR_PREFIX).map(DatabaseId::new)
}

/// Durable identity of one database directory.
///
/// The record binds the directory to the logical database object it
/// materializes. It stores only the database identity and a format version;
/// the authoritative name → id mapping lives with the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseMetadata {
    /// The logical identity of the database.
    pub database_id: DatabaseId,
}

impl DatabaseMetadata {
    /// Creates a record for a database identity.
    ///
    /// Returns an error for a zero identity, which is never a valid database.
    pub fn new(database_id: DatabaseId) -> Result<Self> {
        if database_id.is_zero() {
            return Err(corruption("database metadata has a zero identity"));
        }
        Ok(Self { database_id })
    }

    /// Encodes the record deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(DATABASE_META_HEADER_LEN);
        metadata::encode_prefix(
            &mut out,
            DATABASE_META_MAGIC,
            DATABASE_META_VERSION,
            DATABASE_META_HEADER_LEN as u32,
        );
        put_u64(&mut out, self.database_id.get());
        let checksum = crate::checksum::compute_checksum(&out[..META_CHECKSUM_OFFSET]);
        put_u32(&mut out, checksum);
        out.extend_from_slice(&[0_u8; 4]);
        Ok(out)
    }

    /// Decodes and validates a metadata image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (mut cursor, header_len) = metadata::decode_prefix(
            bytes,
            DATABASE_META_MAGIC,
            DATABASE_META_VERSION,
            "database metadata",
        )?;
        metadata::ensure_header_len(bytes, header_len, "database metadata")?;
        let database_id = DatabaseId::new(cursor.u64("database metadata")?);
        let stored = cursor.u32("database metadata")?;
        crate::checksum::verify_checksum(&bytes[..META_CHECKSUM_OFFSET], stored)?;
        metadata::ensure_reserved_zero(&bytes[cursor.position()..header_len], "database metadata")?;
        Self::new(database_id)
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
    /// Directory holding every database (`objects/databases`).
    ///
    /// This container is created during root initialization so logical database
    /// objects always have a home. Individual `DB-*` directories appear only
    /// when a database is created through the catalog/DDL lifecycle.
    #[must_use]
    pub fn databases_dir(&self) -> PathBuf {
        self.objects_dir().join("databases")
    }

    /// Logical directory of one database.
    #[must_use]
    pub fn database_dir(&self, database_id: DatabaseId) -> PathBuf {
        self.databases_dir().join(database_dir_name(database_id))
    }

    /// Database-local metadata record.
    #[must_use]
    pub fn database_meta_path(&self, database_id: DatabaseId) -> PathBuf {
        self.database_dir(database_id).join(META_FILE_NAME)
    }

    /// Directory holding the schemas of one database.
    #[must_use]
    pub fn database_schemas_dir(&self, database_id: DatabaseId) -> PathBuf {
        self.database_dir(database_id)
            .join(super::schemas::SCHEMAS_DIR_NAME)
    }

    /// Creates the directory of a database and its schemas subdirectory,
    /// idempotently.
    ///
    /// Only directories are materialized: metadata publication remains the
    /// caller's responsibility through the catalog/DDL lifecycle, preserving
    /// the stage/write/flush/verify/sync/rename ordering used by every other
    /// layout metadata file.
    pub fn ensure_database_dir(&self, database_id: DatabaseId) -> Result<PathBuf> {
        if database_id.is_zero() {
            return Err(plomid_core::PlomidError::with_detail(
                plomid_core::ErrorKind::InvalidArgument,
                "database identity is zero",
                String::new(),
            ));
        }
        let dir = self.database_dir(database_id);
        durable::ensure_dir(&dir)?;
        durable::ensure_dir(&self.database_schemas_dir(database_id))?;
        Ok(dir)
    }

    /// Enumerates database directories in deterministic identity order.
    pub fn discover_database_ids(&self) -> Result<Vec<DatabaseId>> {
        let dir = self.databases_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(plomid_core::PlomidError::from)? {
            let entry = entry.map_err(plomid_core::PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(db_id) = database_from_dir_name(&name) {
                ids.push(db_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Reads and validates the metadata record of a database directory.
    pub fn read_database_meta(&self, database_id: DatabaseId) -> Result<DatabaseMetadata> {
        let bytes = metadata::read(&self.database_meta_path(database_id), "database metadata")?;
        DatabaseMetadata::decode(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        database_dir_name, database_from_dir_name, DatabaseMetadata, DATABASE_META_HEADER_LEN,
    };
    use plomid_core::DatabaseId;

    #[test]
    fn directory_names_round_trip_through_identity() {
        assert_eq!(
            database_dir_name(DatabaseId::new(1)),
            "DB-00000000000000000001"
        );
        assert_eq!(
            database_from_dir_name("DB-00000000000000000042"),
            Some(DatabaseId::new(42))
        );
        assert_eq!(database_from_dir_name("DB-42"), None);
        assert_eq!(database_from_dir_name("T-00000000000000000042"), None);
    }

    #[test]
    fn metadata_round_trips_and_detects_corruption() {
        let record = DatabaseMetadata::new(DatabaseId::new(3)).expect("record");
        let bytes = record.encode().expect("encode");
        assert_eq!(bytes.len(), DATABASE_META_HEADER_LEN);
        assert_eq!(DatabaseMetadata::decode(&bytes).expect("decode"), record);
        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0xFF;
        assert!(DatabaseMetadata::decode(&bad_magic).is_err());
        let mut bad_version = bytes.clone();
        bad_version[4] = 0xFF;
        assert!(DatabaseMetadata::decode(&bad_version).is_err());
        let mut bad_checksum = bytes.clone();
        bad_checksum[12] ^= 0x01;
        assert!(DatabaseMetadata::decode(&bad_checksum).is_err());
        assert!(DatabaseMetadata::decode(&bytes[..10]).is_err());
    }

    #[test]
    fn zero_identity_is_rejected() {
        assert!(DatabaseMetadata::new(DatabaseId::new(0)).is_err());
    }
}
