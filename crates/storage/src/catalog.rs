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
//! Versioned catalog metadata: objects, schemas, physical references,
//! generation references, and publication state.
//!
//! The catalog is the durable metadata layer that tells PLOMID what objects
//! exist, which schema state each object was published against, which immutable
//! generation currently represents each object, which generations remain
//! retained, which physical structures those generations reference, and whether
//! each object is published or retired.
//!
//! # Catalog states
//!
//! ```text
//! <root>/catalog/catalog-<generation:020>.cat
//! ```
//!
//! A catalog file holds exactly one coherent catalog state. Files are immutable
//! and are never modified in place: a catalog change builds a new state
//! independently, validates it, synchronizes it, and publishes it with the
//! staged-file/atomic-rename policy shared with checkpoints and generations.
//! The authoritative record of which catalog file is current is the publication
//! pointer maintained by [`crate::generation`]; a catalog file is reachable
//! through that pointer, through a checkpoint that names its catalog version, or
//! through a live reader that is still using it.
//!
//! Because historical catalog files are retained, an older generation keeps
//! resolving the schema state it was published against instead of observing a
//! later one.
//!
//! # Format
//!
//! The format is explicit and versioned; raw Rust structs are never written and
//! no compiler layout detail is observable on disk. All integers are
//! little-endian. The fixed header is followed by exactly `records_len` bytes of
//! object records; trailing data is rejected.
//!
//! ```text
//! magic[4] = PLCT            | format_version[u32] = 1     | header_len[u32] = 72
//! catalog_version[u64]       | catalog_generation[u64]     | storage_generation[u64]
//! checkpoint_lsn[u64]        | publication[u32]            | record_count[u32]
//! records_len[u32]           | checksum[u32]               | reserved[12] = 0
//! records[records_len]
//! ```
//!
//! The checksum is CRC32C over `header[0..56]` followed by the record bytes; the
//! checksum field itself and the reserved bytes are not covered. Readers
//! recompute and compare it before trusting any field beyond magic, version, and
//! length framing.
//!
//! Object records are variable length, carry their own length for
//! self-validation, and are sorted by object id. Every length, count, and offset
//! is checked before allocation, so malformed catalog data is rejected with a
//! structured error and never panics.
//!
//! # Logical identity versus physical location
//!
//! Object, schema, generation, segment, pack, block, page, and row identifiers
//! are logical: none of them encodes a device path, a file name, a byte offset,
//! a pointer, or a memory address. Physical location is carried only by
//! [`PhysicalReference`], which maps a logical object generation onto the
//! existing PLOMID physical hierarchy
//! (`object -> generation -> segment -> pack -> block -> page -> record`).

use crate::checksum::{crc_finalize, crc_init, crc_update};
use crate::codec::{checked_len, corruption, invalid, put_u32, put_u64, Cursor};
use plomid_core::{
    BlockId, CatalogVersion, ColumnId, ErrorKind, GenerationId, Lsn, ObjectId, PackId, PageId,
    PlomidError, Result, RowId, SchemaId, SegmentId, TableIdentity,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Catalog constants are defined once in `plomid_core::constants` and
/// re-exported here; module-local aliases keep the body unchanged.
pub use plomid_core::{
    CATALOG_CHECKSUMMED_PREFIX_LEN as CHECKSUMMED_PREFIX_LEN, CATALOG_DIR_NAME,
    CATALOG_FILE_PREFIX as FILE_PREFIX, CATALOG_FILE_SUFFIX as FILE_SUFFIX, CATALOG_FORMAT_VERSION,
    CATALOG_GENERATION_DIGITS, CATALOG_HEADER_SIZE, CATALOG_MAGIC,
    CATALOG_OFF_CATALOG_GENERATION as OFF_CATALOG_GENERATION,
    CATALOG_OFF_CATALOG_VERSION as OFF_CATALOG_VERSION,
    CATALOG_OFF_CHECKPOINT_LSN as OFF_CHECKPOINT_LSN, CATALOG_OFF_CHECKSUM as OFF_CHECKSUM,
    CATALOG_OFF_HEADER_LEN as OFF_HEADER_LEN, CATALOG_OFF_PUBLICATION as OFF_PUBLICATION,
    CATALOG_OFF_RECORDS_LEN as OFF_RECORDS_LEN, CATALOG_OFF_RECORD_COUNT as OFF_RECORD_COUNT,
    CATALOG_OFF_RESERVED as OFF_RESERVED, CATALOG_OFF_STORAGE_GENERATION as OFF_STORAGE_GENERATION,
    CATALOG_OFF_VERSION as OFF_VERSION, CATALOG_TMP_SUFFIX as TMP_SUFFIX, MAX_CATALOG_RECORDS,
    MAX_PHYSICAL_REFERENCES, MAX_RETAINED_GENERATIONS, MAX_SCHEMA_COLUMNS, MIN_CATALOG_RECORD_LEN,
    PHYSICAL_REFERENCE_LEN, SCHEMA_COLUMN_LEN,
};
/// Publication state of a catalog state, an object record, or a generation.
///
/// A durable metadata image is only trusted when it is marked
/// [`PublicationState::Published`]. [`PublicationState::Staged`] marks a state
/// that is still being built and must never be served to a reader, and
/// [`PublicationState::Retired`] marks an object that has been explicitly
/// retired while its generations remain retained.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u32)]
pub enum PublicationState {
    /// The image is under construction and is not authoritative.
    Staged = 0,
    /// The image is a complete published state.
    Published = 1,
    /// The object has been retired; its generations remain referenced.
    Retired = 2,
}

impl PublicationState {
    /// Decodes a persisted publication state.
    pub fn from_u32(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Staged),
            1 => Ok(Self::Published),
            2 => Ok(Self::Retired),
            _ => Err(corruption("unknown publication state")),
        }
    }

    /// Returns the persisted representation.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Returns true when the state is a published state.
    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Published)
    }
}

/// Physical location of object data inside the PLOMID physical hierarchy.
///
/// A structure is expressed purely through physical identifiers; byte offsets
/// and device paths are resolved by the pack layer and are deliberately absent.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalStructure {
    /// Storage segment holding the data.
    pub segment_id: SegmentId,
    /// Server pack holding the data.
    pub pack_id: PackId,
    /// Logical block holding the data.
    pub block_id: BlockId,
    /// Page within the block.
    pub page_id: PageId,
    /// Row within the page, when the reference names a single record.
    pub record_id: Option<RowId>,
}

impl PhysicalStructure {
    /// Creates a physical structure.
    #[must_use]
    pub fn new(
        segment_id: SegmentId,
        pack_id: PackId,
        block_id: BlockId,
        page_id: PageId,
        record_id: Option<RowId>,
    ) -> Self {
        Self {
            segment_id,
            pack_id,
            block_id,
            page_id,
            record_id,
        }
    }

    /// Validates that every physical identifier is present.
    ///
    /// Physical identifiers name durable structures, so zero is never a valid
    /// value; the record identifier is optional because a reference may name a
    /// whole page.
    pub fn validate(&self) -> Result<()> {
        if self.segment_id.is_zero() {
            return Err(corruption("physical reference has a zero segment ID"));
        }
        if self.pack_id.is_zero() {
            return Err(corruption("physical reference has a zero pack ID"));
        }
        if self.block_id.is_zero() {
            return Err(corruption("physical reference has a zero block ID"));
        }
        if self.page_id.is_zero() {
            return Err(corruption("physical reference has a zero page ID"));
        }
        if self.record_id.is_some_and(RowId::is_zero) {
            return Err(corruption("physical reference has a zero record ID"));
        }
        Ok(())
    }

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        put_u64(out, self.segment_id.get());
        put_u64(out, self.pack_id.get());
        put_u64(out, self.block_id.get());
        put_u64(out, self.page_id.get());
        match self.record_id {
            Some(record_id) => {
                out.push(1);
                put_u64(out, record_id.get());
            }
            None => {
                out.push(0);
                put_u64(out, 0);
            }
        }
    }

    pub(crate) fn decode(cursor: &mut Cursor<'_>) -> Result<Self> {
        let segment_id = SegmentId::new(cursor.u64("physical reference segment ID")?);
        let pack_id = PackId::new(cursor.u64("physical reference pack ID")?);
        let block_id = BlockId::new(cursor.u64("physical reference block ID")?);
        let page_id = PageId::new(cursor.u64("physical reference page ID")?);
        let present = cursor.u8("physical reference record presence")?;
        let record_id = RowId::new(cursor.u64("physical reference record ID")?);
        let record_id = match present {
            0 => None,
            1 => Some(record_id),
            _ => {
                return Err(corruption(
                    "physical reference has an invalid record presence byte",
                ))
            }
        };
        let structure = Self {
            segment_id,
            pack_id,
            block_id,
            page_id,
            record_id,
        };
        structure.validate()?;
        Ok(structure)
    }
}
/// Durable reference from a logical object generation to a physical structure.
///
/// The reference keeps logical identity (`object_id`, `generation_id`) separate
/// from physical location ([`PhysicalStructure`]). It is persisted with the
/// object's catalog record and with the generation metadata, and the two copies
/// must agree.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalReference {
    /// Logical object the reference belongs to.
    pub object_id: ObjectId,
    /// Immutable generation the reference belongs to.
    pub generation_id: GenerationId,
    /// Physical structure holding the data.
    pub structure: PhysicalStructure,
}

impl PhysicalReference {
    /// Creates a durable physical reference.
    #[must_use]
    pub fn new(
        object_id: ObjectId,
        generation_id: GenerationId,
        structure: PhysicalStructure,
    ) -> Self {
        Self {
            object_id,
            generation_id,
            structure,
        }
    }

    /// Validates the reference.
    pub fn validate(&self) -> Result<()> {
        if self.object_id.is_zero() {
            return Err(corruption("physical reference has a zero object ID"));
        }
        if self.generation_id.is_zero() {
            return Err(corruption("physical reference has a zero generation ID"));
        }
        self.structure.validate()
    }

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        put_u64(out, self.object_id.get());
        put_u64(out, self.generation_id.get());
        self.structure.encode_into(out);
    }

    pub(crate) fn decode(cursor: &mut Cursor<'_>) -> Result<Self> {
        let object_id = ObjectId::new(cursor.u64("physical reference object ID")?);
        let generation_id = GenerationId::new(cursor.u64("physical reference generation ID")?);
        let structure = PhysicalStructure::decode(cursor)?;
        let reference = Self {
            object_id,
            generation_id,
            structure,
        };
        reference.validate()?;
        Ok(reference)
    }
}

/// One column of a versioned schema state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SchemaColumn {
    /// Stable column identity within the schema.
    pub column_id: ColumnId,
    /// Logical type identifier assigned by the type layer.
    ///
    /// The catalog stores the code opaquely and never interprets it, so schema
    /// metadata can evolve without a catalog format change.
    pub type_code: u32,
}

impl SchemaColumn {
    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        put_u64(out, self.column_id.get());
        put_u32(out, self.type_code);
    }

    pub(crate) fn decode(cursor: &mut Cursor<'_>) -> Result<Self> {
        let column_id = ColumnId::new(cursor.u64("schema column ID")?);
        let type_code = cursor.u32("schema column type code")?;
        if column_id.is_zero() {
            return Err(corruption("schema column has a zero column ID"));
        }
        Ok(Self {
            column_id,
            type_code,
        })
    }
}
/// Versioned schema metadata associated with an object.
///
/// A schema state is identified by its schema identity and its schema version.
/// Publishing a schema change creates a new schema version; previously
/// published versions stay resolvable through the catalog files that reference
/// them, so a historical generation always maps to the schema state it was
/// published against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaMetadata {
    /// Stable schema identity.
    pub schema_id: SchemaId,
    /// Monotonic schema version.
    pub schema_version: CatalogVersion,
    /// Columns in ascending column-id order.
    pub columns: Vec<SchemaColumn>,
}

impl SchemaMetadata {
    /// Creates a schema state, normalizing columns into deterministic order.
    pub fn new(
        schema_id: SchemaId,
        schema_version: CatalogVersion,
        mut columns: Vec<SchemaColumn>,
    ) -> Result<Self> {
        columns.sort_unstable();
        let schema = Self {
            schema_id,
            schema_version,
            columns,
        };
        schema.validate()?;
        Ok(schema)
    }

    /// Validates schema identity, version, and column ordering.
    pub fn validate(&self) -> Result<()> {
        if self.schema_id.is_zero() {
            return Err(corruption("schema metadata has a zero schema ID"));
        }
        if self.schema_version.get() == u64::MAX {
            return Err(corruption("schema metadata has an impossible version"));
        }
        if self.columns.len() > MAX_SCHEMA_COLUMNS {
            return Err(corruption("schema metadata has too many columns"));
        }
        let mut previous: Option<ColumnId> = None;
        for column in &self.columns {
            if column.column_id.is_zero() {
                return Err(corruption("schema metadata has a zero column ID"));
            }
            if previous == Some(column.column_id) {
                return Err(corruption("schema metadata repeats a column ID"));
            }
            previous = Some(column.column_id);
        }
        Ok(())
    }

    fn encode_into(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        put_u64(out, self.schema_id.get());
        put_u64(out, self.schema_version.get());
        let count = u32::try_from(self.columns.len())
            .map_err(|_| invalid("schema metadata has too many columns"))?;
        put_u32(out, count);
        for column in &self.columns {
            column.encode_into(out);
        }
        Ok(())
    }

    pub(crate) fn decode(cursor: &mut Cursor<'_>) -> Result<Self> {
        let schema_id = SchemaId::new(cursor.u64("schema ID")?);
        let schema_version = CatalogVersion::new(cursor.u64("schema version")?);
        let count = cursor.u32("schema column count")? as usize;
        if count > MAX_SCHEMA_COLUMNS {
            return Err(corruption("schema metadata has too many columns"));
        }
        let needed = checked_len(count, SCHEMA_COLUMN_LEN)
            .ok_or_else(|| corruption("schema length overflow"))?;
        if needed > cursor.remaining() {
            return Err(corruption("schema columns are truncated"));
        }
        let mut columns = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            columns.push(SchemaColumn::decode(cursor)?);
        }
        let schema = Self {
            schema_id,
            schema_version,
            columns,
        };
        schema.validate()?;
        Ok(schema)
    }
}
/// Durable catalog record for one object.
///
/// The record ties a stable logical object identity to the schema state it was
/// published against, to its current immutable generation, to the generations
/// that remain retained for it, to the physical structures its generations
/// reference, and to its publication state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectRecord {
    /// Stable logical object identity.
    pub object_id: ObjectId,
    /// Owning database → schema → table location of the object.
    ///
    /// Durable since catalog format version 2; legacy version-1 images that
    /// lack it decode with the compatibility mapping
    /// (`object_id` as table, default database, `schema.schema_id`).
    pub table_identity: TableIdentity,
    /// Schema state this object was published against.
    pub schema: SchemaMetadata,
    /// Immutable generation that currently represents the object.
    pub current_generation: GenerationId,
    /// Generations retained for the object, in ascending order.
    pub retained_generations: Vec<GenerationId>,
    /// Physical references of the object's generations, in deterministic order.
    pub references: Vec<PhysicalReference>,
    /// Publication state of the object.
    pub publication: PublicationState,
}

impl ObjectRecord {
    /// Creates a record, normalizing ordering.
    ///
    /// Retained generations and physical references are sorted into
    /// deterministic order and de-duplicated so that catalog encoding is
    /// deterministic regardless of how a caller assembled the input.
    ///
    /// The owning identity is derived by the same compatibility rule as
    /// `ObjectChange::new`; hierarchy writers should prefer
    /// [`Self::with_identity`] to persist the real location.
    pub fn new(
        object_id: ObjectId,
        schema: SchemaMetadata,
        current_generation: GenerationId,
        retained_generations: Vec<GenerationId>,
        references: Vec<PhysicalReference>,
        publication: PublicationState,
    ) -> Result<Self> {
        let table_identity = TableIdentity::from_object_and_schema(object_id, schema.schema_id);
        Self::with_identity(
            object_id,
            table_identity,
            schema,
            current_generation,
            retained_generations,
            references,
            publication,
        )
    }

    /// Creates a record with an explicit owning `TableIdentity`.
    pub fn with_identity(
        object_id: ObjectId,
        table_identity: TableIdentity,
        schema: SchemaMetadata,
        current_generation: GenerationId,
        mut retained_generations: Vec<GenerationId>,
        mut references: Vec<PhysicalReference>,
        publication: PublicationState,
    ) -> Result<Self> {
        retained_generations.retain(|generation| *generation != current_generation);
        retained_generations.sort_unstable();
        retained_generations.dedup();
        references.sort_unstable();
        references.dedup();
        let record = Self {
            object_id,
            table_identity,
            schema,
            current_generation,
            retained_generations,
            references,
            publication,
        };
        record.validate()?;
        Ok(record)
    }

    /// Validates the record's identity, ordering, and internal relationships.
    pub fn validate(&self) -> Result<()> {
        if self.object_id.is_zero() {
            return Err(corruption("catalog record has a zero object ID"));
        }
        if self.table_identity.is_zero() {
            return Err(corruption("catalog record has a zero table identity"));
        }
        if self.table_identity.table_id.get() != self.object_id.get() {
            return Err(corruption(
                "catalog record table identity disagrees with its object ID",
            ));
        }
        if self.table_identity.schema_id != self.schema.schema_id {
            return Err(corruption(
                "catalog record table identity disagrees with its schema state",
            ));
        }
        if self.current_generation.is_zero() {
            return Err(corruption("catalog record has a zero current generation"));
        }
        if !matches!(
            self.publication,
            PublicationState::Published | PublicationState::Retired
        ) {
            return Err(corruption("catalog record has an unpublished object state"));
        }
        self.schema.validate()?;
        if self.retained_generations.len() > MAX_RETAINED_GENERATIONS {
            return Err(corruption(
                "catalog record has too many retained generations",
            ));
        }
        let mut previous: Option<GenerationId> = None;
        for generation in &self.retained_generations {
            if generation.is_zero() {
                return Err(corruption("catalog record has a zero retained generation"));
            }
            if *generation == self.current_generation {
                return Err(corruption(
                    "catalog record retains its own current generation",
                ));
            }
            if previous.is_some_and(|value| value >= *generation) {
                return Err(corruption(
                    "catalog record retained generations are not strictly ordered",
                ));
            }
            previous = Some(*generation);
        }
        if self.references.len() > MAX_PHYSICAL_REFERENCES {
            return Err(corruption(
                "catalog record has too many physical references",
            ));
        }
        let mut previous: Option<&PhysicalReference> = None;
        for reference in &self.references {
            reference.validate()?;
            if reference.object_id != self.object_id {
                return Err(corruption("catalog record references a different object"));
            }
            if !self.owns_generation(reference.generation_id) {
                return Err(corruption(
                    "catalog record references a generation it does not own",
                ));
            }
            if previous.is_some_and(|value| value >= reference) {
                return Err(corruption(
                    "catalog record physical references are not strictly ordered",
                ));
            }
            previous = Some(reference);
        }
        Ok(())
    }

    /// Returns true when the object is published rather than retired.
    #[must_use]
    pub fn is_published(&self) -> bool {
        self.publication.is_published()
    }

    /// Returns true when the record owns `generation`.
    ///
    /// Retained generations are still owned by the object, which is what keeps
    /// them reachable for garbage collection.
    #[must_use]
    pub fn owns_generation(&self, generation: GenerationId) -> bool {
        self.current_generation == generation || self.retained_generations.contains(&generation)
    }

    /// Returns every generation owned by the object in ascending order.
    #[must_use]
    pub fn generation_ids(&self) -> Vec<GenerationId> {
        let mut generations = self.retained_generations.clone();
        generations.push(self.current_generation);
        generations.sort_unstable();
        generations
    }
}
impl ObjectRecord {
    /// Returns a copy of the record without `generation` retained.
    ///
    /// The physical references of the released generation are dropped with its
    /// retention: a record only carries references for generations it owns, so
    /// releasing retention is what makes those references, and the generation
    /// itself, collectable.
    pub fn without_retention(&self, generation: GenerationId) -> Result<Self> {
        if self.current_generation == generation {
            return Err(invalid(
                "the current generation of an object cannot be released",
            ));
        }
        if !self.retained_generations.contains(&generation) {
            return Err(PlomidError::new(
                ErrorKind::NotFound,
                "generation is not retained by this object",
            ));
        }
        let mut retained = self.retained_generations.clone();
        retained.retain(|value| *value != generation);
        let mut record = self.clone();
        record.retained_generations = retained;
        record
            .references
            .retain(|reference| reference.generation_id != generation);
        record.validate()?;
        Ok(record)
    }

    /// Returns a copy of the record marked as retired.
    pub fn as_retired(&self) -> Result<Self> {
        let mut record = self.clone();
        record.publication = PublicationState::Retired;
        record.validate()?;
        Ok(record)
    }

    fn encode_into(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let mut body = Vec::new();
        put_u64(&mut body, self.object_id.get());
        put_u64(&mut body, self.table_identity.database_id.get());
        put_u64(&mut body, self.table_identity.schema_id.get());
        put_u64(&mut body, self.table_identity.table_id.get());
        self.schema.encode_into(&mut body)?;
        put_u64(&mut body, self.current_generation.get());
        let retained_count = u32::try_from(self.retained_generations.len())
            .map_err(|_| invalid("catalog record has too many retained generations"))?;
        put_u32(&mut body, retained_count);
        for generation in &self.retained_generations {
            put_u64(&mut body, generation.get());
        }
        let reference_count = u32::try_from(self.references.len())
            .map_err(|_| invalid("catalog record has too many physical references"))?;
        put_u32(&mut body, reference_count);
        for reference in &self.references {
            reference.encode_into(&mut body);
        }
        put_u32(&mut body, self.publication.as_u32());
        let total = body
            .len()
            .checked_add(4)
            .ok_or_else(|| invalid("catalog record length overflow"))?;
        let total = u32::try_from(total).map_err(|_| invalid("catalog record is too large"))?;
        put_u32(out, total);
        out.extend_from_slice(&body);
        Ok(())
    }

    pub(crate) fn decode_versioned(cursor: &mut Cursor<'_>, legacy_v1: bool) -> Result<Self> {
        let record_len = cursor.u32("object record length")? as usize;
        if record_len < MIN_CATALOG_RECORD_LEN {
            return Err(corruption("object record length is invalid"));
        }
        if record_len - 4 > cursor.remaining() {
            return Err(corruption("object record is truncated"));
        }
        let start = cursor.position();
        let object_id = ObjectId::new(cursor.u64("object ID")?);
        // Catalog format version 2 stores the owning `Database → Schema →
        // Table` identity right after the object ID (3 × u64); version-1
        // images predate it. The catalog-level format version selects the
        // variant deterministically, so no length heuristic is needed.
        let table_identity = if legacy_v1 {
            // Placeholder; replaced by the compatibility mapping after the
            // schema state is decoded.
            TableIdentity::default()
        } else {
            let database_id = plomid_core::DatabaseId::new(cursor.u64("object database ID")?);
            let schema_id = SchemaId::new(cursor.u64("object schema ID")?);
            let table_id = plomid_core::TableId::new(cursor.u64("object table ID")?);
            TableIdentity::new(database_id, schema_id, table_id)
        };
        let schema = SchemaMetadata::decode(cursor)?;
        let table_identity = if legacy_v1 {
            TableIdentity::from_object_and_schema(object_id, schema.schema_id)
        } else {
            table_identity
        };
        let current_generation = GenerationId::new(cursor.u64("current generation")?);
        let retained_count = cursor.u32("retained generation count")? as usize;
        if retained_count > MAX_RETAINED_GENERATIONS {
            return Err(corruption(
                "catalog record has too many retained generations",
            ));
        }
        let needed = checked_len(retained_count, 8)
            .ok_or_else(|| corruption("retained generation length overflow"))?;
        if needed > cursor.remaining() {
            return Err(corruption("retained generations are truncated"));
        }
        let mut retained_generations = Vec::with_capacity(retained_count.min(64));
        for _ in 0..retained_count {
            retained_generations.push(GenerationId::new(cursor.u64("retained generation ID")?));
        }
        let reference_count = cursor.u32("physical reference count")? as usize;
        if reference_count > MAX_PHYSICAL_REFERENCES {
            return Err(corruption(
                "catalog record has too many physical references",
            ));
        }
        let needed = checked_len(reference_count, PHYSICAL_REFERENCE_LEN)
            .ok_or_else(|| corruption("physical reference length overflow"))?;
        if needed > cursor.remaining() {
            return Err(corruption("physical references are truncated"));
        }
        let mut references = Vec::with_capacity(reference_count.min(64));
        for _ in 0..reference_count {
            references.push(PhysicalReference::decode(cursor)?);
        }
        let publication = PublicationState::from_u32(cursor.u32("object publication state")?)?;
        let consumed = cursor
            .position()
            .checked_sub(start)
            .and_then(|value| value.checked_add(4))
            .ok_or_else(|| corruption("object record length overflow"))?;
        if consumed != record_len {
            return Err(corruption("object record length is invalid"));
        }
        let record = Self {
            object_id,
            table_identity,
            schema,
            current_generation,
            retained_generations,
            references,
            publication,
        };
        record.validate()?;
        Ok(record)
    }
}
/// A complete, coherent catalog state.
///
/// A state carries its catalog version, the generation it was published as, the
/// durable storage generation and WAL boundary it describes, its publication
/// state, and its object records in ascending object-id order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogState {
    catalog_version: CatalogVersion,
    catalog_generation: GenerationId,
    storage_generation: GenerationId,
    checkpoint_lsn: Lsn,
    publication: PublicationState,
    records: Vec<ObjectRecord>,
}

/// Validates a record list for identity ordering and size, then sorts it.
fn sorted_records(mut records: Vec<ObjectRecord>) -> Result<Vec<ObjectRecord>> {
    if records.len() > MAX_CATALOG_RECORDS {
        return Err(invalid("catalog has too many object records"));
    }
    for record in &records {
        record.validate()?;
    }
    records.sort_unstable_by_key(|record| record.object_id);
    for pair in records.windows(2) {
        if pair[0].object_id == pair[1].object_id {
            return Err(invalid("catalog repeats an object ID"));
        }
    }
    Ok(records)
}

impl CatalogState {
    /// Creates a staged catalog state from a record list.
    ///
    /// A staged state is not authoritative and must be published before any
    /// reader may observe it.
    pub fn staged(
        catalog_version: CatalogVersion,
        catalog_generation: GenerationId,
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
        records: Vec<ObjectRecord>,
    ) -> Result<Self> {
        let state = Self {
            catalog_version,
            catalog_generation,
            storage_generation,
            checkpoint_lsn,
            publication: PublicationState::Staged,
            records: sorted_records(records)?,
        };
        state.validate()?;
        Ok(state)
    }

    /// Returns the catalog version of this state.
    #[must_use]
    pub fn catalog_version(&self) -> CatalogVersion {
        self.catalog_version
    }

    /// Returns the generation this catalog state was published as.
    #[must_use]
    pub fn catalog_generation(&self) -> GenerationId {
        self.catalog_generation
    }

    /// Returns the durable storage generation this state describes.
    #[must_use]
    pub fn storage_generation(&self) -> GenerationId {
        self.storage_generation
    }

    /// Returns the WAL boundary this state describes.
    #[must_use]
    pub fn checkpoint_lsn(&self) -> Lsn {
        self.checkpoint_lsn
    }

    /// Returns the publication state of this catalog image.
    #[must_use]
    pub fn publication(&self) -> PublicationState {
        self.publication
    }

    /// Returns true when this state is marked published.
    #[must_use]
    pub fn is_published(&self) -> bool {
        self.publication.is_published()
    }

    /// Returns the object records in ascending object-id order.
    #[must_use]
    pub fn records(&self) -> &[ObjectRecord] {
        &self.records
    }

    /// Returns the number of object records.
    #[must_use]
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// Returns the record for `object_id`, if the object is catalogued.
    #[must_use]
    pub fn object(&self, object_id: ObjectId) -> Option<&ObjectRecord> {
        self.records
            .binary_search_by_key(&object_id, |record| record.object_id)
            .ok()
            .map(|index| &self.records[index])
    }

    /// Returns the current generation of `object_id`, if the object exists.
    #[must_use]
    pub fn generation_of(&self, object_id: ObjectId) -> Option<GenerationId> {
        self.object(object_id)
            .map(|record| record.current_generation)
    }

    /// Returns true when any record owns `generation`.
    #[must_use]
    pub fn owns_generation(&self, generation: GenerationId) -> bool {
        self.records
            .iter()
            .any(|record| record.owns_generation(generation))
    }

    /// Returns the record that owns `generation`, if any.
    #[must_use]
    pub fn owner_of(&self, generation: GenerationId) -> Option<&ObjectRecord> {
        self.records
            .iter()
            .find(|record| record.owns_generation(generation))
    }

    /// Returns every generation owned by the state in ascending order.
    #[must_use]
    pub fn generation_ids(&self) -> Vec<GenerationId> {
        let mut generations = Vec::new();
        for record in &self.records {
            generations.extend(record.generation_ids());
        }
        generations.sort_unstable();
        generations.dedup();
        generations
    }
}
impl CatalogState {
    /// Returns a new state with `record` inserted or replaced.
    ///
    /// The receiver is consumed, so a published catalog state can never be
    /// modified in place: a catalog change always produces a new state.
    pub fn upsert(mut self, record: ObjectRecord) -> Result<Self> {
        record.validate()?;
        match self
            .records
            .binary_search_by_key(&record.object_id, |existing| existing.object_id)
        {
            Ok(index) => self.records[index] = record,
            Err(index) => self.records.insert(index, record),
        }
        self.validate()?;
        Ok(self)
    }

    /// Returns a new state without the record for `object_id`.
    pub fn remove_object(mut self, object_id: ObjectId) -> Result<Self> {
        let index = self
            .records
            .binary_search_by_key(&object_id, |record| record.object_id)
            .map_err(|_| PlomidError::new(ErrorKind::NotFound, "object is not catalogued"))?;
        self.records.remove(index);
        self.validate()?;
        Ok(self)
    }

    /// Returns a new state in which `object_id` is marked retired.
    ///
    /// The object stays catalogued so its generations remain referenced, which
    /// keeps them reachable for garbage collection.
    pub fn retire_object(mut self, object_id: ObjectId) -> Result<Self> {
        let index = self
            .records
            .binary_search_by_key(&object_id, |record| record.object_id)
            .map_err(|_| PlomidError::new(ErrorKind::NotFound, "object is not catalogued"))?;
        self.records[index] = self.records[index].as_retired()?;
        self.validate()?;
        Ok(self)
    }

    /// Returns a new state in which `generation` is no longer retained.
    ///
    /// Releasing retention does not delete anything: it removes the durable
    /// reference that keeps the generation alive, after which garbage
    /// collection may reclaim it once no reader still requires it.
    pub fn release_generation(mut self, generation: GenerationId) -> Result<Self> {
        if self
            .records
            .iter()
            .any(|record| record.current_generation == generation)
        {
            return Err(invalid(
                "the current generation of an object cannot be released",
            ));
        }
        let mut found = false;
        for record in &mut self.records {
            if record.retained_generations.contains(&generation) {
                *record = record.without_retention(generation)?;
                found = true;
            }
        }
        if !found {
            return Err(PlomidError::new(
                ErrorKind::NotFound,
                "generation is not retained by the catalog",
            ));
        }
        self.validate()?;
        Ok(self)
    }

    /// Returns a new state describing a different publication boundary.
    ///
    /// The copy keeps the same records, which is what makes a publication an
    /// independent state built from the previously published one rather than a
    /// mutation of it.
    pub fn with_boundary(
        mut self,
        catalog_version: CatalogVersion,
        catalog_generation: GenerationId,
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
    ) -> Result<Self> {
        self.catalog_version = catalog_version;
        self.catalog_generation = catalog_generation;
        self.storage_generation = storage_generation;
        self.checkpoint_lsn = checkpoint_lsn;
        self.validate()?;
        Ok(self)
    }

    /// Returns the state marked staged.
    pub fn as_staged(mut self) -> Self {
        self.publication = PublicationState::Staged;
        self
    }

    /// Returns the state marked published.
    pub fn as_published(mut self) -> Self {
        self.publication = PublicationState::Published;
        self
    }
}
impl CatalogState {
    /// Validates the state's boundary fields, publication state, and records.
    pub fn validate(&self) -> Result<()> {
        if self.catalog_version.get() == 0 || self.catalog_version.get() == u64::MAX {
            return Err(corruption("catalog has an impossible version"));
        }
        if self.catalog_generation.is_zero() || self.catalog_generation.get() == u64::MAX {
            return Err(corruption("catalog has an impossible generation"));
        }
        if self.storage_generation.is_zero() || self.storage_generation.get() == u64::MAX {
            return Err(corruption("catalog has an impossible storage generation"));
        }
        if self.checkpoint_lsn.get() == u64::MAX {
            return Err(corruption("catalog has an invalid LSN"));
        }
        if !matches!(
            self.publication,
            PublicationState::Staged | PublicationState::Published
        ) {
            return Err(corruption("catalog has an invalid publication state"));
        }
        if self.records.len() > MAX_CATALOG_RECORDS {
            return Err(corruption("catalog has too many object records"));
        }
        let mut previous: Option<ObjectId> = None;
        for record in &self.records {
            record.validate()?;
            if previous.is_some_and(|object_id| object_id >= record.object_id) {
                return Err(corruption(
                    "catalog object records are not strictly ordered",
                ));
            }
            previous = Some(record.object_id);
        }
        Ok(())
    }

    /// Encodes the state deterministically as header followed by records.
    ///
    /// Encoding is a pure function of the state: records are ordered, counts are
    /// derived from the data, and the checksum covers the header prefix plus the
    /// record bytes, so the same state always produces the same image.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut records = Vec::new();
        for record in &self.records {
            record.encode_into(&mut records)?;
        }
        let record_count = u32::try_from(self.records.len())
            .map_err(|_| invalid("catalog has too many object records"))?;
        let records_len =
            u32::try_from(records.len()).map_err(|_| invalid("catalog records are too large"))?;
        let total = CATALOG_HEADER_SIZE
            .checked_add(records.len())
            .ok_or_else(|| invalid("catalog length overflow"))?;
        let mut bytes = vec![0_u8; total];
        bytes[0..4].copy_from_slice(&CATALOG_MAGIC);
        bytes[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&CATALOG_FORMAT_VERSION.to_le_bytes());
        bytes[OFF_HEADER_LEN..OFF_HEADER_LEN + 4]
            .copy_from_slice(&(CATALOG_HEADER_SIZE as u32).to_le_bytes());
        bytes[OFF_CATALOG_VERSION..OFF_CATALOG_VERSION + 8]
            .copy_from_slice(&self.catalog_version.get().to_le_bytes());
        bytes[OFF_CATALOG_GENERATION..OFF_CATALOG_GENERATION + 8]
            .copy_from_slice(&self.catalog_generation.get().to_le_bytes());
        bytes[OFF_STORAGE_GENERATION..OFF_STORAGE_GENERATION + 8]
            .copy_from_slice(&self.storage_generation.get().to_le_bytes());
        bytes[OFF_CHECKPOINT_LSN..OFF_CHECKPOINT_LSN + 8]
            .copy_from_slice(&self.checkpoint_lsn.get().to_le_bytes());
        bytes[OFF_PUBLICATION..OFF_PUBLICATION + 4]
            .copy_from_slice(&self.publication.as_u32().to_le_bytes());
        bytes[OFF_RECORD_COUNT..OFF_RECORD_COUNT + 4].copy_from_slice(&record_count.to_le_bytes());
        bytes[OFF_RECORDS_LEN..OFF_RECORDS_LEN + 4].copy_from_slice(&records_len.to_le_bytes());
        let checksum = checksum_of(&bytes[..CHECKSUMMED_PREFIX_LEN], &records);
        bytes[OFF_CHECKSUM..OFF_CHECKSUM + 4].copy_from_slice(&checksum.to_le_bytes());
        bytes[OFF_RESERVED..CATALOG_HEADER_SIZE].fill(0);
        bytes[CATALOG_HEADER_SIZE..].copy_from_slice(&records);
        Ok(bytes)
    }
}
impl CatalogState {
    /// Decodes and fully validates a catalog image.
    ///
    /// Every field is length-checked before it is used, the checksum is
    /// recomputed before any field beyond framing is trusted, and the decoded
    /// state is validated, so a malformed image is rejected with a structured
    /// error and can never be presented as a valid catalog.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(bytes);
        if cursor.fixed::<4>("catalog magic")? != CATALOG_MAGIC {
            return Err(corruption("invalid catalog magic"));
        }
        let version = cursor.u32("catalog format version")?;
        if version != CATALOG_FORMAT_VERSION && version != CATALOG_FORMAT_VERSION - 1 {
            if version > CATALOG_FORMAT_VERSION {
                return Err(PlomidError::new(
                    ErrorKind::Unsupported,
                    "unsupported catalog format version",
                ));
            }
            return Err(corruption("unsupported catalog format version"));
        }
        let legacy_v1 = version == CATALOG_FORMAT_VERSION - 1;
        let header_len = cursor.u32("catalog header length")? as usize;
        if header_len != CATALOG_HEADER_SIZE {
            return Err(corruption("catalog header length is invalid"));
        }
        let catalog_version = CatalogVersion::new(cursor.u64("catalog version")?);
        let catalog_generation = GenerationId::new(cursor.u64("catalog generation")?);
        let storage_generation = GenerationId::new(cursor.u64("catalog storage generation")?);
        let checkpoint_lsn = Lsn::new(cursor.u64("catalog checkpoint LSN")?);
        let publication = PublicationState::from_u32(cursor.u32("catalog publication state")?)?;
        let record_count = cursor.u32("catalog record count")? as usize;
        let records_len = cursor.u32("catalog records length")? as usize;
        let expected_checksum = cursor.u32("catalog checksum")?;
        let reserved = cursor.fixed::<12>("catalog reserved bytes")?;
        if reserved.iter().any(|byte| *byte != 0) {
            return Err(corruption("catalog has non-zero reserved bytes"));
        }
        if record_count > MAX_CATALOG_RECORDS {
            return Err(corruption("catalog has too many object records"));
        }
        let expected_len = CATALOG_HEADER_SIZE
            .checked_add(records_len)
            .ok_or_else(|| corruption("catalog length overflow"))?;
        if bytes.len() != expected_len {
            return Err(corruption("catalog has unexpected trailing data"));
        }
        let needed = checked_len(record_count, MIN_CATALOG_RECORD_LEN)
            .ok_or_else(|| corruption("catalog record length overflow"))?;
        if needed > records_len {
            return Err(corruption("catalog record count is invalid"));
        }
        let actual_checksum = checksum_of(
            &bytes[..CHECKSUMMED_PREFIX_LEN],
            &bytes[CATALOG_HEADER_SIZE..expected_len],
        );
        if actual_checksum != expected_checksum {
            return Err(corruption(format!(
                "catalog checksum mismatch (expected {expected_checksum:#010x}, got {actual_checksum:#010x})"
            )));
        }
        let mut records = Vec::with_capacity(record_count.min(1024));
        for _ in 0..record_count {
            records.push(ObjectRecord::decode_versioned(&mut cursor, legacy_v1)?);
        }
        cursor.ensure_end("catalog records")?;
        let state = Self {
            catalog_version,
            catalog_generation,
            storage_generation,
            checkpoint_lsn,
            publication,
            records,
        };
        state.validate()?;
        Ok(state)
    }
}

/// Computes the catalog checksum over the header prefix and the record bytes.
///
/// The checksum field is not part of its own input, and the algorithm is the
/// canonical CRC32C component shared with pages, blocks, packs, WAL records,
/// and checkpoints.
fn checksum_of(prefix: &[u8], records: &[u8]) -> u32 {
    let mut crc = crc_init();
    crc = crc_update(crc, prefix);
    crc = crc_update(crc, records);
    crc_finalize(crc)
}
/// Returns the catalog directory for a storage root without creating it.
#[must_use]
pub fn catalog_dir(root: &Path) -> PathBuf {
    root.join(CATALOG_DIR_NAME)
}

/// Deterministic published file name for a catalog generation.
#[must_use]
pub fn catalog_file_name(generation: u64) -> String {
    format!("{FILE_PREFIX}{generation:020}{FILE_SUFFIX}")
}

/// Parses a published catalog file name into its generation.
///
/// Only the deterministic published form is accepted; staging artifacts and
/// unrelated files return `None`, so discovery never treats a partially written
/// image as a catalog candidate.
#[must_use]
pub fn generation_from_catalog_file_name(name: &str) -> Option<u64> {
    let number = name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;
    if number.len() != CATALOG_GENERATION_DIGITS
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let generation = number.parse::<u64>().ok()?;
    if generation == 0 {
        return None;
    }
    Some(generation)
}

/// Returns the published path of a catalog state.
#[must_use]
pub fn catalog_path(root: &Path, generation: GenerationId) -> PathBuf {
    catalog_dir(root).join(catalog_file_name(generation.get()))
}

/// Enumerates catalog candidates in deterministic generation order.
pub fn discover_catalogs(root: &Path) -> Result<Vec<PathBuf>> {
    let dir = catalog_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&dir).map_err(PlomidError::from)? {
        let entry = entry.map_err(PlomidError::from)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(generation) = generation_from_catalog_file_name(&name) {
            candidates.push((generation, entry.path()));
        }
    }
    candidates.sort_unstable_by_key(|(generation, _)| *generation);
    Ok(candidates.into_iter().map(|(_, path)| path).collect())
}

/// Reads and validates one catalog file.
pub fn load_catalog(path: &Path) -> Result<CatalogState> {
    let bytes = fs::read(path).map_err(PlomidError::from)?;
    CatalogState::decode(&bytes)
}

/// Loads the catalog state published as `generation`, if it exists.
///
/// The stored generation must match the file name it was found under, so a
/// renamed or partially copied file is rejected instead of trusted.
pub fn load_catalog_for_generation(
    root: &Path,
    generation: GenerationId,
) -> Result<Option<CatalogState>> {
    let path = catalog_path(root, generation);
    if !path.exists() {
        return Ok(None);
    }
    let state = load_catalog(&path)?;
    if state.catalog_generation() != generation {
        return Err(corruption(format!(
            "catalog file name does not match its generation ({generation})"
        )));
    }
    Ok(Some(state))
}

/// Loads the catalog state whose version is `version`, if one exists.
///
/// Recovery uses this to resolve the catalog a checkpoint names. A corrupt
/// candidate fails rather than being skipped, because accepting a different
/// version than the one the checkpoint selected would silently change the
/// durable state being recovered.
pub fn load_catalog_for_version(
    root: &Path,
    version: CatalogVersion,
) -> Result<Option<CatalogState>> {
    for path in discover_catalogs(root)? {
        let state = load_catalog(&path)?;
        if state.catalog_version() == version {
            return Ok(Some(state));
        }
    }
    Ok(None)
}

/// Returns the newest valid published catalog state and its path.
///
/// Candidates are examined in descending generation order and a malformed
/// candidate is skipped, so an older valid state can still be discovered. The
/// returned state is not necessarily the authoritative state: only the
/// publication pointer and the checkpoint-selection rules decide that.
pub fn latest_valid_catalog(root: &Path) -> Result<Option<(PathBuf, CatalogState)>> {
    for path in discover_catalogs(root)?.into_iter().rev() {
        match load_catalog(&path) {
            Ok(state) if state.is_published() => return Ok(Some((path, state))),
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::{
        catalog_file_name, generation_from_catalog_file_name, CatalogState, ObjectRecord,
        PhysicalReference, PhysicalStructure, PublicationState, SchemaColumn, SchemaMetadata,
        CATALOG_HEADER_SIZE, CATALOG_MAGIC, MAX_SCHEMA_COLUMNS, OFF_CATALOG_VERSION, OFF_CHECKSUM,
        OFF_HEADER_LEN, OFF_PUBLICATION, OFF_RECORDS_LEN, OFF_RECORD_COUNT, OFF_RESERVED,
        OFF_STORAGE_GENERATION,
    };
    use plomid_core::{
        BlockId, CatalogVersion, ColumnId, ErrorKind, GenerationId, Lsn, ObjectId, PackId, PageId,
        RowId, SchemaId, SegmentId,
    };

    fn structure(page: u64) -> PhysicalStructure {
        PhysicalStructure::new(
            SegmentId::new(1),
            PackId::new(1),
            BlockId::new(1),
            PageId::new(page),
            Some(RowId::new(page)),
        )
    }

    fn reference(object: u64, generation: u64, page: u64) -> PhysicalReference {
        PhysicalReference::new(
            ObjectId::new(object),
            GenerationId::new(generation),
            structure(page),
        )
    }

    fn schema(version: u64) -> SchemaMetadata {
        SchemaMetadata::new(
            SchemaId::new(7),
            CatalogVersion::new(version),
            vec![
                SchemaColumn {
                    column_id: ColumnId::new(1),
                    type_code: 23,
                },
                SchemaColumn {
                    column_id: ColumnId::new(2),
                    type_code: 25,
                },
            ],
        )
        .expect("schema")
    }

    fn record(object: u64, current: u64, retained: &[u64]) -> ObjectRecord {
        ObjectRecord::new(
            ObjectId::new(object),
            schema(1),
            GenerationId::new(current),
            retained.iter().copied().map(GenerationId::new).collect(),
            vec![reference(object, current, 1)],
            PublicationState::Published,
        )
        .expect("record")
    }

    fn state(records: Vec<ObjectRecord>) -> CatalogState {
        CatalogState::staged(
            CatalogVersion::new(1),
            GenerationId::new(9),
            GenerationId::new(1),
            Lsn::new(3),
            records,
        )
        .expect("state")
        .as_published()
    }

    #[test]
    fn records_normalize_into_deterministic_order() {
        let first = ObjectRecord::new(
            ObjectId::new(1),
            schema(1),
            GenerationId::new(5),
            vec![
                GenerationId::new(3),
                GenerationId::new(2),
                GenerationId::new(3),
                GenerationId::new(5),
            ],
            vec![reference(1, 5, 2), reference(1, 5, 1), reference(1, 5, 2)],
            PublicationState::Published,
        )
        .expect("record");
        assert_eq!(
            first
                .retained_generations
                .iter()
                .map(|generation| generation.get())
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(first.references.len(), 2);
        assert_eq!(first.references[0].structure.page_id.get(), 1);
        assert_eq!(first.generation_ids().len(), 3);
    }

    #[test]
    fn encoding_is_deterministic_and_round_trips() {
        let catalog = state(vec![record(2, 4, &[3]), record(1, 7, &[])]);
        let encoded = catalog.encode().expect("encode");
        assert_eq!(encoded, catalog.encode().expect("encode"));
        assert_eq!(CatalogState::decode(&encoded).expect("decode"), catalog);
        // Records are ordered by object id regardless of insertion order.
        let first = encoded[CATALOG_HEADER_SIZE + 4..CATALOG_HEADER_SIZE + 12].to_vec();
        assert_eq!(u64::from_le_bytes(first.try_into().expect("bytes")), 1);
    }

    #[test]
    fn staged_and_published_states_are_distinguishable() {
        let staged = state(vec![record(1, 2, &[])]).as_staged();
        assert!(!staged.is_published());
        let encoded = staged.encode().expect("encode");
        assert_eq!(
            CatalogState::decode(&encoded)
                .expect("decode")
                .publication(),
            PublicationState::Staged
        );
    }
    #[test]
    fn rejects_corruption_matrix() {
        let valid = state(vec![record(1, 2, &[])]).encode().expect("encode");
        let cases: [(&str, usize, u8); 6] = [
            ("magic", 0, b'X'),
            ("version", OFF_CATALOG_VERSION, 0xFF),
            ("header length", OFF_HEADER_LEN, 0x7F),
            ("publication", OFF_PUBLICATION, 0x7F),
            ("record count", OFF_RECORD_COUNT, 0xFF),
            ("records length", OFF_RECORDS_LEN, 0xFF),
        ];
        for (label, offset, value) in cases {
            let mut bytes = valid.clone();
            bytes[offset] = value;
            assert!(
                CatalogState::decode(&bytes).is_err(),
                "corrupt {label} must be rejected"
            );
        }
        let mut bytes = valid.clone();
        bytes[OFF_CHECKSUM] ^= 0xFF;
        assert!(CatalogState::decode(&bytes).is_err());
        let mut bytes = valid.clone();
        bytes[OFF_RESERVED] = 1;
        assert!(CatalogState::decode(&bytes).is_err());
        let mut bytes = valid.clone();
        bytes[OFF_STORAGE_GENERATION..OFF_STORAGE_GENERATION + 8]
            .copy_from_slice(&0_u64.to_le_bytes());
        assert!(CatalogState::decode(&bytes).is_err());
        assert!(CatalogState::decode(&valid[..10]).is_err());
        assert!(CatalogState::decode(&valid[..CATALOG_HEADER_SIZE + 1]).is_err());
        let mut bytes = valid.clone();
        bytes.push(0);
        assert!(CatalogState::decode(&bytes).is_err());
        // A record whose declared length disagrees with its content.
        let mut bytes = valid.clone();
        let record_len = CATALOG_HEADER_SIZE + 4;
        bytes[record_len - 4..record_len].copy_from_slice(&16_u32.to_le_bytes());
        assert!(CatalogState::decode(&bytes).is_err());
    }

    #[test]
    fn inconsistent_record_relationships_are_rejected() {
        let mut corrupted = record(1, 2, &[]);
        corrupted.current_generation = GenerationId::new(0);
        assert!(corrupted.validate().is_err());

        let mut corrupted = record(1, 2, &[]);
        corrupted.retained_generations = vec![GenerationId::new(2)];
        assert!(corrupted.validate().is_err());

        let mut corrupted = record(1, 2, &[3]);
        corrupted.references = vec![reference(1, 4, 1)];
        assert!(corrupted.validate().is_err());

        let mut corrupted = record(1, 2, &[]);
        corrupted.references = vec![reference(2, 2, 1)];
        assert!(corrupted.validate().is_err());

        let mut corrupted = record(1, 2, &[]);
        corrupted.publication = PublicationState::Staged;
        assert!(corrupted.validate().is_err());

        let updated = state(vec![record(1, 2, &[])])
            .upsert(record(1, 5, &[2]))
            .expect("upsert");
        assert_eq!(
            updated.generation_of(ObjectId::new(1)),
            Some(GenerationId::new(5))
        );
        assert!(updated.owns_generation(GenerationId::new(2)));
    }
    #[test]
    fn schema_validation_rejects_duplicate_columns() {
        let error = SchemaMetadata::new(
            SchemaId::new(1),
            CatalogVersion::new(1),
            vec![
                SchemaColumn {
                    column_id: ColumnId::new(4),
                    type_code: 1,
                },
                SchemaColumn {
                    column_id: ColumnId::new(4),
                    type_code: 2,
                },
            ],
        )
        .expect_err("duplicate column identifiers must be rejected");
        assert!(matches!(
            error.kind(),
            ErrorKind::InvalidArgument | ErrorKind::Corruption
        ));

        let columns = (0..(MAX_SCHEMA_COLUMNS as u64 + 1))
            .map(|index| SchemaColumn {
                column_id: ColumnId::new(index + 1),
                type_code: 1,
            })
            .collect::<Vec<_>>();
        assert!(SchemaMetadata::new(SchemaId::new(1), CatalogVersion::new(1), columns).is_err());
    }

    #[test]
    fn physical_reference_validation_matches_physical_rules() {
        assert!(structure(1).validate().is_ok());
        assert!(PhysicalStructure::new(
            SegmentId::new(0),
            PackId::new(1),
            BlockId::new(1),
            PageId::new(1),
            None,
        )
        .validate()
        .is_err());
        assert!(PhysicalStructure::new(
            SegmentId::new(1),
            PackId::new(1),
            BlockId::new(1),
            PageId::new(1),
            Some(RowId::new(0)),
        )
        .validate()
        .is_err());
    }

    #[test]
    fn release_and_retirement_are_explicit_state_changes() {
        let catalog = state(vec![record(1, 5, &[3, 4])]);
        let released = catalog
            .clone()
            .release_generation(GenerationId::new(4))
            .expect("release");
        assert_eq!(
            released
                .object(ObjectId::new(1))
                .expect("object")
                .retained_generations
                .len(),
            1
        );
        assert!(catalog
            .clone()
            .release_generation(GenerationId::new(5))
            .is_err());
        assert!(catalog.release_generation(GenerationId::new(8)).is_err());
        let retired = state(vec![record(1, 5, &[])])
            .retire_object(ObjectId::new(1))
            .expect("retire");
        assert!(!retired
            .object(ObjectId::new(1))
            .expect("object")
            .is_published());
        assert!(state(vec![record(1, 5, &[])])
            .remove_object(ObjectId::new(1))
            .expect("remove")
            .records()
            .is_empty());
    }

    #[test]
    fn catalog_file_names_are_strict() {
        assert_eq!(catalog_file_name(3), "catalog-00000000000000000003.cat");
        assert_eq!(
            generation_from_catalog_file_name(&catalog_file_name(3)),
            Some(3)
        );
        for name in [
            "catalog-00000000000000000003.cat.tmp",
            "catalog-3.cat",
            "catalog-00000000000000000000.cat",
            "catalog-00000000000000000003.ckpt",
            "notes.txt",
        ] {
            assert_eq!(generation_from_catalog_file_name(name), None, "{name}");
        }
        assert_eq!(&CATALOG_MAGIC, b"PLCT");
    }
}
