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
//! Immutable generation management: publication, retention, reader snapshots,
//! and garbage collection.
//!
//! A generation is one coherent, published storage state. Once a generation is
//! published its metadata is never modified: a change builds a new generation
//! independently, publishes it atomically, and leaves the previous generation
//! retained until it is provably unreachable.
//!
//! ```text
//! <root>/objects/tables/T-<table>/generations/GEN-<generation>/META.dat
//!                                                    immutable generation metadata
//! <root>/catalog/catalog-<generation:020>.cat          immutable catalog state
//! <root>/CURRENT                                     authoritative publication pointer
//! ```
//!
//! # Publication
//!
//! Publication follows the crate's single durability policy
//! (`BUILD -> FLUSH -> VERIFY -> SYNC -> PUBLISH -> RETAIN`):
//!
//! * **BUILD** encodes each new generation and the complete new catalog state in
//!   memory, from the currently published state rather than by mutating it. The
//!   published state stays readable throughout, and nothing is exposed to a
//!   reader until the final step.
//! * **FLUSH** writes every image into a `.tmp` staging file and pushes it
//!   through the file's buffered writer. A flush is not durability.
//! * **VERIFY** re-reads every staged image, decodes it, and re-establishes
//!   format validity, generation identity, catalog relationship, physical
//!   references, metadata lengths, checksums, internal consistency, and the
//!   required checkpoint relationships. A state that fails verification is never
//!   published.
//! * **SYNC** fsyncs the staged files. This is the durability boundary: nothing
//!   is reported durable before it succeeds.
//! * **PUBLISH** renames generation files into place, then the catalog file, and
//!   finally replaces the publication pointer with a staged, verified, synced
//!   image. Each rename is atomic and each directory is fsynced afterwards, so a
//!   crash leaves either the old complete state or the new complete state. An
//!   interrupted publication leaves `.tmp` artifacts that discovery ignores.
//! * **RETAIN** is durable in the new catalog state itself: the superseded
//!   generation of every object written by the publication is listed as retained,
//!   so the previous generation survives publication.
//!
//! # Generation identity and immutability
//!
//! One generation identifier names one immutable generation of one object: two
//! changes in one publication can never share an identifier, a new generation is
//! always strictly newer than the object's current generation, and a publication
//! refuses to write a generation file that already exists. Together these rules
//! make it structurally impossible for a publication to replace, patch, or
//! reorder a generation a reader may still be observing.
//!
//! A generation records the durable boundary of the publication that created it,
//! and that publication may be older than the state that currently publishes the
//! generation: a later publication of another object, or a publication that only
//! releases retention, leaves this generation untouched. Validation therefore
//! requires ordering, not equality - a generation may not claim a newer catalog
//! version, storage generation, or WAL boundary than the published state that
//! references it, and a retained generation must come from a strictly older
//! publication than the current one.
//!
//! # Ordering
//!
//! Generation metadata and catalog state are stored separately, so their ordering
//! is enforced explicitly: staged bytes are written before the physical sync, the
//! sync happens before any rename, generation files are renamed before the
//! catalog state that references them, and the catalog state is renamed before
//! the publication pointer that makes it current. A reader therefore never
//! observes a catalog state whose generations are not already durable.
//!
//! # Reader consistency
//!
//! A reader obtains a [`GenerationReader`] snapshot. The snapshot freezes the
//! published catalog state together with the generation metadata it references,
//! all of which are immutable files, and registers itself with the manager for
//! its lifetime. Acquiring a snapshot takes the manager lock and reads the
//! authoritative publication pointer, so a concurrent publication is observed as
//! either the complete previous state or the complete new state, never a mixture,
//! and no reader ever holds the storage engine lock while reading.
//!
//! # Synchronization
//!
//! One [`GenerationManager`] owns one storage root and serializes publication,
//! snapshot acquisition, and reclamation for that root through a single internal
//! lock. The lock guards only the in-memory mirror of the publication pointer and
//! the live reader registry: it is held while a publication replaces the pointer
//! and while reclamation proves reachability and removes files, so no publication
//! can change reachability between the proof and the removal. Readers load the
//! immutable files they need while acquiring a snapshot and then hold no lock at
//! all, so sustained reading never blocks a publication.
//!
//! Serializing durable writers across processes is the caller's responsibility,
//! exactly as it is for checkpoints and the write-ahead log: the manager itself
//! performs no cross-process locking.
//!
//! # Retention and garbage collection
//!
//! Retention is explicit and durable: the published catalog state lists the
//! generations each object still requires. Garbage collection computes the
//! reachable set from durable metadata plus the live reader registry, and
//! reclaims only generation files that are unreachable from every one of those
//! sources. A generation whose reachability cannot be proven is retained,
//! deletion is never the authoritative record of state, and an interrupted
//! reclamation can only leave unreferenced files behind, which recovery ignores.

use crate::catalog::{
    catalog_dir, catalog_file_name, catalog_path, discover_catalogs,
    generation_from_catalog_file_name, latest_valid_catalog, load_catalog,
    load_catalog_for_generation, CatalogState, ObjectRecord, PhysicalReference, PublicationState,
    SchemaMetadata, TMP_SUFFIX,
};
use crate::checkpoint::CheckpointRequest;
use crate::checksum::{crc_finalize, crc_init, crc_update};
use crate::codec::{checked_len, corruption, invalid, Cursor};
use crate::durable;
use crate::layout::DatabaseLayout;
use crate::manager::durable_segment_ids;
use plomid_core::{
    CatalogVersion, DatabaseId, ErrorKind, GenerationId, Lsn, ObjectId, PlomidError, Result,
    SchemaId,
};
use plomid_core::{TableId, TableIdentity};
/// Generation constants are defined once in `plomid_core::constants` and
/// re-exported here; module-local aliases keep the body unchanged.
pub use plomid_core::{
    GENERATION_CHECKSUMMED_PREFIX_LEN, GENERATION_FORMAT_VERSION, GENERATION_HEADER_SIZE,
    GENERATION_MAGIC, MAX_GENERATION_REFERENCES, MAX_PUBLICATION_OBJECTS,
    POINTER_CHECKSUMMED_PREFIX_LEN, PUBLICATION_POINTER_FILE_NAME,
    PUBLICATION_POINTER_FORMAT_VERSION, PUBLICATION_POINTER_MAGIC, PUBLICATION_POINTER_SIZE,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

pub use plomid_core::{
    GENERATION_OFF_CATALOG_VERSION as OFF_CATALOG_VERSION,
    GENERATION_OFF_CHECKPOINT_LSN as OFF_CHECKPOINT_LSN, GENERATION_OFF_CHECKSUM as OFF_CHECKSUM,
    GENERATION_OFF_GENERATION_ID as OFF_GENERATION_ID, GENERATION_OFF_HEADER_LEN as OFF_HEADER_LEN,
    GENERATION_OFF_OBJECT_ID as OFF_OBJECT_ID,
    GENERATION_OFF_PREVIOUS_GENERATION as OFF_PREVIOUS_GENERATION,
    GENERATION_OFF_PUBLICATION_GENERATION as OFF_PUBLICATION_GENERATION,
    GENERATION_OFF_REFERENCES_LEN as OFF_REFERENCES_LEN,
    GENERATION_OFF_REFERENCE_COUNT as OFF_REFERENCE_COUNT, GENERATION_OFF_RESERVED as OFF_RESERVED,
    GENERATION_OFF_STATE as OFF_STATE, GENERATION_OFF_STORAGE_GENERATION as OFF_STORAGE_GENERATION,
    GENERATION_OFF_VERSION as OFF_VERSION,
    POINTER_OFF_CATALOG_GENERATION as OFF_POINTER_CATALOG_GENERATION,
    POINTER_OFF_CATALOG_VERSION as OFF_POINTER_CATALOG_VERSION,
    POINTER_OFF_CHECKPOINT_LSN as OFF_POINTER_CHECKPOINT_LSN,
    POINTER_OFF_CHECKSUM as OFF_POINTER_CHECKSUM, POINTER_OFF_HEADER_LEN as OFF_POINTER_HEADER_LEN,
    POINTER_OFF_RESERVED as OFF_POINTER_RESERVED, POINTER_OFF_STATE as OFF_POINTER_STATE,
    POINTER_OFF_STORAGE_GENERATION as OFF_POINTER_STORAGE_GENERATION,
    POINTER_OFF_VERSION as OFF_POINTER_VERSION,
};
/// Durable metadata for one immutable generation.
///
/// A generation identifies the object it belongs to, the catalog version it was
/// published against, the publication that produced it, the durable storage
/// generation and WAL boundary it represents, its predecessor, its publication
/// state, and the physical structures it references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationMetadata {
    /// Identity of this generation.
    pub generation_id: GenerationId,
    /// Logical object the generation belongs to.
    pub object_id: ObjectId,
    /// Catalog version this generation was published against.
    pub catalog_version: CatalogVersion,
    /// Publication that produced the generation.
    pub publication_generation: GenerationId,
    /// Durable storage generation this generation represents.
    pub storage_generation: GenerationId,
    /// WAL boundary this generation represents.
    pub checkpoint_lsn: Lsn,
    /// Predecessor generation of the same object, when one exists.
    pub previous_generation: Option<GenerationId>,
    /// Publication state of the generation.
    pub state: PublicationState,
    /// Physical references of the generation, in deterministic order.
    pub references: Vec<PhysicalReference>,
}

impl GenerationMetadata {
    /// Creates generation metadata, normalizing reference ordering.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        generation_id: GenerationId,
        object_id: ObjectId,
        catalog_version: CatalogVersion,
        publication_generation: GenerationId,
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
        previous_generation: Option<GenerationId>,
        state: PublicationState,
        mut references: Vec<PhysicalReference>,
    ) -> Result<Self> {
        references.sort_unstable();
        references.dedup();
        let metadata = Self {
            generation_id,
            object_id,
            catalog_version,
            publication_generation,
            storage_generation,
            checkpoint_lsn,
            previous_generation,
            state,
            references,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Validates generation identity, relationships, and references.
    pub fn validate(&self) -> Result<()> {
        if self.generation_id.is_zero() || self.generation_id.get() == u64::MAX {
            return Err(corruption("generation has an impossible identity"));
        }
        if self.object_id.is_zero() {
            return Err(corruption("generation has a zero object ID"));
        }
        if self.catalog_version.get() == 0 || self.catalog_version.get() == u64::MAX {
            return Err(corruption("generation has an impossible catalog version"));
        }
        if self.publication_generation.is_zero() || self.publication_generation.get() == u64::MAX {
            return Err(corruption("generation has an impossible publication"));
        }
        if self.storage_generation.is_zero() || self.storage_generation.get() == u64::MAX {
            return Err(corruption(
                "generation has an impossible storage generation",
            ));
        }
        if self.checkpoint_lsn.get() == u64::MAX {
            return Err(corruption("generation has an invalid LSN"));
        }
        if !matches!(
            self.state,
            PublicationState::Staged | PublicationState::Published
        ) {
            return Err(corruption("generation has an invalid publication state"));
        }
        if let Some(previous) = self.previous_generation {
            if previous.is_zero() || previous == self.generation_id {
                return Err(corruption("generation has an impossible predecessor"));
            }
        }
        if self.references.len() > MAX_GENERATION_REFERENCES {
            return Err(corruption("generation has too many physical references"));
        }
        let mut previous: Option<&PhysicalReference> = None;
        for reference in &self.references {
            reference.validate()?;
            if reference.object_id != self.object_id {
                return Err(corruption("generation references a different object"));
            }
            if reference.generation_id != self.generation_id {
                return Err(corruption("generation references a different generation"));
            }
            if previous.is_some_and(|value| value >= reference) {
                return Err(corruption(
                    "generation physical references are not strictly ordered",
                ));
            }
            previous = Some(reference);
        }
        Ok(())
    }

    /// Returns true when the generation is marked published.
    #[must_use]
    pub fn is_published(&self) -> bool {
        self.state.is_published()
    }
}
impl GenerationMetadata {
    /// Encodes the generation deterministically as header followed by
    /// references.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut references = Vec::new();
        for reference in &self.references {
            reference.encode_into(&mut references);
        }
        let reference_count = u32::try_from(self.references.len())
            .map_err(|_| invalid("generation has too many physical references"))?;
        let references_len = u32::try_from(references.len())
            .map_err(|_| invalid("generation references are too large"))?;
        let total = GENERATION_HEADER_SIZE
            .checked_add(references.len())
            .ok_or_else(|| invalid("generation length overflow"))?;
        let mut bytes = vec![0_u8; total];
        bytes[0..4].copy_from_slice(&GENERATION_MAGIC);
        bytes[OFF_VERSION..OFF_VERSION + 4]
            .copy_from_slice(&GENERATION_FORMAT_VERSION.to_le_bytes());
        bytes[OFF_HEADER_LEN..OFF_HEADER_LEN + 4]
            .copy_from_slice(&(GENERATION_HEADER_SIZE as u32).to_le_bytes());
        bytes[OFF_GENERATION_ID..OFF_GENERATION_ID + 8]
            .copy_from_slice(&self.generation_id.get().to_le_bytes());
        bytes[OFF_OBJECT_ID..OFF_OBJECT_ID + 8]
            .copy_from_slice(&self.object_id.get().to_le_bytes());
        bytes[OFF_CATALOG_VERSION..OFF_CATALOG_VERSION + 8]
            .copy_from_slice(&self.catalog_version.get().to_le_bytes());
        bytes[OFF_PUBLICATION_GENERATION..OFF_PUBLICATION_GENERATION + 8]
            .copy_from_slice(&self.publication_generation.get().to_le_bytes());
        bytes[OFF_STORAGE_GENERATION..OFF_STORAGE_GENERATION + 8]
            .copy_from_slice(&self.storage_generation.get().to_le_bytes());
        bytes[OFF_CHECKPOINT_LSN..OFF_CHECKPOINT_LSN + 8]
            .copy_from_slice(&self.checkpoint_lsn.get().to_le_bytes());
        bytes[OFF_PREVIOUS_GENERATION..OFF_PREVIOUS_GENERATION + 8].copy_from_slice(
            &self
                .previous_generation
                .map_or(0, |previous| previous.get())
                .to_le_bytes(),
        );
        bytes[OFF_REFERENCE_COUNT..OFF_REFERENCE_COUNT + 4]
            .copy_from_slice(&reference_count.to_le_bytes());
        bytes[OFF_REFERENCES_LEN..OFF_REFERENCES_LEN + 4]
            .copy_from_slice(&references_len.to_le_bytes());
        bytes[OFF_STATE..OFF_STATE + 4].copy_from_slice(&self.state.as_u32().to_le_bytes());
        let checksum = checksum_of(&bytes[..GENERATION_CHECKSUMMED_PREFIX_LEN], &references);
        bytes[OFF_CHECKSUM..OFF_CHECKSUM + 4].copy_from_slice(&checksum.to_le_bytes());
        bytes[OFF_RESERVED..GENERATION_HEADER_SIZE].fill(0);
        bytes[GENERATION_HEADER_SIZE..].copy_from_slice(&references);
        Ok(bytes)
    }
}
impl GenerationMetadata {
    /// Decodes and fully validates a generation image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(bytes);
        if cursor.fixed::<4>("generation magic")? != GENERATION_MAGIC {
            return Err(corruption("invalid generation magic"));
        }
        let version = cursor.u32("generation format version")?;
        if version != GENERATION_FORMAT_VERSION {
            if version > GENERATION_FORMAT_VERSION {
                return Err(PlomidError::new(
                    ErrorKind::Unsupported,
                    "unsupported generation format version",
                ));
            }
            return Err(corruption("unsupported generation format version"));
        }
        let header_len = cursor.u32("generation header length")? as usize;
        if header_len != GENERATION_HEADER_SIZE {
            return Err(corruption("generation header length is invalid"));
        }
        let generation_id = GenerationId::new(cursor.u64("generation ID")?);
        let object_id = ObjectId::new(cursor.u64("generation object ID")?);
        let catalog_version = CatalogVersion::new(cursor.u64("generation catalog version")?);
        let publication_generation =
            GenerationId::new(cursor.u64("generation publication generation")?);
        let storage_generation = GenerationId::new(cursor.u64("generation storage generation")?);
        let checkpoint_lsn = Lsn::new(cursor.u64("generation checkpoint LSN")?);
        let previous = cursor.u64("generation predecessor")?;
        let reference_count = cursor.u32("generation reference count")? as usize;
        let references_len = cursor.u32("generation references length")? as usize;
        let state = PublicationState::from_u32(cursor.u32("generation publication state")?)?;
        let expected_checksum = cursor.u32("generation checksum")?;
        let reserved = cursor.fixed::<12>("generation reserved bytes")?;
        if reserved.iter().any(|byte| *byte != 0) {
            return Err(corruption("generation has non-zero reserved bytes"));
        }
        if reference_count > MAX_GENERATION_REFERENCES {
            return Err(corruption("generation has too many physical references"));
        }
        let expected_len = GENERATION_HEADER_SIZE
            .checked_add(references_len)
            .ok_or_else(|| corruption("generation length overflow"))?;
        if bytes.len() != expected_len {
            return Err(corruption("generation has unexpected trailing data"));
        }
        let needed = checked_len(reference_count, crate::catalog::PHYSICAL_REFERENCE_LEN)
            .ok_or_else(|| corruption("generation reference length overflow"))?;
        if needed != references_len {
            return Err(corruption("generation reference count is invalid"));
        }
        let actual_checksum = checksum_of(
            &bytes[..GENERATION_CHECKSUMMED_PREFIX_LEN],
            &bytes[GENERATION_HEADER_SIZE..expected_len],
        );
        if actual_checksum != expected_checksum {
            return Err(corruption(format!(
                "generation checksum mismatch (expected {expected_checksum:#010x}, got {actual_checksum:#010x})"
            )));
        }
        let mut references = Vec::with_capacity(reference_count.min(1024));
        for _ in 0..reference_count {
            references.push(PhysicalReference::decode(&mut cursor)?);
        }
        cursor.ensure_end("generation references")?;
        let metadata = Self {
            generation_id,
            object_id,
            catalog_version,
            publication_generation,
            storage_generation,
            checkpoint_lsn,
            previous_generation: if previous == 0 {
                None
            } else {
                Some(GenerationId::new(previous))
            },
            state,
            references,
        };
        metadata.validate()?;
        Ok(metadata)
    }
}

/// Computes a metadata checksum over a header prefix and a payload.
///
/// The checksum field is never part of its own input, and the algorithm is the
/// canonical CRC32C component shared with pages, blocks, packs, WAL records,
/// checkpoints, and catalog states.
fn checksum_of(prefix: &[u8], payload: &[u8]) -> u32 {
    let mut crc = crc_init();
    crc = crc_update(crc, prefix);
    crc = crc_update(crc, payload);
    crc_finalize(crc)
}
mod paths;
pub use paths::{
    discover_generation_ids, discover_generations, generation_path, generation_path_in_schema,
    generation_staged_path, generation_staged_path_in_schema, load_generation,
};

/// Returns the path of the authoritative publication pointer.
#[must_use]
pub fn publication_pointer_path(root: &Path) -> PathBuf {
    DatabaseLayout::new(root).current_path()
}
/// Authoritative record of the currently published catalog state.
///
/// The pointer is the single durable authority for which catalog state is
/// current. Readers and recovery never infer the current generation from
/// filesystem enumeration; they read this record, validate it, and follow it to
/// the immutable catalog state it names.
///
/// # Format
///
/// ```text
/// magic[4] = PLPT       | format_version[u32] = 1        | header_len[u32] = 72
/// catalog_generation[u64] | catalog_version[u64]         | storage_generation[u64]
/// checkpoint_lsn[u64]   | state[u32] = 1                | checksum[u32]
/// reserved[20] = 0
/// ```
///
/// The checksum is CRC32C over `pointer[0..48]`; the checksum field itself and
/// the reserved bytes are not covered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationPointer {
    /// Generation the published catalog state was published as.
    pub catalog_generation: GenerationId,
    /// Version of the published catalog state.
    pub catalog_version: CatalogVersion,
    /// Durable storage generation the published state describes.
    pub storage_generation: GenerationId,
    /// WAL boundary the published state describes.
    pub checkpoint_lsn: Lsn,
    /// Publication state the pointer names; always
    /// [`PublicationState::Published`].
    pub state: PublicationState,
}

impl PublicationPointer {
    /// Creates a publication pointer.
    pub fn new(
        catalog_generation: GenerationId,
        catalog_version: CatalogVersion,
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
    ) -> Result<Self> {
        let pointer = Self {
            catalog_generation,
            catalog_version,
            storage_generation,
            checkpoint_lsn,
            state: PublicationState::Published,
        };
        pointer.validate()?;
        Ok(pointer)
    }

    /// Creates the pointer that names a published catalog state.
    pub fn from_catalog(state: &CatalogState) -> Result<Self> {
        let pointer = Self {
            catalog_generation: state.catalog_generation(),
            catalog_version: state.catalog_version(),
            storage_generation: state.storage_generation(),
            checkpoint_lsn: state.checkpoint_lsn(),
            state: PublicationState::Published,
        };
        pointer.validate()?;
        Ok(pointer)
    }

    /// Returns the publication state the pointer names.
    #[must_use]
    pub const fn publication_state(&self) -> PublicationState {
        self.state
    }

    /// Encodes the pointer deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = vec![0_u8; PUBLICATION_POINTER_SIZE];
        bytes[0..4].copy_from_slice(&PUBLICATION_POINTER_MAGIC);
        bytes[OFF_POINTER_VERSION..OFF_POINTER_VERSION + 4]
            .copy_from_slice(&PUBLICATION_POINTER_FORMAT_VERSION.to_le_bytes());
        bytes[OFF_POINTER_HEADER_LEN..OFF_POINTER_HEADER_LEN + 4]
            .copy_from_slice(&(PUBLICATION_POINTER_SIZE as u32).to_le_bytes());
        bytes[OFF_POINTER_CATALOG_GENERATION..OFF_POINTER_CATALOG_GENERATION + 8]
            .copy_from_slice(&self.catalog_generation.get().to_le_bytes());
        bytes[OFF_POINTER_CATALOG_VERSION..OFF_POINTER_CATALOG_VERSION + 8]
            .copy_from_slice(&self.catalog_version.get().to_le_bytes());
        bytes[OFF_POINTER_STORAGE_GENERATION..OFF_POINTER_STORAGE_GENERATION + 8]
            .copy_from_slice(&self.storage_generation.get().to_le_bytes());
        bytes[OFF_POINTER_CHECKPOINT_LSN..OFF_POINTER_CHECKPOINT_LSN + 8]
            .copy_from_slice(&self.checkpoint_lsn.get().to_le_bytes());
        bytes[OFF_POINTER_STATE..OFF_POINTER_STATE + 4]
            .copy_from_slice(&self.state.as_u32().to_le_bytes());
        let checksum = checksum_of(&bytes[..POINTER_CHECKSUMMED_PREFIX_LEN], &[]);
        bytes[OFF_POINTER_CHECKSUM..OFF_POINTER_CHECKSUM + 4]
            .copy_from_slice(&checksum.to_le_bytes());
        bytes[OFF_POINTER_RESERVED..PUBLICATION_POINTER_SIZE].fill(0);
        Ok(bytes)
    }
}
impl PublicationPointer {
    /// Decodes and fully validates a publication pointer image.
    ///
    /// Framing, checksum, reserved bytes, and every field are validated before
    /// the pointer is trusted, so a truncated or corrupt record is rejected
    /// with a structured error instead of being followed.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(bytes);
        if cursor.fixed::<4>("publication pointer magic")? != PUBLICATION_POINTER_MAGIC {
            return Err(corruption("invalid publication pointer magic"));
        }
        let version = cursor.u32("publication pointer format version")?;
        if version != PUBLICATION_POINTER_FORMAT_VERSION {
            if version > PUBLICATION_POINTER_FORMAT_VERSION {
                return Err(PlomidError::new(
                    ErrorKind::Unsupported,
                    "unsupported publication pointer format version",
                ));
            }
            return Err(corruption("unsupported publication pointer format version"));
        }
        let header_len = cursor.u32("publication pointer header length")? as usize;
        if header_len != PUBLICATION_POINTER_SIZE {
            return Err(corruption("publication pointer header length is invalid"));
        }
        let catalog_generation =
            GenerationId::new(cursor.u64("publication pointer catalog generation")?);
        let catalog_version =
            CatalogVersion::new(cursor.u64("publication pointer catalog version")?);
        let storage_generation =
            GenerationId::new(cursor.u64("publication pointer storage generation")?);
        let checkpoint_lsn = Lsn::new(cursor.u64("publication pointer checkpoint LSN")?);
        let state = PublicationState::from_u32(cursor.u32("publication pointer state")?)?;
        let expected_checksum = cursor.u32("publication pointer checksum")?;
        let reserved = cursor.fixed::<20>("publication pointer reserved bytes")?;
        if reserved.iter().any(|byte| *byte != 0) {
            return Err(corruption(
                "publication pointer has non-zero reserved bytes",
            ));
        }
        cursor.ensure_end("publication pointer")?;
        let actual_checksum = checksum_of(&bytes[..POINTER_CHECKSUMMED_PREFIX_LEN], &[]);
        if actual_checksum != expected_checksum {
            return Err(corruption(format!(
                "publication pointer checksum mismatch (expected {expected_checksum:#010x}, got {actual_checksum:#010x})"
            )));
        }
        let pointer = Self {
            catalog_generation,
            catalog_version,
            storage_generation,
            checkpoint_lsn,
            state,
        };
        pointer.validate()?;
        Ok(pointer)
    }
}
impl PublicationPointer {
    /// Validates the pointer's boundary fields.
    pub fn validate(&self) -> Result<()> {
        if self.catalog_generation.is_zero() || self.catalog_generation.get() == u64::MAX {
            return Err(corruption(
                "publication pointer has an impossible generation",
            ));
        }
        if self.catalog_version.get() == 0 || self.catalog_version.get() == u64::MAX {
            return Err(corruption(
                "publication pointer has an impossible catalog version",
            ));
        }
        if self.storage_generation.is_zero() || self.storage_generation.get() == u64::MAX {
            return Err(corruption(
                "publication pointer has an impossible storage generation",
            ));
        }
        if self.checkpoint_lsn.get() == u64::MAX {
            return Err(corruption("publication pointer has an invalid LSN"));
        }
        if self.state != PublicationState::Published {
            return Err(corruption(
                "publication pointer does not name a published state",
            ));
        }
        Ok(())
    }
}
/// Returns the staging path used while publishing the pointer.
#[must_use]
pub fn publication_pointer_staged_path(root: &Path) -> PathBuf {
    DatabaseLayout::new(root).current_staged_path()
}

/// Reads and validates the authoritative publication pointer.
///
/// The pointer names the currently published catalog state. Absence is reported
/// as [`ErrorKind::NotFound`] rather than as an empty pointer, so a caller can
/// distinguish "nothing has been published yet" from "a state was published".
pub fn load_publication_pointer(root: &Path) -> Result<PublicationPointer> {
    let path = publication_pointer_path(root);
    let bytes = fs::read(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                "publication pointer is missing",
                format!("path={}", path.display()),
            )
        } else {
            PlomidError::from(error)
        }
    })?;
    PublicationPointer::decode(&bytes)
}

/// One object written by a publication.
///
/// A change names the logical object, the schema state it is published
/// against, the immutable generation it is written as, and the physical
/// structures that generation references. The `table_identity` binds the
/// change to its owning `Database → Schema → Table` location so the
/// generation metadata resolves inside the SQL-facing logical tree; the
/// flat `objects/tables/` storage path is derived from it at build time.
/// Retention, the predecessor relationship, and the durable publication
/// boundary are derived by the publication itself, so a caller can never
/// assemble a catalog entry that disagrees with the generation metadata
/// published alongside it.
///
/// The identity defaults to the flat mapping (`object_id` as table, default
/// database/schema) so pre-hierarchy callers (benches, legacy tests) keep
/// compiling; SQL-driven writers pass the real hierarchy identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectChange {
    /// Logical object identity.
    pub object_id: ObjectId,
    /// Owning database → schema → table location of the object.
    pub table_identity: TableIdentity,
    /// Schema state the object is published against.
    pub schema: SchemaMetadata,
    /// Immutable generation the object is written as.
    pub generation_id: GenerationId,
    /// Physical references the new generation owns.
    pub references: Vec<PhysicalReference>,
}

impl ObjectChange {
    /// Creates a change, normalizing reference ordering.
    ///
    /// Derives the owning `TableIdentity` from compatibility rules: the
    /// table component is `object_id` (the historical flat mapping), and
    /// the database/schema components come from `schema.schema_id` through
    /// the layout convention. Dedicated hierarchy writers should prefer
    /// [`Self::with_identity`] to pass the explicit identity.
    pub fn new(
        object_id: ObjectId,
        schema: SchemaMetadata,
        generation_id: GenerationId,
        mut references: Vec<PhysicalReference>,
    ) -> Result<Self> {
        references.sort_unstable();
        references.dedup();
        let table_identity = TableIdentity::from_object_and_schema(object_id, schema.schema_id);
        let change = Self {
            object_id,
            table_identity,
            schema,
            generation_id,
            references,
        };
        change.validate()?;
        Ok(change)
    }

    /// Creates a change with an explicit owning `TableIdentity`.
    ///
    /// Used by SQL-driven writers that know the real
    /// `Database → Schema → Table` location of the object.
    pub fn with_identity(
        object_id: ObjectId,
        table_identity: TableIdentity,
        schema: SchemaMetadata,
        generation_id: GenerationId,
        mut references: Vec<PhysicalReference>,
    ) -> Result<Self> {
        references.sort_unstable();
        references.dedup();
        let change = Self {
            object_id,
            table_identity,
            schema,
            generation_id,
            references,
        };
        change.validate()?;
        Ok(change)
    }

    /// Validates the change's identity, schema, and references.
    pub fn validate(&self) -> Result<()> {
        if self.object_id.is_zero() {
            return Err(invalid("object change has a zero object ID"));
        }
        if self.table_identity.is_zero() {
            return Err(invalid("object change has a zero table identity"));
        }
        if self.table_identity.table_id.get() != self.object_id.get() {
            return Err(invalid(
                "object change table identity disagrees with its object ID",
            ));
        }
        if self.generation_id.is_zero() || self.generation_id.get() == u64::MAX {
            return Err(invalid("object change has an impossible generation"));
        }
        self.schema.validate()?;
        if self.references.len() > MAX_GENERATION_REFERENCES {
            return Err(invalid("object change has too many physical references"));
        }
        let mut previous: Option<&PhysicalReference> = None;
        for reference in &self.references {
            reference.validate()?;
            if reference.object_id != self.object_id {
                return Err(invalid("object change references a different object"));
            }
            if reference.generation_id != self.generation_id {
                return Err(invalid("object change references a different generation"));
            }
            if previous.is_some_and(|value| value >= reference) {
                return Err(invalid("object change references are not strictly ordered"));
            }
            previous = Some(reference);
        }
        Ok(())
    }
}
/// A complete publication request.
///
/// The durable boundary of the new state is supplied by the caller because only
/// the caller knows which storage flush and which WAL LSN the new state
/// represents. Both are checked against the currently published state, so a
/// request can never move durable state backwards.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationRequest {
    /// Objects written by this publication, in ascending object-id order.
    pub changes: Vec<ObjectChange>,
    /// Generations whose retention this publication releases, ascending.
    pub releases: Vec<GenerationId>,
    /// Objects that no longer exist in the logical catalog and whose
    /// generations remain retained only until the normal GC pass can reclaim
    /// them.
    pub retired_objects: Vec<ObjectId>,
    /// Objects removed from the published generation catalog after a logical
    /// drop. Their files remain reclaimable by the normal GC pass.
    pub removed_objects: Vec<ObjectId>,
    /// Durable storage generation the new state represents.
    pub storage_generation: GenerationId,
    /// WAL boundary the new state represents.
    pub checkpoint_lsn: Lsn,
}

impl PublicationRequest {
    /// Requests a publication that writes `changes` and retains the superseded
    /// generation of every object it writes.
    pub fn write(
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
        changes: Vec<ObjectChange>,
    ) -> Result<Self> {
        let request = Self {
            changes,
            releases: Vec::new(),
            retired_objects: Vec::new(),
            removed_objects: Vec::new(),
            storage_generation,
            checkpoint_lsn,
        };
        request.validate()?;
        Ok(request)
    }

    /// Requests a publication that releases retention for `generations`.
    ///
    /// Releasing retention writes no generation: it publishes a new catalog
    /// state in which the named generations are no longer referenced. Once such
    /// a generation is unreachable from every durable and active reference,
    /// garbage collection may reclaim it.
    pub fn release(
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
        generations: Vec<GenerationId>,
    ) -> Result<Self> {
        let mut release = generations;
        release.sort_unstable();
        release.dedup();
        let request = Self {
            changes: Vec::new(),
            releases: release,
            retired_objects: Vec::new(),
            removed_objects: Vec::new(),
            storage_generation,
            checkpoint_lsn,
        };
        request.validate()?;
        Ok(request)
    }

    /// Returns a request that also releases retention for `generations`.
    pub fn with_releases(mut self, generations: Vec<GenerationId>) -> Result<Self> {
        let mut release = generations;
        release.sort_unstable();
        release.dedup();
        self.releases = release;
        self.validate()?;
        Ok(self)
    }

    /// Returns a request that marks logical objects as dropped while retaining
    /// their immutable generations until the normal reclamation pass.
    pub fn with_retired_objects(mut self, objects: Vec<ObjectId>) -> Result<Self> {
        let mut objects = objects;
        objects.sort_unstable();
        objects.dedup();
        self.retired_objects = objects;
        self.validate()?;
        Ok(self)
    }

    pub fn with_removed_objects(mut self, objects: Vec<ObjectId>) -> Result<Self> {
        let mut objects = objects;
        objects.sort_unstable();
        objects.dedup();
        self.removed_objects = objects;
        self.validate()?;
        Ok(self)
    }

    /// Validates the request independently of any durable state.
    pub fn validate(&self) -> Result<()> {
        if self.storage_generation.is_zero() || self.storage_generation.get() == u64::MAX {
            return Err(invalid("publication has an impossible storage generation"));
        }
        if self.checkpoint_lsn.get() == u64::MAX {
            return Err(invalid("publication has an invalid LSN"));
        }
        if self.changes.len() > MAX_PUBLICATION_OBJECTS {
            return Err(invalid("publication writes too many objects"));
        }
        let mut previous: Option<ObjectId> = None;
        let mut generations: BTreeSet<GenerationId> = BTreeSet::new();
        for change in &self.changes {
            change.validate()?;
            if previous.is_some_and(|object_id| object_id >= change.object_id) {
                return Err(invalid(
                    "publication object changes are not strictly ordered",
                ));
            }
            // One generation identifier names one immutable generation of one
            // object, so two changes can never share it.
            if !generations.insert(change.generation_id) {
                return Err(invalid("publication repeats a generation ID"));
            }
            previous = Some(change.object_id);
        }
        let mut previous: Option<GenerationId> = None;
        for generation in &self.releases {
            if generation.is_zero() {
                return Err(invalid("publication releases a zero generation"));
            }
            if previous.is_some_and(|value| value >= *generation) {
                return Err(invalid("publication releases are not strictly ordered"));
            }
            previous = Some(*generation);
        }
        let mut previous: Option<ObjectId> = None;
        for object in &self.retired_objects {
            if object.is_zero() {
                return Err(invalid("publication retires a zero object"));
            }
            if previous.is_some_and(|value| value >= *object) {
                return Err(invalid(
                    "publication retired objects are not strictly ordered",
                ));
            }
            previous = Some(*object);
        }
        let mut previous: Option<ObjectId> = None;
        for object in &self.removed_objects {
            if object.is_zero() {
                return Err(invalid("publication removes a zero object"));
            }
            if previous.is_some_and(|value| value >= *object) {
                return Err(invalid(
                    "publication removed objects are not strictly ordered",
                ));
            }
            previous = Some(*object);
        }
        Ok(())
    }
}

/// Injection point for publication-boundary failure tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationFailPoint {
    /// No failure; normal production path.
    None,
    /// Fail before the new state is built.
    BeforeBuild,
    /// Fail after the new state is built but before it is flushed.
    AfterBuild,
    /// Fail after flush but before verification.
    AfterFlush,
    /// Fail during verification.
    DuringVerify,
    /// Fail after verification but before synchronization.
    BeforeSync,
    /// Fail after synchronization but before publication.
    AfterSync,
    /// Fail during atomic publication.
    DuringPublish,
    /// Fail immediately after the publication pointer is replaced.
    AfterPublish,
}

/// Injection point for reclamation-boundary safety tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GcFailPoint {
    /// No failure; normal production path.
    None,
    /// Fail after reachability is proven but before any file is reclaimed.
    BeforeReclaim,
    /// Fail after the first unreachable generation is reclaimed.
    DuringReclaim,
    /// Fail after every unreachable file is reclaimed.
    AfterReclaim,
}

/// Phase-boundary timings of one publication.
///
/// The values are observability data only: they never affect the durability
/// guarantees of a publication and are never persisted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PublicationPhaseTimings {
    /// BUILD: constructing the complete new catalog and generation state.
    pub build: std::time::Duration,
    /// FLUSH: pushing staged bytes through the buffered I/O layer.
    pub flush: std::time::Duration,
    /// VERIFY: re-reading and cross-validating every staged image.
    pub verify: std::time::Duration,
    /// SYNC: fsync of every staged image.
    pub sync: std::time::Duration,
    /// PUBLISH: atomic renames plus directory durability.
    pub publish: std::time::Duration,
}

/// Result of a completed publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedGeneration {
    /// Pointer that is now authoritative.
    pub pointer: PublicationPointer,
    /// Catalog state that is now published.
    pub catalog: CatalogState,
    /// Generation identities written by this publication, ascending.
    pub generations: Vec<GenerationId>,
    /// Superseded catalog generation, retained for recovery.
    pub superseded_catalog: Option<GenerationId>,
}

/// Result of one garbage-collection pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcOutcome {
    /// Generations proven reachable and therefore retained, ascending.
    pub retained: Vec<GenerationId>,
    /// Generation files reclaimed by this pass, ascending.
    pub reclaimed: Vec<GenerationId>,
    /// Catalog states reclaimed by this pass, ascending.
    pub reclaimed_catalogs: Vec<GenerationId>,
}

/// Source of the publication state selected by recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationPointerSource {
    /// The durable publication pointer named the recovered state.
    Pointer,
    /// The newest valid checkpoint named the recovered state.
    Checkpoint,
    /// Deterministic discovery of published catalog states selected the
    /// recovered state because the pointer was unusable.
    Catalog,
}

/// Result of startup recovery.
///
/// Recovery is read-only: it reads and validates durable metadata and never
/// writes, mutates, or publishes anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryOutcome {
    /// Publication pointer that describes the recovered state.
    pub pointer: PublicationPointer,
    /// Recovered published catalog state.
    pub catalog: CatalogState,
    /// Generations the recovered state references, ascending.
    pub generations: Vec<GenerationId>,
    /// Durable metadata the recovered state was selected from.
    pub source: PublicationPointerSource,
}

/// The generations and catalog state one live reader still requires.
#[derive(Clone, Debug)]
struct ReaderLease {
    catalog_generation: GenerationId,
    generations: BTreeSet<GenerationId>,
}

/// Mutable metadata of a generation manager, guarded by one small lock.
///
/// The lock guards the in-memory mirror of the authoritative publication
/// pointer and the live reader registry. No file is read or written while a
/// reader is using a snapshot, so reading never holds this lock.
struct ManagerState {
    pointer: Option<PublicationPointer>,
    next_reader: u64,
    readers: BTreeMap<u64, ReaderLease>,
}

/// Durable versioned catalog and immutable generation management.
///
/// A manager owns one storage root. It publishes new catalog and generation
/// states atomically, hands out reader snapshots that observe complete
/// generations only, tracks reader lifetimes explicitly, and reclaims only
/// generations proven unreachable from every durable and active reference.
///
/// ```text
/// publish:  BUILD -> FLUSH -> VERIFY -> SYNC -> PUBLISH atomically -> RETAIN
/// reader:   acquire snapshot -> read immutable files -> release snapshot
/// reclaim:  prove unreachable from current state, checkpoints, and readers
/// ```
pub struct GenerationManager {
    root: PathBuf,
    inner: Mutex<ManagerState>,
}

impl GenerationManager {
    /// Opens the durable metadata of a storage root.
    ///
    /// The published state is recovered and validated before the manager is
    /// usable. A root that has never been published reports absence rather than
    /// a failure, so a new store starts by publishing its initial state.
    pub fn open(root: &Path) -> Result<Self> {
        let recovered = Self::recover_if_present(root)?;
        durable::ensure_dir(&DatabaseLayout::new(root).tables_dir())?;
        durable::ensure_dir(&catalog_dir(root))?;
        Ok(Self {
            root: root.to_path_buf(),
            inner: Mutex::new(ManagerState {
                pointer: recovered.map(|outcome| outcome.pointer),
                next_reader: 0,
                readers: BTreeMap::new(),
            }),
        })
    }

    /// Recovers the published state of a storage root without modifying it.
    pub fn recover(root: &Path) -> Result<RecoveryOutcome> {
        Self::recover_if_present(root)?.ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                "no durable published catalog state",
                format!("root={}", root.display()),
            )
        })
    }

    /// Recovers the published state of a storage root, if one exists.
    ///
    /// The authoritative publication pointer is used when it is readable and
    /// valid. When the pointer is unusable, the newest safe durable state is
    /// selected deterministically from the checkpoint chain and from published
    /// catalog states, which is the existing recovery contract for metadata
    /// that cannot be trusted. A pointer that is itself valid but names damaged
    /// state fails instead of being replaced, because substituting a different
    /// state would silently change what is recovered.
    pub fn recover_if_present(root: &Path) -> Result<Option<RecoveryOutcome>> {
        // An incompatible legacy root is refused before any state is inferred;
        // there is exactly one legacy detector, shared with layout validation.
        crate::layout::reject_legacy_layout(&DatabaseLayout::new(root))?;
        if let Ok(pointer) = load_publication_pointer(root) {
            return Ok(Some(resolve_publication(
                root,
                pointer,
                PublicationPointerSource::Pointer,
            )?));
        }
        let by_checkpoint = match crate::checkpoint::latest_valid(root)? {
            Some((_, checkpoint)) => {
                match published_catalog_for_version(root, checkpoint.catalog_generation)? {
                    Some(state) => Some((state, PublicationPointerSource::Checkpoint)),
                    None => return Err(corruption(
                        "the newest valid checkpoint names a catalog version that does not exist",
                    )),
                }
            }
            None => None,
        };
        let by_catalog = match latest_valid_catalog(root)? {
            Some((_, state)) => Some((state, PublicationPointerSource::Catalog)),
            None => None,
        };
        let selected = match (by_checkpoint, by_catalog) {
            (Some((checkpoint, checkpoint_source)), Some((catalog, catalog_source))) => {
                if checkpoint.catalog_generation() >= catalog.catalog_generation() {
                    Some((checkpoint, checkpoint_source))
                } else {
                    Some((catalog, catalog_source))
                }
            }
            (Some(entry), None) => Some(entry),
            (None, Some(entry)) => Some(entry),
            (None, None) => None,
        };
        match selected {
            Some((state, source)) => Ok(Some(resolve_state(root, state, source)?)),
            None => Ok(None),
        }
    }
    /// Returns the storage root this manager owns.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the authoritative publication pointer.
    pub fn pointer(&self) -> Result<PublicationPointer> {
        self.lock().pointer.ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                "no published catalog state",
                format!("root={}", self.root.display()),
            )
        })
    }

    /// Loads the currently published catalog state.
    pub fn load(&self) -> Result<CatalogState> {
        let pointer = self.pointer()?;
        load_catalog_for_generation(&self.root, pointer.catalog_generation)?
            .ok_or_else(|| missing_catalog(pointer.catalog_generation))
    }

    /// Returns the generations the currently published catalog state references.
    pub fn published_generations(&self) -> Result<Vec<GenerationId>> {
        let mut generations = self.load()?.generation_ids();
        generations.sort_unstable();
        generations.dedup();
        Ok(generations)
    }

    /// Returns true when the published catalog retains `generation`.
    pub fn retains(&self, generation: GenerationId) -> Result<bool> {
        Ok(self.load()?.owns_generation(generation))
    }

    /// Publishes a catalog state in which `generation` is no longer retained.
    ///
    /// Releasing retention removes the durable reference that keeps a superseded
    /// generation alive. It deletes nothing: once the generation is unreachable
    /// from every durable and active reference, garbage collection may reclaim
    /// it.
    pub fn release(self: &Arc<Self>, generation: GenerationId) -> Result<PublishedGeneration> {
        let pointer = self.pointer()?;
        self.publish(PublicationRequest::release(
            pointer.storage_generation,
            pointer.checkpoint_lsn,
            vec![generation],
        )?)
    }

    /// Marks an object as dropped while retaining its generations for GC.
    pub fn retire_object(self: &Arc<Self>, object: ObjectId) -> Result<PublishedGeneration> {
        let pointer = self.pointer()?;
        let request = PublicationRequest::release(
            pointer.storage_generation,
            pointer.checkpoint_lsn,
            Vec::new(),
        )?
        .with_retired_objects(vec![object])?;
        self.publish(request)
    }

    pub fn remove_object(self: &Arc<Self>, object: ObjectId) -> Result<PublishedGeneration> {
        let pointer = self.pointer()?;
        let request = PublicationRequest::release(
            pointer.storage_generation,
            pointer.checkpoint_lsn,
            Vec::new(),
        )?
        .with_removed_objects(vec![object])?;
        self.publish(request)
    }

    /// Returns the checkpoint request that captures the current published state.
    ///
    /// Publishing catalog and generation metadata and then creating a checkpoint
    /// from this request pins the catalog version the recovery chain resolves,
    /// without duplicating checkpoint persistence or its publication guarantees.
    pub fn checkpoint_request(&self) -> Result<CheckpointRequest> {
        let pointer = self.pointer()?;
        Ok(CheckpointRequest {
            checkpoint_lsn: pointer.checkpoint_lsn,
            storage_generation: pointer.storage_generation,
            catalog_generation: pointer.catalog_version,
            metadata: Vec::new(),
        })
    }

    /// Validates the published state, including catalog and generation
    /// relationships. Reads nothing else and changes nothing.
    pub fn validate(&self) -> Result<()> {
        let pointer = self.pointer()?;
        let state = load_catalog_for_generation(&self.root, pointer.catalog_generation)?
            .ok_or_else(|| missing_catalog(pointer.catalog_generation))?;
        validate_publication_state(&self.root, &pointer, &state)?;
        Ok(())
    }

    /// Validates that every physical reference of the published state names a
    /// durable segment of the storage image.
    ///
    /// This resolves catalog references against the materialized storage
    /// hierarchy, which requires a storage image to exist. It is separate from
    /// [`GenerationManager::validate`], which validates metadata relationships
    /// and is independent of the physical image.
    pub fn validate_physical(&self) -> Result<()> {
        validate_storage_references(&self.root, &self.load()?)
    }

    fn lock(&self) -> MutexGuard<'_, ManagerState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn release_reader(&self, lease: u64) {
        self.lock().readers.remove(&lease);
    }
}
/// A stable view of one complete published generation.
///
/// A snapshot freezes the published catalog state together with the generation
/// metadata it references. Every one of those files is immutable, so a
/// concurrent publication cannot change what the snapshot observes: the reader
/// keeps seeing the complete state it began reading while new readers observe
/// the newer complete state. A reader can therefore never observe a mixture of
/// two generations.
///
/// The snapshot registers itself for its lifetime, which is what keeps the
/// generations it observes out of reach of garbage collection. Releasing the
/// snapshot is explicit: dropping it deregisters the reader.
pub struct GenerationReader {
    manager: Arc<GenerationManager>,
    lease: u64,
    pointer: PublicationPointer,
    catalog: CatalogState,
    generations: BTreeMap<GenerationId, GenerationMetadata>,
}

impl GenerationManager {
    /// Acquires a snapshot of the currently published generation.
    ///
    /// Acquisition takes the manager lock, which is held only while the
    /// authoritative pointer is read and the immutable catalog and generation
    /// files it names are loaded. The snapshot registers itself before the lock
    /// is released, so no publication can reclaim what the snapshot observes
    /// between selection and registration. Reading after acquisition holds no
    /// lock and never blocks a publication.
    pub fn reader(self: &Arc<Self>) -> Result<GenerationReader> {
        let mut inner = self.lock();
        let pointer = inner.pointer.ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                "no published catalog state",
                format!("root={}", self.root.display()),
            )
        })?;
        let catalog = load_catalog_for_generation(&self.root, pointer.catalog_generation)?
            .ok_or_else(|| missing_catalog(pointer.catalog_generation))?;
        let mut generations = BTreeMap::new();
        for record in catalog.records() {
            for generation in record.generation_ids() {
                if let std::collections::btree_map::Entry::Vacant(slot) =
                    generations.entry(generation)
                {
                    slot.insert(load_generation(&self.root, generation)?);
                }
            }
        }
        let lease = inner.next_reader;
        inner.next_reader = inner
            .next_reader
            .checked_add(1)
            .ok_or_else(|| invalid("reader registry overflow"))?;
        let held = generations.keys().copied().collect();
        inner.readers.insert(
            lease,
            ReaderLease {
                catalog_generation: pointer.catalog_generation,
                generations: held,
            },
        );
        Ok(GenerationReader {
            manager: Arc::clone(self),
            lease,
            pointer,
            catalog,
            generations,
        })
    }
}

impl GenerationReader {
    /// Returns the publication pointer the snapshot was acquired from.
    #[must_use]
    pub fn pointer(&self) -> PublicationPointer {
        self.pointer
    }

    /// Returns the complete catalog state the snapshot observes.
    #[must_use]
    pub fn catalog(&self) -> &CatalogState {
        &self.catalog
    }

    /// Returns the object records of the observed state, in ascending order.
    #[must_use]
    pub fn objects(&self) -> &[ObjectRecord] {
        self.catalog.records()
    }

    /// Returns one object record of the observed state.
    #[must_use]
    pub fn object(&self, object_id: ObjectId) -> Option<&ObjectRecord> {
        self.catalog.object(object_id)
    }

    /// Returns the generation metadata of the observed state.
    #[must_use]
    pub fn generations(&self) -> &BTreeMap<GenerationId, GenerationMetadata> {
        &self.generations
    }

    /// Returns one generation of the observed state.
    #[must_use]
    pub fn generation(&self, generation: GenerationId) -> Option<&GenerationMetadata> {
        self.generations.get(&generation)
    }

    /// Returns every generation the snapshot observes, ascending.
    #[must_use]
    pub fn generation_ids(&self) -> Vec<GenerationId> {
        self.generations.keys().copied().collect()
    }

    /// Returns the catalog generation the snapshot observes.
    #[must_use]
    pub fn catalog_generation(&self) -> GenerationId {
        self.pointer.catalog_generation
    }

    /// Returns true when the snapshot holds every generation its catalog
    /// references, which is the completeness condition for a published state.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.catalog
            .generation_ids()
            .iter()
            .all(|generation| self.generations.contains_key(generation))
    }
}

impl Drop for GenerationReader {
    fn drop(&mut self) {
        self.manager.release_reader(self.lease);
    }
}
/// Returns the staging path used while publishing a catalog state.
#[must_use]
pub fn catalog_staged_path(root: &Path, generation: GenerationId) -> PathBuf {
    catalog_dir(root).join(format!(
        "{}{TMP_SUFFIX}",
        catalog_file_name(generation.get())
    ))
}

/// One complete metadata image staged for publication.
struct StagedImage {
    staged: PathBuf,
    final_path: PathBuf,
    bytes: Vec<u8>,
}

/// One generation written by a publication, with its staged image.
struct PendingGeneration {
    metadata: GenerationMetadata,
    image: StagedImage,
    /// Owning `Database → Schema` location of the generation. Generation bytes
    /// are written inside the logical hierarchy, so the flush phase needs the
    /// owning identity to materialize the directory before staging.
    database_id: DatabaseId,
    schema_id: SchemaId,
}

/// One publication in progress.
///
/// A publication is BUILT in memory from the currently published state, FLUSHED
/// into a staging file per image, VERIFIED by re-reading and cross-validating
/// every staged image, SYNCED, and then PUBLISHED atomically. Nothing it builds
/// is visible to a reader before PUBLISH completes, and the currently published
/// state stays readable for the whole BUILD phase.
///
/// Retention is durable in the state itself: the superseded generation of every
/// object a publication writes is listed as retained by the published catalog
/// state, so publishing never removes the previous generation.
pub struct GenerationPublication {
    manager: Arc<GenerationManager>,
    previous: Option<PublicationPointer>,
    state: CatalogState,
    pending: Vec<PendingGeneration>,
    staged_catalog: StagedImage,
    staged_pointer: StagedImage,
    flushed: bool,
    verified: bool,
    synced: bool,
    published: bool,
}

impl GenerationManager {
    /// Begins an explicit phase-by-phase publication.
    ///
    /// The returned publication is fully built in memory and is not visible to
    /// any reader until [`GenerationPublication::publish`] completes.
    pub fn begin_publication(
        self: &Arc<Self>,
        request: PublicationRequest,
    ) -> Result<GenerationPublication> {
        build(self, request)
    }

    /// Runs the complete publication sequence.
    pub fn publish(self: &Arc<Self>, request: PublicationRequest) -> Result<PublishedGeneration> {
        self.publish_with_fail_point(request, PublicationFailPoint::None)
    }

    /// Runs the complete publication sequence with a failure injected at one
    /// durability or lifecycle boundary.
    ///
    /// Injection exists so that every durability boundary can be exercised
    /// against real filesystem state. Publication either completes or reports
    /// failure; it is never partially applied.
    pub fn publish_with_fail_point(
        self: &Arc<Self>,
        request: PublicationRequest,
        fail_at: PublicationFailPoint,
    ) -> Result<PublishedGeneration> {
        Ok(self.publish_inner(request, fail_at)?.0)
    }

    /// Runs the complete publication sequence and reports phase timings.
    pub fn publish_with_timings(
        self: &Arc<Self>,
        request: PublicationRequest,
    ) -> Result<(PublishedGeneration, PublicationPhaseTimings)> {
        self.publish_inner(request, PublicationFailPoint::None)
    }

    fn publish_inner(
        self: &Arc<Self>,
        request: PublicationRequest,
        fail_at: PublicationFailPoint,
    ) -> Result<(PublishedGeneration, PublicationPhaseTimings)> {
        if fail_at == PublicationFailPoint::BeforeBuild {
            return Err(injected("before publication build"));
        }
        let start = std::time::Instant::now();
        let mut publication = build(self, request)?;
        let mut timings = PublicationPhaseTimings {
            build: start.elapsed(),
            ..PublicationPhaseTimings::default()
        };
        if fail_at == PublicationFailPoint::AfterBuild {
            return Err(injected("after publication build"));
        }
        let start = std::time::Instant::now();
        publication.flush()?;
        timings.flush = start.elapsed();
        if fail_at == PublicationFailPoint::AfterFlush {
            return Err(injected("after publication flush"));
        }
        if fail_at == PublicationFailPoint::BeforeSync {
            return Err(injected("before publication sync"));
        }
        let start = std::time::Instant::now();
        publication.verify()?;
        timings.verify = start.elapsed();
        if fail_at == PublicationFailPoint::DuringVerify {
            return Err(injected("during publication verify"));
        }
        let start = std::time::Instant::now();
        publication.sync()?;
        timings.sync = start.elapsed();
        if fail_at == PublicationFailPoint::AfterSync {
            return Err(injected("after publication sync"));
        }
        if fail_at == PublicationFailPoint::DuringPublish {
            return Err(injected("during publication publish"));
        }
        let start = std::time::Instant::now();
        let published = publication.publish()?;
        timings.publish = start.elapsed();
        if fail_at == PublicationFailPoint::AfterPublish {
            return Err(injected("after publication pointer replaced"));
        }
        Ok((published, timings))
    }
}
/// BUILD: constructs the complete new catalog and generation state in memory.
///
/// The new state is derived from the currently published one without modifying
/// it, which keeps the published state fully readable for the whole build. The
/// new state does not become current here: only PUBLISH does that.
fn build(
    manager: &Arc<GenerationManager>,
    request: PublicationRequest,
) -> Result<GenerationPublication> {
    request.validate()?;
    let previous = manager.lock().pointer;
    let current = match previous {
        Some(pointer) => Some(
            load_catalog_for_generation(&manager.root, pointer.catalog_generation)?
                .ok_or_else(|| missing_catalog(pointer.catalog_generation))?,
        ),
        None => None,
    };
    if let Some(pointer) = previous {
        if request.storage_generation < pointer.storage_generation {
            return Err(PlomidError::new(
                ErrorKind::Conflict,
                "publication would move the durable storage generation backwards",
            ));
        }
        if request.checkpoint_lsn < pointer.checkpoint_lsn {
            return Err(PlomidError::new(
                ErrorKind::Conflict,
                "publication would move the WAL boundary backwards",
            ));
        }
    }
    let (catalog_version, catalog_generation) = match previous {
        Some(pointer) => (
            next_catalog_version(pointer.catalog_version)?,
            next_catalog_generation(pointer.catalog_generation)?,
        ),
        None => (CatalogVersion::new(1), GenerationId::new(1)),
    };
    let mut records: BTreeMap<ObjectId, ObjectRecord> = current
        .as_ref()
        .map(|state| {
            state
                .records()
                .iter()
                .map(|record| (record.object_id, record.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut pending = Vec::with_capacity(request.changes.len());
    let durable_generations: BTreeSet<_> = discover_generation_ids(&manager.root)?
        .into_iter()
        .collect();
    for change in &request.changes {
        let existing = records.get(&change.object_id);
        // A published generation is immutable, so a publication may only ever
        // create a generation that does not exist yet. Without this check a
        // request that reuses a generation identifier would replace a durable
        // image that readers may still be observing.
        if durable_generations.contains(&change.generation_id) {
            return Err(PlomidError::with_detail(
                ErrorKind::Conflict,
                "generation is already published and cannot be replaced",
                format!("generation_id={}", change.generation_id),
            ));
        }
        // Generations of one object are strictly increasing, so a request can
        // never move an object's durable state backwards even when the
        // generation it names has already been reclaimed.
        if let Some(record) = existing {
            if change.generation_id <= record.current_generation {
                return Err(PlomidError::with_detail(
                    ErrorKind::Conflict,
                    "a new generation must be newer than the current generation of its object",
                    format!("generation_id={}", change.generation_id),
                ));
            }
        }
        let retained = existing
            .map(ObjectRecord::generation_ids)
            .unwrap_or_default();
        let mut references = existing
            .map(|record| record.references.clone())
            .unwrap_or_default();
        references.extend(change.references.iter().copied());
        let record = ObjectRecord::with_identity(
            change.object_id,
            change.table_identity,
            change.schema.clone(),
            change.generation_id,
            retained,
            references,
            PublicationState::Published,
        )?;
        let metadata = GenerationMetadata::new(
            change.generation_id,
            change.object_id,
            catalog_version,
            catalog_generation,
            request.storage_generation,
            request.checkpoint_lsn,
            existing.map(|record| record.current_generation),
            PublicationState::Published,
            change.references.clone(),
        )?;
        // Ownership resolution: the change's `TableIdentity` names the real
        // `Database → Schema → Table` location. The durable generation bytes
        // are written under that hierarchy so the physical layout reflects the
        // logical ownership without a separate flat copy.
        let identity = change.table_identity;
        let table_id = identity.table_id;
        DatabaseLayout::new(&manager.root).ensure_table_in_schema(
            identity.database_id,
            identity.schema_id,
            table_id,
        )?;
        pending.push(PendingGeneration {
            database_id: identity.database_id,
            schema_id: identity.schema_id,
            image: StagedImage {
                staged: generation_staged_path_in_schema(
                    &manager.root,
                    identity.database_id,
                    identity.schema_id,
                    table_id,
                    metadata.generation_id,
                ),
                final_path: generation_path_in_schema(
                    &manager.root,
                    identity.database_id,
                    identity.schema_id,
                    table_id,
                    metadata.generation_id,
                ),
                bytes: Vec::new(),
            },
            metadata,
        });
        records.insert(change.object_id, record);
    }
    pending.sort_unstable_by_key(|entry| entry.metadata.generation_id);
    let mut state = CatalogState::staged(
        catalog_version,
        catalog_generation,
        request.storage_generation,
        request.checkpoint_lsn,
        records.into_values().collect(),
    )?
    .as_published();
    for generation in &request.releases {
        state = state.release_generation(*generation)?;
    }
    for object in &request.retired_objects {
        state = state.retire_object(*object)?;
    }
    for object in &request.removed_objects {
        state = state.remove_object(*object)?;
    }
    validate_built_state(
        &state,
        pending.iter().map(|entry| &entry.metadata),
        previous,
    )?;
    let root = &manager.root;
    Ok(GenerationPublication {
        manager: Arc::clone(manager),
        previous,
        state: state.clone(),
        pending,
        staged_catalog: StagedImage {
            staged: catalog_staged_path(root, catalog_generation),
            final_path: catalog_path(root, catalog_generation),
            bytes: Vec::new(),
        },
        staged_pointer: StagedImage {
            staged: publication_pointer_staged_path(root),
            final_path: publication_pointer_path(root),
            bytes: Vec::new(),
        },
        flushed: false,
        verified: false,
        synced: false,
        published: false,
    })
}

/// Validates a newly built state against the generations published with it.
///
/// The check runs both on the in-memory state before any byte is written and
/// again on the decoded staged images, so a publication is only ever applied
/// when its written form agrees with its intended form.
fn validate_built_state<'a>(
    state: &CatalogState,
    metadata: impl Iterator<Item = &'a GenerationMetadata>,
    previous: Option<PublicationPointer>,
) -> Result<()> {
    state.validate()?;
    if let Some(pointer) = previous {
        if state.catalog_generation() <= pointer.catalog_generation {
            return Err(corruption(
                "publication would move the catalog generation backwards",
            ));
        }
        if state.catalog_version() <= pointer.catalog_version {
            return Err(corruption(
                "publication would move the catalog version backwards",
            ));
        }
    }
    let mut seen_objects: BTreeSet<ObjectId> = BTreeSet::new();
    let mut seen_generations: BTreeSet<GenerationId> = BTreeSet::new();
    for metadata in metadata {
        metadata.validate()?;
        if !seen_objects.insert(metadata.object_id) {
            return Err(corruption(
                "publication wrote more than one generation for the same object",
            ));
        }
        if !seen_generations.insert(metadata.generation_id) {
            return Err(corruption(
                "publication wrote more than one generation with the same identity",
            ));
        }
        let record = state.object(metadata.object_id).ok_or_else(|| {
            corruption("publication wrote a generation for an object the catalog does not contain")
        })?;
        if record.current_generation != metadata.generation_id {
            return Err(corruption(
                "publication wrote a generation that is not the current generation of its object",
            ));
        }
        if metadata.catalog_version != state.catalog_version() {
            return Err(corruption(
                "publication wrote a generation against a different catalog version",
            ));
        }
        if metadata.publication_generation != state.catalog_generation() {
            return Err(corruption(
                "publication wrote a generation from a different publication",
            ));
        }
        if !record.is_published() {
            return Err(corruption("publication wrote a retired object"));
        }
    }
    Ok(())
}

/// Returns one plus `version`, rejecting an exhausted version space.
fn next_catalog_version(version: CatalogVersion) -> Result<CatalogVersion> {
    let next = version
        .get()
        .checked_add(1)
        .ok_or_else(|| invalid("catalog version space is exhausted"))?;
    if next == u64::MAX {
        return Err(invalid("catalog version space is exhausted"));
    }
    Ok(CatalogVersion::new(next))
}

/// Returns one plus `generation`, rejecting an exhausted generation space.
fn next_catalog_generation(generation: GenerationId) -> Result<GenerationId> {
    let next = generation
        .get()
        .checked_add(1)
        .ok_or_else(|| invalid("catalog generation space is exhausted"))?;
    if next == u64::MAX {
        return Err(invalid("catalog generation space is exhausted"));
    }
    Ok(GenerationId::new(next))
}

/// Builds an error for an injected durability-boundary failure.
fn injected(what: &str) -> PlomidError {
    PlomidError::new(ErrorKind::Io, format!("injected failure {what}"))
}

/// Builds an error for a missing catalog state.
fn missing_catalog(generation: GenerationId) -> PlomidError {
    PlomidError::with_detail(
        ErrorKind::Corruption,
        "the published catalog state is missing",
        format!("catalog_generation={generation}"),
    )
}
impl GenerationPublication {
    /// Returns the pointer that will become authoritative at PUBLISH.
    pub fn pointer(&self) -> Result<PublicationPointer> {
        PublicationPointer::from_catalog(&self.state)
    }

    /// Returns the catalog state being published.
    #[must_use]
    pub fn catalog(&self) -> &CatalogState {
        &self.state
    }

    /// Returns the generations written by this publication, ascending.
    #[must_use]
    pub fn generation_ids(&self) -> Vec<GenerationId> {
        self.pending
            .iter()
            .map(|entry| entry.metadata.generation_id)
            .collect()
    }

    /// Returns true once the publication has been applied durably.
    #[must_use]
    pub fn is_published(&self) -> bool {
        self.published
    }
}
impl GenerationPublication {
    /// FLUSH: encodes every image and writes it through the buffered I/O layer.
    ///
    /// Encoding is deterministic, so the staged bytes are a pure function of the
    /// state. A flush is not durability: the staged files are not yet durable
    /// and the publication is not yet authoritative.
    pub fn flush(&mut self) -> Result<()> {
        if self.flushed {
            return Err(invalid("publication was already flushed"));
        }
        for entry in &mut self.pending {
            // The generation directory is materialized inside the logical
            // hierarchy before staging, so a generation can never exist
            // without the database/schema/table that owns it.
            DatabaseLayout::new(&self.manager.root).ensure_generation_dir(
                entry.database_id,
                entry.schema_id,
                TableId::new(entry.metadata.object_id.get()),
                entry.metadata.generation_id,
            )?;
            entry.image.bytes = entry.metadata.encode()?;
            durable::stage_bytes(&entry.image.staged, &entry.image.bytes)?;
        }
        self.staged_catalog.bytes = self.state.encode()?;
        durable::stage_bytes(&self.staged_catalog.staged, &self.staged_catalog.bytes)?;
        self.staged_pointer.bytes = self.pointer()?.encode()?;
        durable::stage_bytes(&self.staged_pointer.staged, &self.staged_pointer.bytes)?;
        self.flushed = true;
        Ok(())
    }

    /// VERIFY: re-reads and cross-validates the complete staged publication.
    ///
    /// Verification re-establishes format validity, generation identity, the
    /// catalog relationship, physical references, metadata lengths, checksums,
    /// internal consistency, and the required checkpoint relationship. A
    /// publication that has not passed verification is never published.
    pub fn verify(&mut self) -> Result<()> {
        if !self.flushed {
            return Err(invalid("publication must be flushed before verification"));
        }
        if self.verified {
            return Err(invalid("publication was already verified"));
        }
        let mut decoded_generations = Vec::with_capacity(self.pending.len());
        for entry in &self.pending {
            durable::verify_staged(&entry.image.staged, &entry.image.bytes)?;
            let staged = fs::read(&entry.image.staged).map_err(PlomidError::from)?;
            let decoded = GenerationMetadata::decode(&staged)?;
            if decoded != entry.metadata {
                return Err(corruption("staged generation metadata changed"));
            }
            decoded_generations.push(decoded);
        }
        durable::verify_staged(&self.staged_catalog.staged, &self.staged_catalog.bytes)?;
        let staged = fs::read(&self.staged_catalog.staged).map_err(PlomidError::from)?;
        let decoded = CatalogState::decode(&staged)?;
        if decoded != self.state {
            return Err(corruption("staged catalog state changed"));
        }
        durable::verify_staged(&self.staged_pointer.staged, &self.staged_pointer.bytes)?;
        let staged = fs::read(&self.staged_pointer.staged).map_err(PlomidError::from)?;
        let decoded_pointer = PublicationPointer::decode(&staged)?;
        if decoded_pointer != self.pointer()? {
            return Err(corruption("staged publication pointer changed"));
        }
        validate_built_state(&decoded, decoded_generations.iter(), self.previous)?;
        self.verified = true;
        Ok(())
    }

    /// SYNC: makes every staged image durable.
    ///
    /// This is the durability boundary. Generation metadata is synchronized
    /// before the catalog state that references it, and the catalog state is
    /// synchronized before the pointer that makes it current, so a reader can
    /// never observe a catalog state whose generations are not already durable.
    pub fn sync(&mut self) -> Result<()> {
        if !self.verified {
            return Err(invalid(
                "publication must be verified before synchronization",
            ));
        }
        if self.synced {
            return Err(invalid("publication was already synchronized"));
        }
        for entry in &self.pending {
            durable::sync_file(&entry.image.staged)?;
        }
        durable::sync_file(&self.staged_catalog.staged)?;
        durable::sync_file(&self.staged_pointer.staged)?;
        self.synced = true;
        Ok(())
    }

    /// PUBLISH: applies the publication atomically.
    ///
    /// Generation files are renamed into place first, then the catalog state
    /// that references them, then the publication pointer that makes the state
    /// current, with each directory synchronized so the renames survive a crash.
    /// An interrupted publication therefore leaves either the previous complete
    /// state or the new complete state, and a reader observes one or the other,
    /// never a mixture. The currently published state is never overwritten.
    pub fn publish(&mut self) -> Result<PublishedGeneration> {
        if !self.synced {
            return Err(invalid(
                "publication must be synchronized before publication",
            ));
        }
        if self.published {
            return Err(invalid("publication was already published"));
        }
        let pointer = self.pointer()?;
        let mut inner = self.manager.lock();
        if inner.pointer != self.previous {
            return Err(PlomidError::new(
                ErrorKind::Conflict,
                "the published state changed while this publication was being built",
            ));
        }
        for entry in &self.pending {
            durable::publish_rename(&entry.image.staged, &entry.image.final_path)?;
            if let Some(parent) = entry.image.final_path.parent() {
                // Persist the new logical directory chain before a catalog
                // pointer can make this generation reachable after restart.
                for directory in parent.ancestors() {
                    durable::sync_dir(directory)?;
                    if directory == self.manager.root {
                        break;
                    }
                }
            }
        }
        durable::publish_rename(&self.staged_catalog.staged, &self.staged_catalog.final_path)?;
        durable::sync_dir(&catalog_dir(&self.manager.root))?;
        durable::publish_rename(&self.staged_pointer.staged, &self.staged_pointer.final_path)?;
        durable::sync_dir(&self.manager.root)?;
        inner.pointer = Some(pointer);
        self.published = true;
        Ok(PublishedGeneration {
            pointer,
            catalog: self.state.clone(),
            generations: self.generation_ids(),
            superseded_catalog: self.previous.map(|previous| previous.catalog_generation),
        })
    }
}
impl GenerationManager {
    /// Reclaims generation and catalog files proven unreachable.
    ///
    /// A generation is reclaimable only when it is unreachable from the currently
    /// published catalog state, from every live reader, and from every valid
    /// checkpoint. Reachability is computed under the manager lock and the
    /// deletion happens while that lock is still held, so no publication can
    /// change reachability between the proof and the removal. When reachability
    /// cannot be proven, the generation is retained.
    ///
    /// Deletion is never the authoritative record of state: the publication
    /// pointer and the immutable catalog state are. An interrupted reclamation
    /// can therefore only leave unreferenced files behind, and recovery never
    /// depends on a file this pass removed.
    pub fn gc(self: &Arc<Self>) -> Result<GcOutcome> {
        self.gc_with_fail_point(GcFailPoint::None)
    }

    /// Reclaims unreachable files, with a failure injected at one boundary.
    pub fn gc_with_fail_point(self: &Arc<Self>, fail_at: GcFailPoint) -> Result<GcOutcome> {
        let inner = self.lock();
        let pointer = inner.pointer.ok_or_else(|| {
            PlomidError::with_detail(
                ErrorKind::NotFound,
                "no published catalog state",
                format!("root={}", self.root.display()),
            )
        })?;
        let current = load_catalog_for_generation(&self.root, pointer.catalog_generation)?
            .ok_or_else(|| missing_catalog(pointer.catalog_generation))?;
        let mut reachable: BTreeSet<GenerationId> = current.generation_ids().into_iter().collect();
        let mut retained_catalogs: BTreeSet<GenerationId> = BTreeSet::new();
        retained_catalogs.insert(pointer.catalog_generation);
        for lease in inner.readers.values() {
            retained_catalogs.insert(lease.catalog_generation);
            reachable.extend(lease.generations.iter().copied());
        }
        for path in crate::checkpoint::discover(&self.root)? {
            let checkpoint = match crate::checkpoint::load_checkpoint(&path) {
                Ok(checkpoint) => checkpoint,
                Err(_) => continue,
            };
            if let Some(state) =
                published_catalog_for_version(&self.root, checkpoint.catalog_generation)?
            {
                retained_catalogs.insert(state.catalog_generation());
                reachable.extend(state.generation_ids());
            }
        }
        if fail_at == GcFailPoint::BeforeReclaim {
            return Err(injected("before generation reclamation"));
        }
        let mut retained = Vec::new();
        let mut reclaimed = Vec::new();
        for (generation, path) in discover_generations(&self.root)? {
            if reachable.contains(&generation) {
                retained.push(generation);
                continue;
            }
            fs::remove_file(&path).map_err(PlomidError::from)?;
            if let Some(parent) = path.parent() {
                durable::sync_dir(parent)?;
                // Best-effort removal of the now-empty generation directory
                // (and its empty `segments` child, when present). remove_dir
                // only succeeds on empty directories, so a concurrent flush
                // or a non-empty directory is left untouched; all errors are
                // ignored because directory litter is cosmetic and never
                // affects recovery or reachability.
                let _ = fs::remove_dir(parent.join("segments"));
                let _ = fs::remove_dir(parent);
            }
            reclaimed.push(generation);
            if fail_at == GcFailPoint::DuringReclaim {
                return Err(injected("during generation reclamation"));
            }
        }
        let mut reclaimed_catalogs = Vec::new();
        for path in discover_catalogs(&self.root)? {
            let generation = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(generation_from_catalog_file_name);
            let Some(generation) = generation else {
                continue;
            };
            if retained_catalogs.contains(&GenerationId::new(generation)) {
                continue;
            }
            durable::remove_if_exists(&path);
            reclaimed_catalogs.push(GenerationId::new(generation));
        }
        if fail_at == GcFailPoint::AfterReclaim {
            return Err(injected("after reclamation"));
        }
        Ok(GcOutcome {
            retained,
            reclaimed,
            reclaimed_catalogs,
        })
    }
}
/// Resolves a validated publication pointer into a complete recovered state.
fn resolve_publication(
    root: &Path,
    pointer: PublicationPointer,
    source: PublicationPointerSource,
) -> Result<RecoveryOutcome> {
    let state = load_catalog_for_generation(root, pointer.catalog_generation)?
        .ok_or_else(|| missing_catalog(pointer.catalog_generation))?;
    let generations = validate_publication_state(root, &pointer, &state)?;
    Ok(RecoveryOutcome {
        pointer,
        catalog: state,
        generations,
        source,
    })
}

/// Resolves a discovered published state into a complete recovered state.
fn resolve_state(
    root: &Path,
    state: CatalogState,
    source: PublicationPointerSource,
) -> Result<RecoveryOutcome> {
    let pointer = PublicationPointer::from_catalog(&state)?;
    let generations = validate_publication_state(root, &pointer, &state)?;
    Ok(RecoveryOutcome {
        pointer,
        catalog: state,
        generations,
        source,
    })
}

/// Validates one published state and every generation it references.
///
/// A catalog must not reference a nonexistent generation, a generation must not
/// claim a catalog version that does not exist, physical references must agree
/// between the catalog record and the generation metadata, and the publication
/// state must agree with the authoritative pointer.
fn validate_publication_state(
    root: &Path,
    pointer: &PublicationPointer,
    state: &CatalogState,
) -> Result<Vec<GenerationId>> {
    if !state.is_published() {
        return Err(corruption(
            "the authoritative catalog state is not published",
        ));
    }
    if state.catalog_generation() != pointer.catalog_generation {
        return Err(corruption(
            "catalog state generation disagrees with the publication pointer",
        ));
    }
    if state.catalog_version() != pointer.catalog_version {
        return Err(corruption(
            "catalog version disagrees with the publication pointer",
        ));
    }
    if state.storage_generation() != pointer.storage_generation {
        return Err(corruption(
            "storage generation disagrees with the publication pointer",
        ));
    }
    if state.checkpoint_lsn() != pointer.checkpoint_lsn {
        return Err(corruption(
            "WAL boundary disagrees with the publication pointer",
        ));
    }
    let mut loaded: BTreeMap<GenerationId, GenerationMetadata> = BTreeMap::new();
    for record in state.records() {
        for generation in record.generation_ids() {
            let metadata = load_generation(root, generation)?;
            validate_generation_relationship(&metadata, record, state)?;
            loaded.insert(generation, metadata);
        }
    }
    for record in state.records() {
        for reference in &record.references {
            let metadata = loaded
                .get(&reference.generation_id)
                .ok_or_else(|| corruption("catalog references a generation that does not exist"))?;
            if !metadata.references.contains(reference) {
                return Err(corruption(
                    "catalog physical reference is missing from its generation metadata",
                ));
            }
        }
    }
    Ok(loaded.keys().copied().collect())
}
/// Validates one generation against the catalog record that references it.
///
/// A generation records the durable boundary of the publication that created
/// it. That publication may be older than the state that currently publishes
/// the generation, because a later publication of another object, or a
/// publication that only releases retention, leaves this generation untouched.
/// The relationship that must hold is therefore an ordering one: no generation
/// may claim a newer catalog version, storage generation, or WAL boundary than
/// the published state that references it, and a retained generation must come
/// from a strictly older publication than the one that is current.
fn validate_generation_relationship(
    metadata: &GenerationMetadata,
    record: &ObjectRecord,
    state: &CatalogState,
) -> Result<()> {
    if metadata.object_id != record.object_id {
        return Err(corruption(
            "generation belongs to a different object than its catalog record",
        ));
    }
    if !metadata.is_published() {
        return Err(corruption(
            "published state references an unpublished generation",
        ));
    }
    if metadata.catalog_version > state.catalog_version() {
        return Err(corruption(
            "a generation claims a catalog version newer than the published state",
        ));
    }
    if metadata.storage_generation > state.storage_generation() {
        return Err(corruption(
            "a generation represents a storage generation newer than the published state",
        ));
    }
    if metadata.checkpoint_lsn > state.checkpoint_lsn() {
        return Err(corruption(
            "a generation represents a WAL boundary newer than the published state",
        ));
    }
    if metadata.generation_id == record.current_generation {
        if metadata.publication_generation > state.catalog_generation() {
            return Err(corruption(
                "the current generation was written by a publication newer than the published state",
            ));
        }
    } else {
        if !record
            .retained_generations
            .contains(&metadata.generation_id)
        {
            return Err(corruption(
                "published state references a generation the record does not retain",
            ));
        }
        if metadata.publication_generation >= state.catalog_generation() {
            return Err(corruption(
                "a retained generation is not older than the published state",
            ));
        }
    }
    // The predecessor link is provenance, not a live reference: a superseded
    // generation may already have had its retention released, and reclamation
    // never consults this relationship. It must only describe an older
    // generation than the one that names it.
    if let Some(previous) = metadata.previous_generation {
        if previous >= metadata.generation_id {
            return Err(corruption(
                "generation predecessor is not older than the generation",
            ));
        }
    }
    for reference in &metadata.references {
        if reference.object_id != record.object_id
            || reference.generation_id != metadata.generation_id
        {
            return Err(corruption(
                "generation physical reference disagrees with the generation identity",
            ));
        }
        if !record.references.contains(reference) {
            return Err(corruption(
                "generation physical reference is missing from the catalog record",
            ));
        }
    }
    Ok(())
}

/// Validates that every physical reference names a durable segment.
fn validate_storage_references(root: &Path, state: &CatalogState) -> Result<()> {
    let durable = durable_segment_ids(root)?;
    for record in state.records() {
        for reference in &record.references {
            if !durable.contains(&reference.structure.segment_id.get()) {
                return Err(PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "catalog references a segment the durable image does not contain",
                    format!("segment_id={}", reference.structure.segment_id),
                ));
            }
        }
    }
    Ok(())
}

/// Reads the published catalog state of one version.
///
/// Candidates are examined in descending deterministic generation order and a
/// malformed candidate is skipped, because recovery must be able to identify the
/// newest safe durable state without a damaged historical catalog state making
/// the whole root unrecoverable. The state that is finally selected is fully
/// validated before it is used.
fn published_catalog_for_version(
    root: &Path,
    version: CatalogVersion,
) -> Result<Option<CatalogState>> {
    for path in discover_catalogs(root)?.into_iter().rev() {
        match load_catalog(&path) {
            Ok(state) if state.is_published() && state.catalog_version() == version => {
                return Ok(Some(state))
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::{
        validate_generation_relationship, GenerationMetadata, ObjectChange, PublicationPointer,
        PublicationRequest, PUBLICATION_POINTER_SIZE,
    };
    use crate::catalog::{
        CatalogState, ObjectRecord, PhysicalReference, PhysicalStructure, PublicationState,
        SchemaColumn, SchemaMetadata,
    };
    use plomid_core::{
        BlockId, CatalogVersion, ColumnId, ErrorKind, GenerationId, Lsn, ObjectId, PackId, PageId,
        RowId, SchemaId, SegmentId,
    };

    /// Builds a physical structure inside one object's data.
    pub(super) fn structure_for_test(page: u64) -> PhysicalStructure {
        PhysicalStructure::new(
            SegmentId::new(1),
            PackId::new(1),
            BlockId::new(1),
            PageId::new(page),
            Some(RowId::new(page)),
        )
    }

    /// Builds a physical reference for `object` generation `generation`.
    pub(super) fn reference_for_test(object: u64, generation: u64, page: u64) -> PhysicalReference {
        PhysicalReference::new(
            ObjectId::new(object),
            GenerationId::new(generation),
            structure_for_test(page),
        )
    }

    /// Builds versioned schema metadata for `version`.
    pub(super) fn schema_for_test(version: u64) -> SchemaMetadata {
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

    /// Builds an object change writing generation `generation` of `object`.
    pub(super) fn change_for_test(object: u64, generation: u64) -> ObjectChange {
        ObjectChange::new(
            ObjectId::new(object),
            schema_for_test(1),
            GenerationId::new(generation),
            vec![reference_for_test(object, generation, 1)],
        )
        .expect("change")
    }

    /// Builds a publication request with a durable boundary.
    pub(super) fn request_for_test(
        storage: u64,
        lsn: u64,
        changes: Vec<ObjectChange>,
    ) -> PublicationRequest {
        PublicationRequest::write(GenerationId::new(storage), Lsn::new(lsn), changes)
            .expect("request")
    }

    #[test]
    fn publication_pointer_round_trips_deterministically() {
        let pointer = PublicationPointer::new(
            GenerationId::new(4),
            CatalogVersion::new(9),
            GenerationId::new(2),
            Lsn::new(31),
        )
        .expect("pointer");
        let bytes = pointer.encode().expect("encode");
        assert_eq!(bytes.len(), PUBLICATION_POINTER_SIZE);
        assert_eq!(pointer.encode().expect("encode"), bytes);
        assert_eq!(PublicationPointer::decode(&bytes).expect("decode"), pointer);
        assert_eq!(pointer.publication_state(), PublicationState::Published);
    }

    #[test]
    fn corrupted_publication_pointers_are_rejected() {
        let pointer = PublicationPointer::new(
            GenerationId::new(4),
            CatalogVersion::new(9),
            GenerationId::new(2),
            Lsn::new(31),
        )
        .expect("pointer");
        let valid = pointer.encode().expect("encode");
        let cases: [(usize, u8); 5] = [(0, b'X'), (4, 0xFF), (8, 0x7F), (44, 0x7F), (48, 0xFF)];
        for (offset, value) in cases {
            let mut bytes = valid.clone();
            bytes[offset] = value;
            assert!(
                PublicationPointer::decode(&bytes).is_err(),
                "corrupt pointer byte {offset} must be rejected"
            );
        }
        let mut bytes = valid.clone();
        bytes[52] = 1;
        assert!(PublicationPointer::decode(&bytes).is_err());
        assert!(PublicationPointer::decode(&valid[..10]).is_err());
        let mut bytes = valid.clone();
        bytes.push(0);
        assert!(PublicationPointer::decode(&bytes).is_err());
    }

    #[test]
    fn unpublished_publication_pointers_are_rejected() {
        let error = PublicationPointer {
            catalog_generation: GenerationId::new(4),
            catalog_version: CatalogVersion::new(1),
            storage_generation: GenerationId::new(1),
            checkpoint_lsn: Lsn::new(1),
            state: PublicationState::Staged,
        }
        .validate()
        .expect_err("a staged pointer must be rejected");
        assert_eq!(error.kind(), ErrorKind::Corruption);
    }

    #[test]
    fn generation_metadata_round_trips_and_rejects_corruption() {
        let metadata = GenerationMetadata::new(
            GenerationId::new(5),
            ObjectId::new(1),
            CatalogVersion::new(2),
            GenerationId::new(3),
            GenerationId::new(1),
            Lsn::new(9),
            Some(GenerationId::new(4)),
            PublicationState::Published,
            vec![reference_for_test(1, 5, 1)],
        )
        .expect("generation");
        let bytes = metadata.encode().expect("encode");
        assert_eq!(metadata.encode().expect("encode"), bytes);
        assert_eq!(
            GenerationMetadata::decode(&bytes).expect("decode"),
            metadata
        );
        for offset in [
            0,
            super::OFF_VERSION,
            super::OFF_HEADER_LEN,
            super::OFF_GENERATION_ID,
            super::OFF_OBJECT_ID,
            super::OFF_CATALOG_VERSION,
            super::OFF_PUBLICATION_GENERATION,
            super::OFF_STORAGE_GENERATION,
            super::OFF_CHECKPOINT_LSN,
            super::OFF_PREVIOUS_GENERATION,
            super::OFF_REFERENCE_COUNT,
            super::OFF_REFERENCES_LEN,
            super::OFF_STATE,
            super::OFF_CHECKSUM,
            super::OFF_RESERVED,
        ] {
            let mut corrupted = bytes.clone();
            corrupted[offset] ^= 0xFF;
            assert!(
                GenerationMetadata::decode(&corrupted).is_err(),
                "corrupt generation byte {offset} must be rejected"
            );
        }
        assert!(GenerationMetadata::decode(&bytes[..super::GENERATION_HEADER_SIZE - 1]).is_err());
        let mut corrupted = bytes.clone();
        corrupted.push(0);
        assert!(GenerationMetadata::decode(&corrupted).is_err());
    }

    #[test]
    fn object_change_validation_rejects_mismatched_references() {
        assert!(change_for_test(1, 2).validate().is_ok());
        let error = ObjectChange::new(
            ObjectId::new(1),
            schema_for_test(1),
            GenerationId::new(2),
            vec![reference_for_test(2, 2, 1)],
        )
        .expect_err("a reference to another object must be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        let error = ObjectChange::new(
            ObjectId::new(1),
            schema_for_test(1),
            GenerationId::new(2),
            vec![reference_for_test(1, 3, 1)],
        )
        .expect_err("a reference to another generation must be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        let error = ObjectChange::new(
            ObjectId::new(0),
            schema_for_test(1),
            GenerationId::new(2),
            Vec::new(),
        )
        .expect_err("a zero object ID must be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn publication_requests_require_deterministic_ordering() {
        assert!(request_for_test(1, 1, vec![change_for_test(1, 1)])
            .validate()
            .is_ok());
        assert!(PublicationRequest::write(GenerationId::new(0), Lsn::new(1), Vec::new()).is_err());
        assert!(
            PublicationRequest::write(GenerationId::new(1), Lsn::new(u64::MAX), Vec::new())
                .is_err()
        );
        let error = PublicationRequest::write(
            GenerationId::new(1),
            Lsn::new(1),
            vec![change_for_test(2, 1), change_for_test(1, 1)],
        )
        .expect_err("unordered object changes must be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
        let request = PublicationRequest::write(
            GenerationId::new(1),
            Lsn::new(1),
            vec![change_for_test(1, 1)],
        )
        .expect("request")
        .with_releases(vec![GenerationId::new(3), GenerationId::new(3)])
        .expect("releases");
        assert_eq!(request.releases, vec![GenerationId::new(3)]);
    }

    #[test]
    fn publication_rejects_repeated_generation_identities() {
        let error = PublicationRequest::write(
            GenerationId::new(1),
            Lsn::new(1),
            vec![
                change_for_test(1, 5),
                ObjectChange::new(
                    ObjectId::new(2),
                    schema_for_test(1),
                    GenerationId::new(5),
                    vec![reference_for_test(2, 5, 1)],
                )
                .expect("change"),
            ],
        )
        .expect_err("two objects cannot share one generation identity");
        assert_eq!(error.kind(), ErrorKind::InvalidArgument);
    }

    #[test]
    fn generation_relationships_require_ordering_not_equality() {
        let record = ObjectRecord::new(
            ObjectId::new(1),
            schema_for_test(1),
            GenerationId::new(6),
            vec![GenerationId::new(5)],
            vec![reference_for_test(1, 6, 1), reference_for_test(1, 5, 1)],
            PublicationState::Published,
        )
        .expect("record");
        let state = CatalogState::staged(
            CatalogVersion::new(3),
            GenerationId::new(3),
            GenerationId::new(2),
            Lsn::new(7),
            vec![record.clone()],
        )
        .expect("state")
        .as_published();

        // The object's current generation was written by publication one, which
        // is older than the state that currently publishes it.
        let current = GenerationMetadata::new(
            GenerationId::new(6),
            ObjectId::new(1),
            CatalogVersion::new(2),
            GenerationId::new(2),
            GenerationId::new(2),
            Lsn::new(5),
            Some(GenerationId::new(5)),
            PublicationState::Published,
            vec![reference_for_test(1, 6, 1)],
        )
        .expect("generation");
        assert!(validate_generation_relationship(&current, &record, &state).is_ok());

        // A retained generation is owned by its object and is strictly older.
        let retained = GenerationMetadata::new(
            GenerationId::new(5),
            ObjectId::new(1),
            CatalogVersion::new(1),
            GenerationId::new(1),
            GenerationId::new(1),
            Lsn::new(3),
            None,
            PublicationState::Published,
            vec![reference_for_test(1, 5, 1)],
        )
        .expect("generation");
        assert!(validate_generation_relationship(&retained, &record, &state).is_ok());

        // A generation may not claim a newer durable boundary than the state
        // that references it.
        let future_boundary = GenerationMetadata::new(
            GenerationId::new(6),
            ObjectId::new(1),
            CatalogVersion::new(4),
            GenerationId::new(4),
            GenerationId::new(2),
            Lsn::new(5),
            Some(GenerationId::new(5)),
            PublicationState::Published,
            vec![reference_for_test(1, 6, 1)],
        )
        .expect("generation");
        assert!(validate_generation_relationship(&future_boundary, &record, &state).is_err());

        // A retained generation must come from a strictly older publication.
        let same_publication = GenerationMetadata::new(
            GenerationId::new(5),
            ObjectId::new(1),
            CatalogVersion::new(3),
            GenerationId::new(3),
            GenerationId::new(1),
            Lsn::new(3),
            None,
            PublicationState::Published,
            vec![reference_for_test(1, 5, 1)],
        )
        .expect("generation");
        assert!(validate_generation_relationship(&same_publication, &record, &state).is_err());

        // A predecessor whose retention has been released stays valid provenance.
        let released_record = ObjectRecord::new(
            ObjectId::new(1),
            schema_for_test(1),
            GenerationId::new(6),
            Vec::new(),
            vec![reference_for_test(1, 6, 1)],
            PublicationState::Published,
        )
        .expect("record");
        let released_state = CatalogState::staged(
            CatalogVersion::new(3),
            GenerationId::new(3),
            GenerationId::new(2),
            Lsn::new(7),
            vec![released_record.clone()],
        )
        .expect("state")
        .as_published();
        assert!(
            validate_generation_relationship(&current, &released_record, &released_state).is_ok()
        );

        // A predecessor that is not older than the generation it names is not.
        let newer_predecessor = GenerationMetadata::new(
            GenerationId::new(6),
            ObjectId::new(1),
            CatalogVersion::new(2),
            GenerationId::new(2),
            GenerationId::new(2),
            Lsn::new(5),
            Some(GenerationId::new(7)),
            PublicationState::Published,
            vec![reference_for_test(1, 6, 1)],
        )
        .expect("generation");
        assert!(validate_generation_relationship(&newer_predecessor, &record, &state).is_err());
    }
}
