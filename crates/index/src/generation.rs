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
//! Index generation lifecycle: build → verify → flush → sync → publish → retain → GC.
//!
//! This module is the one authoritative index-generation engine. Both triggers
//! enter the same lifecycle; neither selects a different implementation:
//!
//! ```text
//!   automatic request ──┐
//!                       ├──►  build_index_generation(trigger)  ──► one lifecycle
//!   manual request   ───┘
//!
//!   BUILD   BTreeIndex::create + insert every visible row
//!   VERIFY  reopen the payload and require the entry count to match
//!   FLUSH   stage the generation metadata image
//!   SYNC    fsync the staged record (the durability boundary)
//!   PUBLISH atomic rename; readers see old or new, never a partial record
//!   RETAIN  the superseded current generation becomes `Retained`
//!   GC      only `Retired` generations are reclaimed
//! ```
//!
//! # What this module does not own
//!
//! It owns no catalog, WAL, recovery, checkpoint, allocator, checksum, or
//! storage implementation. It reuses:
//!
//! * [`plomid_storage::DatabaseLayout`] for every path and metadata publication,
//! * [`plomid_storage::IndexGenerationMetadata`] for the durable record,
//! * [`plomid_storage::checksum`] (CRC32C) through the layout's publication,
//! * [`BufferPool`] for the persistent page storage of the B+Tree payload,
//! * [`BTreeIndex`] for the ordered structure itself.
//!
//! # Durable artifacts
//!
//! One index generation owns exactly two files inside its index directory:
//!
//! ```text
//! objects/databases/DB-*/schemas/S-*/tables/T-*/indexes/I-*/
//! ├── META.dat              index identity (existing)
//! ├── GEN-<generation>.dat  generation metadata (this module)
//! └── IDX-<generation>.dat  persistent B+Tree payload
//! ```
//!
//! Both artifacts are named by generation identity, so rebuilding an index can
//! never overwrite a generation that is still retained.

use std::path::{Path, PathBuf};

use plomid_core::{
    ErrorKind, GenerationId, IndexId, ObjectId, PlomidError, Result, RowId, TableIdentity,
    DEFAULT_POOL_CAPACITY,
};

use plomid_storage::{
    DatabaseLayout, IndexGenerationMetadata, IndexGenerationState, IndexGenerationTrigger,
};

use crate::btree::BTreeIndex;
use crate::tree::{ArtIndex, IndexKind};

// Default capacity of the buffer pool backing an index payload build.
// Buffer-pool pages per index B+Tree file (one page is 16 KiB).
//
// The pool allocates frames lazily, so this is a ceiling on the resident
// working set, not a preallocation. The previous 64-page (1 MiB) ceiling
// thrashed on any index spanning more than a few thousand rows: an insert
// touches a root-to-leaf path, and once the index exceeded the cache every
// touch re-read pages that had just been evicted, with a write syscall per
// dirty eviction. 1024 pages (16 MiB) keeps a realistic index working set
// resident while staying bounded.
// (Floating section note kept as plain comments so the blank line below
// does not read as detached documentation.)

/// Reduces a multi-current record set to the one authoritative generation.
///
/// Generations of one index strictly increase, so when more than one record is
/// marked current the one with the highest identity is the generation a
/// completed build would have left current; the others are presented as
/// retained. Records that are not marked current pass through unchanged.
fn normalize_supersession(
    records: Vec<IndexGenerationMetadata>,
) -> Result<Vec<IndexGenerationMetadata>> {
    let newest = records
        .iter()
        .filter(|record| record.state.is_current())
        .map(|record| record.generation_id)
        .max();
    let Some(newest) = newest else {
        return Ok(records);
    };
    records
        .into_iter()
        .map(|record| {
            if record.state.is_current() && record.generation_id != newest {
                record.with_state(IndexGenerationState::Retained)
            } else {
                Ok(record)
            }
        })
        .collect()
}

/// Outcome of an automatic generation request.
///
/// Automatic maintenance must never churn generations, so a request whose
/// source data generation is already represented reports
/// [`Self::AlreadyCurrent`] and writes nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticOutcome {
    /// A new generation was built and published.
    Published(GenerationId),
    /// The current generation already represents this source state; no work.
    AlreadyCurrent(GenerationId),
    /// The index has no rows and no generation, so nothing was created.
    ///
    /// An index over an empty table is legitimately empty; materializing a
    /// generation for it would create durable state with no content.
    Empty,
}

/// Deterministic failure point in the index-generation build lifecycle.
///
/// Mirrors the existing `ColumnarFailPoint` / `CompactionFailPoint` convention:
/// production callers pass [`Self::None`], and a test selects a single stage to
/// prove that a crash there cannot make an unverified or unpublished generation
/// visible. The stages name the durable boundary the build has just crossed, so
/// a test can assert exactly which state a crash leaves behind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexBuildFailPoint {
    /// No failure; the production path.
    None,
    /// Fail after the payload is built and synced, before VERIFY.
    ///
    /// The payload bytes exist but verification never ran, so the build has not
    /// produced a trustworthy generation.
    AfterBuild,
    /// Fail after VERIFY, before the metadata record is published.
    ///
    /// This is the crash-before-publication boundary: a verified payload may
    /// exist on disk, but no durable record makes it current.
    BeforePublish,
    /// Fail after the metadata record is atomically published.
    ///
    /// The new generation IS current and reader-visible in this case; the error
    /// only reports that the caller did not observe the success.
    AfterPublish,
}

impl IndexBuildFailPoint {
    /// Reports the injected error when this point matches `at`.
    ///
    /// A `None` fail point never fails, and a fail point only fires at its own
    /// stage, so exactly one boundary can be exercised per build.
    fn failure_at(self, at: Self) -> Option<PlomidError> {
        if self != at {
            return None;
        }
        match self {
            Self::None => None,
            Self::AfterBuild => Some(PlomidError::new(
                ErrorKind::Io,
                "injected failure after index generation build",
            )),
            Self::BeforePublish => Some(PlomidError::new(
                ErrorKind::Io,
                "injected failure before index generation publication",
            )),
            Self::AfterPublish => Some(PlomidError::new(
                ErrorKind::Io,
                "injected failure after index generation publication",
            )),
        }
    }
}

/// Result of one garbage-collection pass over an index.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexGcOutcome {
    /// Generations proven reachable and therefore retained, ascending.
    pub retained: Vec<GenerationId>,
    /// Generation payload and metadata files reclaimed, ascending.
    pub reclaimed: Vec<GenerationId>,
}

/// One built index generation before publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedIndexGeneration {
    /// Identity of the index.
    pub index_id: IndexId,
    /// Immutable generation identity that is now current.
    pub generation_id: GenerationId,
    /// Data generation the build read from.
    pub source_data_generation_id: GenerationId,
    /// Provenance of the build.
    pub trigger: IndexGenerationTrigger,
    /// Number of indexed keys.
    pub entry_count: u64,
}

/// The one authoritative index-generation engine.
///
/// Every automatic and manual request converges on the single build path. The
/// store is stateless: current state is discovered from the durable layout, so a
/// process restart cannot lose or alias a generation identity.
#[derive(Clone, Debug)]
pub struct IndexGenerationStore {
    layout: DatabaseLayout,
}

impl IndexGenerationStore {
    /// Opens the engine over a storage root.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            layout: DatabaseLayout::new(root),
        }
    }

    /// The layout this engine resolves every path through.
    #[must_use]
    pub fn layout(&self) -> &DatabaseLayout {
        &self.layout
    }

    /// The storage root.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.layout.root().to_path_buf()
    }

    /// Every published generation of one index, ascending by identity.
    ///
    /// The returned view carries the authoritative meaning of the durable
    /// records. Generations of one index are strictly increasing, and a build
    /// installs its new record atomically and *then* demotes the generation it
    /// supersedes. A crash in the window between those two steps can therefore
    /// leave the superseded record still marked current. The newest record is
    /// unambiguous, so that window is resolved here exactly as a completed build
    /// would have left it: the highest generation is current and every older
    /// record is reported retained. This defines the meaning of a legal state
    /// rather than repairing corruption — an unreadable or malformed record is
    /// still rejected by the decoder.
    pub fn generations(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
    ) -> Result<Vec<IndexGenerationMetadata>> {
        let records = self.layout.discover_index_generations_in_schema(
            identity.database_id,
            identity.schema_id,
            identity.table_id,
            index_id,
        )?;
        normalize_supersession(records)
    }

    /// The durable records exactly as they are stored, without normalization.
    ///
    /// Used by the write path to decide which records still need their
    /// supersession written, so a completed build converges the on-disk state
    /// instead of relying on the read-time rule indefinitely.
    fn durable_records(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
    ) -> Result<Vec<IndexGenerationMetadata>> {
        self.layout.discover_index_generations_in_schema(
            identity.database_id,
            identity.schema_id,
            identity.table_id,
            index_id,
        )
    }

    /// The current (authoritative) generation of one index, when one exists.
    ///
    /// Exactly one generation is current. [`Self::generations`] resolves the
    /// crash window where a newer record was installed before its predecessor's
    /// supersession was written, so the newest generation is always the one
    /// reported here. The check below is a defensive assertion: it can only fire
    /// if that resolution were ever bypassed.
    pub fn current_generation(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
    ) -> Result<Option<IndexGenerationMetadata>> {
        let mut current: Option<IndexGenerationMetadata> = None;
        for record in self.generations(identity, index_id)? {
            if !record.state.is_current() {
                continue;
            }
            if current.is_some() {
                return Err(PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "an index has more than one current generation",
                    format!("index_id={}", index_id.get()),
                ));
            }
            current = Some(record);
        }
        Ok(current)
    }

    /// True when the current generation was built from exactly `source`.
    ///
    /// This is the compatibility rule: an index generation is usable only for
    /// the data generation it was built from, so a reader never combines a data
    /// generation with an index built from a different one.
    pub fn is_current_for(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        source: GenerationId,
    ) -> Result<bool> {
        Ok(self
            .current_generation(identity, index_id)?
            .is_some_and(|record| record.is_compatible(source)))
    }

    /// Derives the in-memory ART for one index from its current durable
    /// generation.
    ///
    /// The persistent B+Tree payload of the current generation is the sole
    /// authority; the returned ART is a *derived* runtime structure, so this is
    /// a reconstruction rather than a recovery path and adds no durability of
    /// its own:
    ///
    /// ```text
    /// current index generation      (durable, discovered here)
    ///          │  payload B+Tree
    ///          ▼
    ///      scan_all()               ordered entries
    ///          │
    ///          ▼
    /// ArtIndex::rebuild_from_entries
    /// ```
    ///
    /// Returns `Ok(None)` when the index has no published generation, which is
    /// the correct answer for an index that was never built. A durable record
    /// that cannot be read, or a B+Tree whose contents are corrupt, produces a
    /// deterministic error instead of a partial tree — a caller can never
    /// mistake an incomplete reconstruction for a valid index.
    ///
    /// Read-only: no directory is created and no metadata is written.
    pub fn derive_art(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
    ) -> Result<Option<ArtIndex>> {
        let Some(current) = self.current_generation(identity, index_id)? else {
            return Ok(None);
        };
        let payload_path = self.layout.index_payload_path_in_schema(
            identity.database_id,
            identity.schema_id,
            identity.table_id,
            index_id,
            current.generation_id,
        );
        let mut tree = BTreeIndex::open(&payload_path, DEFAULT_POOL_CAPACITY)?;
        // Uniqueness is recorded in the persistent root metadata, so the derived
        // index inherits the durable policy instead of guessing it.
        let kind = if tree.is_unique() {
            IndexKind::Unique
        } else {
            IndexKind::NonUnique
        };
        let mut entries: Vec<(Vec<u8>, RowId)> = Vec::new();
        for entry in tree.scan_all()? {
            for row_id in entry.row_ids {
                entries.push((entry.key.clone(), row_id));
            }
        }
        tree.close()?;
        let art = ArtIndex::rebuild_from_entries(kind, entries).map_err(|error| {
            PlomidError::with_detail(
                ErrorKind::Corruption,
                "index generation could not be reconstructed into a runtime index",
                format!("index_id={} error={error}", index_id.get()),
            )
        })?;
        Ok(Some(art))
    }

    /// The next free generation identity for one index.
    ///
    /// Identities are never reused: the value is derived from what is durably
    /// published, so a reopened process continues the sequence instead of
    /// restarting it. This is what prevents a rebuild from aliasing a payload a
    /// retained generation still owns.
    pub fn next_generation_id(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
    ) -> Result<GenerationId> {
        let next = self
            .generations(identity, index_id)?
            .last()
            .map_or(1, |record| record.generation_id.get() + 1);
        if next == 0 || next == u64::MAX {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "index generation identity space is exhausted",
            ));
        }
        Ok(GenerationId::new(next))
    }
    // ------------------------------------------------------------------
    // The one engine
    // ------------------------------------------------------------------

    /// Builds, verifies, and atomically publishes one index generation.
    ///
    /// This is the single implementation behind both triggers. `trigger` is
    /// recorded as provenance and changes nothing else about the lifecycle.
    fn build_generation(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        source_data_generation: GenerationId,
        trigger: IndexGenerationTrigger,
        rows: &[(Vec<u8>, RowId)],
        fail_point: IndexBuildFailPoint,
    ) -> Result<PublishedIndexGeneration> {
        if source_data_generation.is_zero() {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "index generation requires a source data generation",
            ));
        }
        let database_id = identity.database_id;
        let schema_id = identity.schema_id;
        let table_id = identity.table_id;
        let object_id = ObjectId::new(table_id.get());

        // The index directory must exist before any payload is created, so a
        // generation can never appear outside a materialized index.
        self.layout
            .ensure_index_in_schema(database_id, schema_id, table_id, index_id)?;

        let generation_id = self.next_generation_id(identity, index_id)?;

        // BUILD — create the payload and insert every visible row.
        let payload_path = self.layout.index_payload_path_in_schema(
            database_id,
            schema_id,
            table_id,
            index_id,
            generation_id,
        );
        let mut index = BTreeIndex::create(
            &payload_path,
            DEFAULT_POOL_CAPACITY,
            index_id,
            object_id,
            generation_id,
            false,
        )?;
        // BUILD — pack the whole key set bottom-up in one pass. The input
        // holds one (key, RowId) pair per visible row with distinct RowIds,
        // which is exactly the bulk builder's contract; per-row insertion
        // here re-descended, re-chunked, and re-split on every row
        // (quadratic on low-cardinality runs — measured 181s at 1M rows).
        let perf = tracing::enabled!(tracing::Level::DEBUG, target: "plomid::perf");
        let phase_started = std::time::Instant::now();
        index.build_bulk(rows.to_vec())?;
        let insert_us = phase_started.elapsed().as_micros() as u64;
        let built = index.stats()?;
        let phase_started = std::time::Instant::now();
        index.sync()?;
        let sync_us = phase_started.elapsed().as_micros() as u64;
        let phase_started = std::time::Instant::now();
        index.close()?;
        let close_us = phase_started.elapsed().as_micros() as u64;
        if let Some(error) = fail_point.failure_at(IndexBuildFailPoint::AfterBuild) {
            return Err(error);
        }

        // VERIFY — reopen exactly what was written and require agreement.
        let phase_started = std::time::Instant::now();
        let mut reopened = BTreeIndex::open(&payload_path, DEFAULT_POOL_CAPACITY)?;
        let reopen_us = phase_started.elapsed().as_micros() as u64;
        let phase_started = std::time::Instant::now();
        let verified = reopened.stats()?;
        let stats_us = phase_started.elapsed().as_micros() as u64;
        if verified.entry_count != built.entry_count {
            return Err(PlomidError::with_detail(
                ErrorKind::Corruption,
                "index generation payload failed verification",
                format!(
                    "expected={} observed={}",
                    built.entry_count, verified.entry_count
                ),
            ));
        }
        reopened.close()?;

        // FLUSH + SYNC + PUBLISH — the metadata record is staged, re-read,
        // fsynced, and renamed atomically, so a reader observes either the
        // previous complete record or the new complete record.
        let record = IndexGenerationMetadata::new(
            index_id,
            generation_id,
            database_id,
            schema_id,
            table_id,
            source_data_generation,
            trigger,
            IndexGenerationState::Current,
        )?;
        let meta_path = self.layout.index_generation_path_in_schema(
            database_id,
            schema_id,
            table_id,
            index_id,
            generation_id,
        );
        if let Some(error) = fail_point.failure_at(IndexBuildFailPoint::BeforePublish) {
            return Err(error);
        }
        record.publish(&meta_path)?;
        if let Some(error) = fail_point.failure_at(IndexBuildFailPoint::AfterPublish) {
            return Err(error);
        }

        // RETAIN — the generation this one supersedes stops being current but
        // stays reachable, so the existing retention rules decide reclamation.
        // The durable records are read (not the normalized view) so a completed
        // build still writes the demotion and the on-disk state converges.
        let superseded: Vec<IndexGenerationMetadata> = self
            .durable_records(identity, index_id)?
            .into_iter()
            .filter(|existing| existing.generation_id != generation_id)
            .filter(|existing| existing.state.is_current())
            .collect();
        for previous in superseded {
            let retained = previous.with_state(IndexGenerationState::Retained)?;
            let previous_path = self.layout.index_generation_path_in_schema(
                database_id,
                schema_id,
                table_id,
                index_id,
                previous.generation_id,
            );
            retained.publish(&previous_path)?;
        }

        if perf {
            tracing::debug!(
                target: "plomid::perf",
                event = "index_build",
                index_id = index_id.get(),
                rows = rows.len() as u64,
                insert_us,
                sync_us,
                close_us,
                reopen_us,
                stats_us,
            );
        }
        Ok(PublishedIndexGeneration {
            index_id,
            generation_id,
            source_data_generation_id: source_data_generation,
            trigger,
            entry_count: built.entry_count,
        })
    }
    // ------------------------------------------------------------------
    // Automatic trigger
    // ------------------------------------------------------------------

    /// Requests an automatic index generation for a stable source data state.
    ///
    /// This is the automatic maintenance entry point. It is deliberately
    /// idempotent: a request whose source data generation is already represented
    /// by the current generation reports [`AutomaticOutcome::AlreadyCurrent`] and
    /// writes nothing, so repeated lifecycle events cannot churn generations.
    ///
    /// DML never calls this. The trigger is an explicit deterministic boundary,
    /// so a write never rebuilds an index.
    pub fn request_automatic_generation(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        source_data_generation: GenerationId,
        rows: &[(Vec<u8>, RowId)],
    ) -> Result<AutomaticOutcome> {
        self.request_automatic_generation_with_failpoint(
            identity,
            index_id,
            source_data_generation,
            rows,
            IndexBuildFailPoint::None,
        )
    }

    /// Automatic generation with an explicit deterministic failure point.
    ///
    /// Identical to [`Self::request_automatic_generation`] apart from the
    /// injectable failure, so a test can prove that a crash at a chosen durable
    /// boundary cannot publish a half-built generation. Production callers use
    /// the fail-point-free entry point.
    pub fn request_automatic_generation_with_failpoint(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        source_data_generation: GenerationId,
        rows: &[(Vec<u8>, RowId)],
        fail_point: IndexBuildFailPoint,
    ) -> Result<AutomaticOutcome> {
        if let Some(current) = self.current_generation(identity, index_id)? {
            if current.is_compatible(source_data_generation) {
                return Ok(AutomaticOutcome::AlreadyCurrent(current.generation_id));
            }
        } else if rows.is_empty() {
            // Nothing was ever materialized and there is nothing to index, so
            // creating durable state would be meaningless.
            return Ok(AutomaticOutcome::Empty);
        }
        let published = self.build_generation(
            identity,
            index_id,
            source_data_generation,
            IndexGenerationTrigger::Automatic,
            rows,
            fail_point,
        )?;
        Ok(AutomaticOutcome::Published(published.generation_id))
    }

    // ------------------------------------------------------------------
    // Manual trigger
    // ------------------------------------------------------------------

    /// Rebuilds an index generation from an explicit request.
    ///
    /// Unlike the automatic path, a manual rebuild always creates a new
    /// generation, even when the current one already represents the same source
    /// state. The superseded generation is retained rather than replaced, so a
    /// reader holding it keeps a valid generation.
    pub fn rebuild_generation_manually(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        source_data_generation: GenerationId,
        rows: &[(Vec<u8>, RowId)],
    ) -> Result<PublishedIndexGeneration> {
        self.rebuild_generation_manually_with_failpoint(
            identity,
            index_id,
            source_data_generation,
            rows,
            IndexBuildFailPoint::None,
        )
    }

    /// Manual rebuild with an explicit deterministic failure point.
    ///
    /// Identical to [`Self::rebuild_generation_manually`] apart from the
    /// injectable failure, so a test can prove a failed rebuild leaves the
    /// previous generation current.
    pub fn rebuild_generation_manually_with_failpoint(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        source_data_generation: GenerationId,
        rows: &[(Vec<u8>, RowId)],
        fail_point: IndexBuildFailPoint,
    ) -> Result<PublishedIndexGeneration> {
        self.build_generation(
            identity,
            index_id,
            source_data_generation,
            IndexGenerationTrigger::Manual,
            rows,
            fail_point,
        )
    }

    // ------------------------------------------------------------------
    // Retention and reclamation
    // ------------------------------------------------------------------

    /// Marks a generation retired, making its files reclaimable.
    ///
    /// Retirement is explicit: nothing retires a generation merely because a
    /// newer one exists. A caller retires a generation only once the existing
    /// reader/snapshot rules prove no valid reader can still require it.
    pub fn retire_generation(
        &self,
        identity: TableIdentity,
        index_id: IndexId,
        generation: GenerationId,
    ) -> Result<IndexGenerationMetadata> {
        let current = self.layout.read_index_generation_in_schema(
            identity.database_id,
            identity.schema_id,
            identity.table_id,
            index_id,
            generation,
        )?;
        let retired = current.with_state(IndexGenerationState::Retired)?;
        let path = self.layout.index_generation_path_in_schema(
            identity.database_id,
            identity.schema_id,
            identity.table_id,
            index_id,
            generation,
        );
        retired.publish(&path)?;
        Ok(retired)
    }

    /// Reclaims the durable files of every `Retired` generation of one index.
    ///
    /// Only retired generations are reclaimed. `Current` and `Retained`
    /// generations keep both artifacts, so reclamation can never delete a
    /// generation a valid reader may still resolve.
    pub fn gc(&self, identity: TableIdentity, index_id: IndexId) -> Result<IndexGcOutcome> {
        let mut outcome = IndexGcOutcome::default();
        for record in self.generations(identity, index_id)? {
            let generation = record.generation_id;
            if record.state != IndexGenerationState::Retired {
                outcome.retained.push(generation);
                continue;
            }
            let payload = self.layout.index_payload_path_in_schema(
                identity.database_id,
                identity.schema_id,
                identity.table_id,
                index_id,
                generation,
            );
            let meta = self.layout.index_generation_path_in_schema(
                identity.database_id,
                identity.schema_id,
                identity.table_id,
                index_id,
                generation,
            );
            if payload.is_file() {
                std::fs::remove_file(&payload).map_err(PlomidError::from)?;
            }
            if meta.is_file() {
                std::fs::remove_file(&meta).map_err(PlomidError::from)?;
            }
            outcome.reclaimed.push(generation);
        }
        Ok(outcome)
    }
}
