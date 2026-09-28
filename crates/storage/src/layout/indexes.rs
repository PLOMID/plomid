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
//! Index directories: `objects/tables/T-<table id>/indexes/I-<index id>/`.
//!
//! An index is owned by the table it belongs to, so its directory lives inside
//! that table's directory:
//!
//! ```text
//! objects/tables/T-0000000000000001/indexes/I-0000000000000001/
//! ├── META.dat                    index identity and its owning table
//! └── GEN-0000000000000001.dat    immutable generation metadata of the index
//! ```
//!
//! Index generations are single metadata files inside the index directory,
//! while table generations are directories holding their own `META.dat`; both
//! use the same explicit record format. Physical index bytes are never stored
//! here: the persistent index implementation stores through the physical
//! storage layer exactly like table data, and only an index's identity and
//! generation metadata live in the logical tree.

use super::metadata;
use super::names::{parse_layout_id, render_prefixed_id};
use super::DatabaseLayout;
use crate::checksum::{compute_checksum, verify_checksum};
use crate::codec::{corruption, put_u32, put_u64};
use crate::durable;
use plomid_core::{DatabaseId, GenerationId, IndexId, PlomidError, Result, SchemaId, TableId};
use std::path::{Path, PathBuf};

/// Directory holding the indexes of one table.
pub const INDEXES_DIR_NAME: &str = "indexes";
/// Prefix of an index directory name.
pub const INDEX_DIR_PREFIX: &str = "I-";
/// Prefix of an index generation metadata file name.
pub const INDEX_GENERATION_FILE_PREFIX: &str = "GEN-";
/// Suffix of an index generation metadata file name.
pub const INDEX_GENERATION_FILE_SUFFIX: &str = ".dat";
/// Magic of the index metadata record.
pub const INDEX_META_MAGIC: [u8; 4] = *b"PLIX";
/// Format version of the index metadata record.
pub const INDEX_META_VERSION: u32 = 1;
/// Total length of the index metadata record.
pub const INDEX_META_HEADER_LEN: usize = 40;
/// Offset of the metadata checksum.
const META_CHECKSUM_OFFSET: usize = 28;
/// Magic of the index generation metadata record.
pub const INDEX_GENERATION_META_MAGIC: [u8; 4] = *b"PLIG";
/// Format version of the index generation metadata record.
pub const INDEX_GENERATION_META_VERSION: u32 = 1;
/// Total length of the index generation metadata record.
pub const INDEX_GENERATION_META_HEADER_LEN: usize = 72;
/// Offset of the index generation metadata checksum.
const INDEX_GENERATION_META_CHECKSUM_OFFSET: usize = 62;
/// File-name prefix of the persistent index payload of one generation.
pub const INDEX_PAYLOAD_FILE_PREFIX: &str = "IDX-";

/// Renders the payload file name of one index generation.
///
/// An index generation has two durable artifacts inside its index directory:
/// the generation's metadata record (`GEN-<id>.dat`) and its persistent B+Tree
/// payload (`IDX-<id>.dat`). Both are named by generation identity, so two
/// generations of the same index can never share durable state.
#[must_use]
pub fn index_payload_file_name(generation: GenerationId) -> String {
    format!(
        "{INDEX_PAYLOAD_FILE_PREFIX}{}{INDEX_GENERATION_FILE_SUFFIX}",
        super::names::render_layout_id(generation.get())
    )
}

/// Parses an index payload file name.
#[must_use]
pub fn index_payload_generation_from_file_name(name: &str) -> Option<GenerationId> {
    let number = name
        .strip_prefix(INDEX_PAYLOAD_FILE_PREFIX)?
        .strip_suffix(INDEX_GENERATION_FILE_SUFFIX)?;
    if number.len() != super::names::LAYOUT_ID_DIGITS
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let generation = number.parse::<u64>().ok()?;
    if generation == 0 {
        return None;
    }
    Some(GenerationId::new(generation))
}

/// Renders the directory name of an index.
#[must_use]
pub fn index_dir_name(index_id: IndexId) -> String {
    render_prefixed_id(INDEX_DIR_PREFIX, index_id.get())
}

/// Parses an index directory name.
#[must_use]
pub fn index_from_dir_name(name: &str) -> Option<IndexId> {
    parse_layout_id(name, INDEX_DIR_PREFIX).map(IndexId::new)
}

/// Renders the file name of one index generation.
#[must_use]
pub fn index_generation_file_name(generation: GenerationId) -> String {
    format!(
        "{INDEX_GENERATION_FILE_PREFIX}{}{INDEX_GENERATION_FILE_SUFFIX}",
        super::names::render_layout_id(generation.get())
    )
}

/// Parses an index generation metadata file name.
#[must_use]
pub fn index_generation_from_file_name(name: &str) -> Option<GenerationId> {
    let number = name
        .strip_prefix(INDEX_GENERATION_FILE_PREFIX)?
        .strip_suffix(INDEX_GENERATION_FILE_SUFFIX)?;
    if number.len() != super::names::LAYOUT_ID_DIGITS
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let generation = number.parse::<u64>().ok()?;
    if generation == 0 {
        return None;
    }
    Some(GenerationId::new(generation))
}

/// Why an index generation was created.
///
/// Provenance is diagnostic metadata only: it never selects a different
/// physical format, and both triggers execute the same lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexGenerationTrigger {
    /// Created as part of normal deterministic maintenance.
    Automatic,
    /// Created by an explicit rebuild request.
    Manual,
}

impl IndexGenerationTrigger {
    /// Stable persisted code.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Automatic => 0,
            Self::Manual => 1,
        }
    }

    /// Decodes the stable persisted code.
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Automatic),
            1 => Ok(Self::Manual),
            _ => Err(corruption("index generation trigger is invalid")),
        }
    }
}

/// Lifecycle state of an index generation.
///
/// Only [`Self::Current`] is usable by readers. Every other state is
/// non-authoritative: a generation becomes visible only after it has been
/// durably and atomically published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexGenerationState {
    /// Built but not yet published.
    Staged,
    /// Published and authoritative for its index.
    Current,
    /// Superseded but still retained.
    Retained,
    /// No longer referenced; reclamation is permitted.
    Retired,
}

impl IndexGenerationState {
    /// Stable persisted code.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Staged => 0,
            Self::Current => 1,
            Self::Retained => 2,
            Self::Retired => 3,
        }
    }

    /// Decodes the stable persisted code.
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Staged),
            1 => Ok(Self::Current),
            2 => Ok(Self::Retained),
            3 => Ok(Self::Retired),
            _ => Err(corruption("index generation state is invalid")),
        }
    }

    /// True when this state is authoritative for its index.
    #[must_use]
    pub const fn is_current(self) -> bool {
        matches!(self, Self::Current)
    }
}

/// Durable metadata of one immutable index generation.
///
/// The record binds an index generation to the index it belongs to, to the
/// owning `Database → Schema → Table` location, to the *data* generation it was
/// built from, and to its creation provenance and lifecycle state.
///
/// # Format
///
/// ```text
/// magic[4] = PLIG | format_version[u32] = 1 | header_len[u32] = 72
/// index_id[u64]                  | generation_id[u64]
/// database_id[u64]               | schema_id[u64]
/// table_id[u64]                  | source_data_generation_id[u64]
/// trigger[u8]                    | state[u8]
/// checksum[u32]                  | reserved[6] = 0
/// ```
///
/// The checksum is CRC32C over `record[0..62]`; the checksum field and the
/// reserved bytes are not covered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexGenerationMetadata {
    /// Identity of the index this generation belongs to.
    pub index_id: IndexId,
    /// Immutable generation identity of this index build.
    pub generation_id: GenerationId,
    /// Owning database.
    pub database_id: DatabaseId,
    /// Owning schema.
    pub schema_id: SchemaId,
    /// Owning table.
    pub table_id: TableId,
    /// Data generation this index was built from.
    pub source_data_generation_id: GenerationId,
    /// Why this generation was created.
    pub trigger: IndexGenerationTrigger,
    /// Lifecycle state of this generation.
    pub state: IndexGenerationState,
}

impl IndexGenerationMetadata {
    /// Creates index generation metadata, validating identity and provenance.
    // Seven identity/provenance inputs travel together by construction;
    // bundling them would churn every publisher for no behavioral gain.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        index_id: IndexId,
        generation_id: GenerationId,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        source_data_generation_id: GenerationId,
        trigger: IndexGenerationTrigger,
        state: IndexGenerationState,
    ) -> Result<Self> {
        let record = Self {
            index_id,
            generation_id,
            database_id,
            schema_id,
            table_id,
            source_data_generation_id,
            trigger,
            state,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        if self.index_id.is_zero() {
            return Err(corruption("index generation metadata has a zero index ID"));
        }
        if self.generation_id.is_zero() {
            return Err(corruption(
                "index generation metadata has a zero generation ID",
            ));
        }
        if self.database_id.is_zero() {
            return Err(corruption(
                "index generation metadata has a zero database ID",
            ));
        }
        if self.schema_id.is_zero() {
            return Err(corruption("index generation metadata has a zero schema ID"));
        }
        if self.table_id.is_zero() {
            return Err(corruption("index generation metadata has a zero table ID"));
        }
        if self.source_data_generation_id.is_zero() {
            return Err(corruption(
                "index generation metadata has a zero source data generation",
            ));
        }
        Ok(())
    }

    /// True when this generation was built from exactly `source`.
    #[must_use]
    pub fn is_compatible(self, source: GenerationId) -> bool {
        self.source_data_generation_id == source
    }

    /// Encodes the record deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::with_capacity(INDEX_GENERATION_META_HEADER_LEN);
        metadata::encode_prefix(
            &mut out,
            INDEX_GENERATION_META_MAGIC,
            INDEX_GENERATION_META_VERSION,
            INDEX_GENERATION_META_HEADER_LEN as u32,
        );
        put_u64(&mut out, self.index_id.get());
        put_u64(&mut out, self.generation_id.get());
        put_u64(&mut out, self.database_id.get());
        put_u64(&mut out, self.schema_id.get());
        put_u64(&mut out, self.table_id.get());
        put_u64(&mut out, self.source_data_generation_id.get());
        out.push(self.trigger.as_u8());
        out.push(self.state.as_u8());
        debug_assert_eq!(out.len(), INDEX_GENERATION_META_CHECKSUM_OFFSET);
        let checksum = compute_checksum(&out[..INDEX_GENERATION_META_CHECKSUM_OFFSET]);
        put_u32(&mut out, checksum);
        out.extend_from_slice(&[0_u8; 6]);
        debug_assert_eq!(out.len(), INDEX_GENERATION_META_HEADER_LEN);
        Ok(out)
    }

    /// Decodes and validates a metadata image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (mut cursor, header_len) = metadata::decode_prefix(
            bytes,
            INDEX_GENERATION_META_MAGIC,
            INDEX_GENERATION_META_VERSION,
            "index generation metadata",
        )?;
        metadata::ensure_header_len(bytes, header_len, "index generation metadata")?;
        let index_id = IndexId::new(cursor.u64("index generation metadata")?);
        let generation_id = GenerationId::new(cursor.u64("index generation metadata")?);
        let database_id = DatabaseId::new(cursor.u64("index generation metadata")?);
        let schema_id = SchemaId::new(cursor.u64("index generation metadata")?);
        let table_id = TableId::new(cursor.u64("index generation metadata")?);
        let source_data_generation_id = GenerationId::new(cursor.u64("index generation metadata")?);
        let trigger = IndexGenerationTrigger::from_u8(cursor.u8("index generation metadata")?)?;
        let state = IndexGenerationState::from_u8(cursor.u8("index generation metadata")?)?;
        let stored = cursor.u32("index generation metadata")?;
        verify_checksum(&bytes[..INDEX_GENERATION_META_CHECKSUM_OFFSET], stored)?;
        metadata::ensure_reserved_zero(
            &bytes[cursor.position()..header_len],
            "index generation metadata",
        )?;
        Self::new(
            index_id,
            generation_id,
            database_id,
            schema_id,
            table_id,
            source_data_generation_id,
            trigger,
            state,
        )
    }

    /// Returns a copy of the record in a different lifecycle state.
    pub fn with_state(&self, state: IndexGenerationState) -> Result<Self> {
        Self::new(
            self.index_id,
            self.generation_id,
            self.database_id,
            self.schema_id,
            self.table_id,
            self.source_data_generation_id,
            self.trigger,
            state,
        )
    }

    /// Publishes the record atomically at `path`.
    pub fn publish(&self, path: &Path) -> Result<()> {
        let bytes = self.encode()?;
        Self::decode(&bytes)?;
        metadata::publish(path, &bytes)
    }
}

impl DatabaseLayout {
    /// Directory holding the indexes of one table.
    #[must_use]
    pub fn indexes_dir(&self, table_id: TableId) -> PathBuf {
        self.table_indexes_dir(table_id)
    }

    /// Logical directory of one index.
    #[must_use]
    pub fn index_dir(&self, table_id: TableId, index_id: IndexId) -> PathBuf {
        self.table_indexes_dir(table_id)
            .join(index_dir_name(index_id))
    }

    /// Metadata record of one index.
    #[must_use]
    pub fn index_meta_path(&self, table_id: TableId, index_id: IndexId) -> PathBuf {
        self.index_dir(table_id, index_id)
            .join(super::names::META_FILE_NAME)
    }

    /// Logical directory of one index within its table's schema.
    #[must_use]
    pub fn index_dir_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
    ) -> PathBuf {
        self.table_indexes_dir_in_schema(database_id, schema_id, table_id)
            .join(index_dir_name(index_id))
    }

    /// Metadata record of one index within its table's schema.
    #[must_use]
    pub fn index_meta_path_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
    ) -> PathBuf {
        self.index_dir_in_schema(database_id, schema_id, table_id, index_id)
            .join(super::names::META_FILE_NAME)
    }

    /// Metadata file of one immutable index generation within its schema.
    #[must_use]
    pub fn index_generation_path_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
        generation: GenerationId,
    ) -> PathBuf {
        self.index_dir_in_schema(database_id, schema_id, table_id, index_id)
            .join(index_generation_file_name(generation))
    }

    /// Staging path used while publishing index generation metadata, within schema.
    #[must_use]
    pub fn index_generation_staged_path_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
        generation: GenerationId,
    ) -> PathBuf {
        metadata::staged_path(&self.index_generation_path_in_schema(
            database_id,
            schema_id,
            table_id,
            index_id,
            generation,
        ))
    }

    /// Persistent B+Tree payload file of one index generation, within schema.
    ///
    /// The payload is the durable ordered structure itself; the sibling
    /// `GEN-<id>.dat` record describes it. Both are named by generation
    /// identity, so a rebuild can never overwrite a retained generation.
    #[must_use]
    pub fn index_payload_path_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
        generation: GenerationId,
    ) -> PathBuf {
        self.index_dir_in_schema(database_id, schema_id, table_id, index_id)
            .join(index_payload_file_name(generation))
    }

    /// Reads and validates the generation metadata of one index generation.
    pub fn read_index_generation_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
        generation: GenerationId,
    ) -> Result<IndexGenerationMetadata> {
        let bytes = metadata::read(
            &self.index_generation_path_in_schema(
                database_id,
                schema_id,
                table_id,
                index_id,
                generation,
            ),
            "index generation metadata",
        )?;
        IndexGenerationMetadata::decode(&bytes)
    }

    /// Enumerates the generation metadata of one index, ascending by identity.
    ///
    /// Only published generation records are reported; staging artifacts and
    /// the index's own `META.dat` are ignored because their names do not parse
    /// as generation identities.
    pub fn discover_index_generations_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
    ) -> Result<Vec<IndexGenerationMetadata>> {
        let dir = self.index_dir_in_schema(database_id, schema_id, table_id, index_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.path().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if index_generation_from_file_name(&name).is_none() {
                continue;
            }
            let bytes = std::fs::read(entry.path()).map_err(PlomidError::from)?;
            records.push(IndexGenerationMetadata::decode(&bytes)?);
        }
        records.sort_unstable_by_key(|record| record.generation_id);
        Ok(records)
    }

    /// Creates the logical directory of an index within its schema, idempotently.
    pub fn ensure_index_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
    ) -> Result<PathBuf> {
        if index_id.is_zero() {
            return Err(corruption("index directory identity is zero"));
        }
        self.ensure_table_in_schema(database_id, schema_id, table_id)?;
        let dir = self.index_dir_in_schema(database_id, schema_id, table_id, index_id);
        durable::ensure_dir(&dir)?;
        let meta_path = self.index_meta_path_in_schema(database_id, schema_id, table_id, index_id);
        let record = IndexMetadata::new(index_id, table_id)?;
        match metadata::read_optional(&meta_path)? {
            Some(bytes) => {
                let existing = IndexMetadata::decode(&bytes)?;
                if existing != record {
                    return Err(corruption(
                        "index metadata does not describe this index directory",
                    ));
                }
            }
            None => record.publish(&meta_path)?,
        }
        Ok(dir)
    }

    /// Reads and validates the metadata record of an index directory within its schema.
    pub fn read_index_meta_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
        index_id: IndexId,
    ) -> Result<IndexMetadata> {
        let bytes = metadata::read(
            &self.index_meta_path_in_schema(database_id, schema_id, table_id, index_id),
            "index metadata",
        )?;
        IndexMetadata::decode(&bytes)
    }

    /// Enumerates the indexes of one table within its schema in deterministic identity order.
    pub fn discover_index_ids_in_schema(
        &self,
        database_id: DatabaseId,
        schema_id: SchemaId,
        table_id: TableId,
    ) -> Result<Vec<IndexId>> {
        let dir = self.table_indexes_dir_in_schema(database_id, schema_id, table_id);
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
            if let Some(index_id) = index_from_dir_name(&name) {
                ids.push(index_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Metadata file of one immutable index generation.
    #[must_use]
    pub fn index_generation_path(
        &self,
        table_id: TableId,
        index_id: IndexId,
        generation: GenerationId,
    ) -> PathBuf {
        self.index_dir(table_id, index_id)
            .join(index_generation_file_name(generation))
    }

    /// Staging path used while publishing index generation metadata.
    #[must_use]
    pub fn index_generation_staged_path(
        &self,
        table_id: TableId,
        index_id: IndexId,
        generation: GenerationId,
    ) -> PathBuf {
        metadata::staged_path(&self.index_generation_path(table_id, index_id, generation))
    }

    /// Creates the logical directory of an index, idempotently.
    pub fn ensure_index(&self, table_id: TableId, index_id: IndexId) -> Result<PathBuf> {
        if index_id.is_zero() {
            return Err(corruption("index directory identity is zero"));
        }
        self.ensure_table(table_id)?;
        let dir = self.index_dir(table_id, index_id);
        durable::ensure_dir(&dir)?;
        let meta_path = self.index_meta_path(table_id, index_id);
        let record = IndexMetadata::new(index_id, table_id)?;
        match metadata::read_optional(&meta_path)? {
            Some(bytes) => {
                let existing = IndexMetadata::decode(&bytes)?;
                if existing != record {
                    return Err(corruption(
                        "index metadata does not describe this index directory",
                    ));
                }
            }
            None => record.publish(&meta_path)?,
        }
        Ok(dir)
    }

    /// Reads and validates the metadata record of an index directory.
    pub fn read_index_meta(&self, table_id: TableId, index_id: IndexId) -> Result<IndexMetadata> {
        let bytes = metadata::read(&self.index_meta_path(table_id, index_id), "index metadata")?;
        IndexMetadata::decode(&bytes)
    }

    /// Enumerates the indexes of one table in deterministic identity order.
    pub fn discover_index_ids(&self, table_id: TableId) -> Result<Vec<IndexId>> {
        let dir = self.table_indexes_dir(table_id);
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
            if let Some(index_id) = index_from_dir_name(&name) {
                ids.push(index_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }
}

/// Durable identity of one index directory.
///
/// The record binds an index directory to its owning table, which is what makes
/// an index directory meaningful: the layout renders the index's logical object
/// as `I-<object id>` inside the directory of the table that owns it, and the
/// owning table is recorded here rather than inferred from a path.
///
/// # Format
///
/// ```text
/// magic[4] = PLIX | format_version[u32] = 1 | header_len[u32] = 40
/// index_id[u64] | table_id[u64] | checksum[u32] | reserved[8] = 0
/// ```
///
/// The checksum is CRC32C over `record[0..28]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexMetadata {
    /// Identity of the index directory.
    pub index_id: IndexId,
    /// Table that owns the index.
    pub table_id: TableId,
}

impl IndexMetadata {
    /// Creates index metadata.
    pub fn new(index_id: IndexId, table_id: TableId) -> Result<Self> {
        if index_id.is_zero() {
            return Err(corruption("index metadata has a zero index ID"));
        }
        if table_id.is_zero() {
            return Err(corruption("index metadata has a zero owning table ID"));
        }
        Ok(Self { index_id, table_id })
    }

    /// Encodes the record deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(INDEX_META_HEADER_LEN);
        metadata::encode_prefix(
            &mut out,
            INDEX_META_MAGIC,
            INDEX_META_VERSION,
            INDEX_META_HEADER_LEN as u32,
        );
        put_u64(&mut out, self.index_id.get());
        put_u64(&mut out, self.table_id.get());
        let checksum = compute_checksum(&out[..META_CHECKSUM_OFFSET]);
        put_u32(&mut out, checksum);
        out.extend_from_slice(&[0_u8; 8]);
        Ok(out)
    }

    /// Decodes and validates a metadata image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (mut cursor, header_len) = metadata::decode_prefix(
            bytes,
            INDEX_META_MAGIC,
            INDEX_META_VERSION,
            "index metadata",
        )?;
        metadata::ensure_header_len(bytes, header_len, "index metadata")?;
        let index_id = IndexId::new(cursor.u64("index metadata")?);
        let table_id = TableId::new(cursor.u64("index metadata")?);
        let stored = cursor.u32("index metadata")?;
        verify_checksum(&bytes[..META_CHECKSUM_OFFSET], stored)?;
        metadata::ensure_reserved_zero(&bytes[cursor.position()..header_len], "index metadata")?;
        Self::new(index_id, table_id)
    }

    /// Publishes the record atomically at `path`.
    pub fn publish(&self, path: &Path) -> Result<()> {
        let bytes = self.encode()?;
        Self::decode(&bytes)?;
        metadata::publish(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        index_dir_name, index_from_dir_name, index_generation_file_name,
        index_generation_from_file_name, index_payload_file_name,
        index_payload_generation_from_file_name, IndexGenerationMetadata, IndexGenerationState,
        IndexGenerationTrigger, IndexMetadata, INDEX_GENERATION_META_HEADER_LEN,
        INDEX_META_HEADER_LEN,
    };
    use plomid_core::{DatabaseId, GenerationId, IndexId, SchemaId, TableId};

    #[test]
    fn directory_and_generation_names_round_trip() {
        assert_eq!(index_dir_name(IndexId::new(5)), "I-00000000000000000005");
        assert_eq!(
            index_from_dir_name("I-00000000000000000005"),
            Some(IndexId::new(5))
        );
        assert_eq!(index_from_dir_name("T-00000000000000000005"), None);
        assert_eq!(
            index_generation_file_name(GenerationId::new(9)),
            "GEN-00000000000000000009.dat"
        );
        assert_eq!(
            index_generation_from_file_name("GEN-00000000000000000009.dat"),
            Some(GenerationId::new(9))
        );
        assert_eq!(index_generation_from_file_name("META.dat"), None);
        assert_eq!(index_generation_from_file_name("GEN-9.dat"), None);
    }

    #[test]
    fn metadata_round_trips_and_detects_corruption() {
        let record = IndexMetadata::new(IndexId::new(2), TableId::new(1)).expect("record");
        let bytes = record.encode().expect("encode");
        assert_eq!(bytes.len(), INDEX_META_HEADER_LEN);
        assert_eq!(IndexMetadata::decode(&bytes).expect("decode"), record);
        let mut bad = bytes.clone();
        bad[20] ^= 0x01;
        assert!(IndexMetadata::decode(&bad).is_err());
        assert!(IndexMetadata::decode(&bytes[..12]).is_err());
    }

    #[test]
    fn index_generation_metadata_round_trips_and_detects_corruption() {
        let record = IndexGenerationMetadata::new(
            IndexId::new(2),
            GenerationId::new(9),
            DatabaseId::new(1),
            SchemaId::new(3),
            TableId::new(1),
            GenerationId::new(7),
            IndexGenerationTrigger::Manual,
            IndexGenerationState::Current,
        )
        .expect("record");
        let bytes = record.encode().expect("encode");
        assert_eq!(bytes.len(), INDEX_GENERATION_META_HEADER_LEN);
        assert_eq!(
            IndexGenerationMetadata::decode(&bytes).expect("decode"),
            record
        );

        // Every checksummed region rejects a single flipped byte.
        for offset in [12usize, 28, 44, 61] {
            let mut bad = bytes.clone();
            bad[offset] ^= 0x01;
            assert!(
                IndexGenerationMetadata::decode(&bad).is_err(),
                "corruption at offset {offset} must be rejected"
            );
        }

        // Truncation and a bad magic are rejected too.
        assert!(IndexGenerationMetadata::decode(&bytes[..20]).is_err());
        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0x01;
        assert!(IndexGenerationMetadata::decode(&bad_magic).is_err());

        // The lifecycle transition preserves identity and provenance.
        let retained = record
            .with_state(IndexGenerationState::Retained)
            .expect("transition");
        assert_eq!(retained.generation_id, record.generation_id);
        assert_eq!(
            retained.source_data_generation_id,
            record.source_data_generation_id
        );
        assert_eq!(retained.trigger, record.trigger);
        assert_eq!(retained.state, IndexGenerationState::Retained);
        assert!(!retained.state.is_current());
        assert!(record.is_compatible(GenerationId::new(7)));
        assert!(!record.is_compatible(GenerationId::new(8)));
    }

    #[test]
    fn index_generation_metadata_rejects_impossible_identity() {
        let zero_table = IndexGenerationMetadata::new(
            IndexId::new(1),
            GenerationId::new(1),
            DatabaseId::new(1),
            SchemaId::new(1),
            TableId::new(0),
            GenerationId::new(1),
            IndexGenerationTrigger::Automatic,
            IndexGenerationState::Current,
        );
        assert!(zero_table.is_err());
    }

    #[test]
    fn index_payload_names_round_trip_through_identity() {
        assert_eq!(
            index_payload_file_name(GenerationId::new(4)),
            "IDX-00000000000000000004.dat"
        );
        assert_eq!(
            index_payload_generation_from_file_name("IDX-00000000000000000004.dat"),
            Some(GenerationId::new(4))
        );
        // A generation record is not a payload, and neither is a stray name.
        assert_eq!(
            index_payload_generation_from_file_name("GEN-00000000000000000004.dat"),
            None
        );
        assert_eq!(index_payload_generation_from_file_name("IDX-4.dat"), None);
        assert_eq!(index_payload_generation_from_file_name("META.dat"), None);
    }
}
