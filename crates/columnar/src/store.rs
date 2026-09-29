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
//! Durable Hot Row Store → Immutable Columnar Segment flush.
//!
//! This module is the integration point between the columnar pipeline and the
//! existing PLOMID infrastructure. It owns no storage, WAL, MVCC, checkpoint,
//! recovery, or checksum implementation of its own; it coordinates the ones
//! that already exist:
//!
//! ```text
//! HOT ROW STORE (plomid_txn)
//!       │  engine.scan observes the committed MVCC snapshot
//!       ▼
//!     FLUSH (materialize → statistics → compress → verify)
//!       │  FlushedSegment bytes, still private to the caller
//!       ▼
//! STAGE (one StorageEngine transaction over StorageManager)
//!       │  manifest + fixed-size slices under deterministic keys
//!       ▼
//!    VERIFY (reassemble + SegmentReader::decode + row-count check)
//!       │
//!       ▼
//!     SYNC (engine.sync; commit then applies + syncs)
//!       │
//!       ▼
//!   PUBLISH (GenerationManager::publish; catalog stays authoritative)
//!       │
//!       ▼
//!    RETAIN / GC (existing generation machinery, untouched)
//! ```
//!
//! # Physical storage
//!
//! Columnar payloads live inside the existing storage engine: the segment
//! image is split into fixed-size slices stored as ordinary transactional
//! key/value pairs through [`StorageEngine`], so they ride the same B+Tree
//! pages, WAL, checkpoint, and recovery path as Hot Row Store data. No
//! per-column or per-segment files are created. The key scheme keeps columnar
//! keys disjoint from Hot Row Store keys and orders slices deterministically:
//!
//! ```text
//! key = PREFIX[4] | segment_id[u64 BE] | kind[u8] | index[u32 BE]
//! kind = 0x00 manifest (single record describing the segment)
//! kind = 0x01 payload slice (index 0..slice_count)
//! ```
//!
//! # Generation references
//!
//! Publication records one [`PhysicalReference`] per payload slice so the
//! generation catalog names the segment's durable footprint. The structures
//! are deterministic logical positions in the existing
//! segment → pack → block → page naming scheme derived from the columnar
//! segment identity and slice ordinal; they carry no byte offsets, paths, or
//! pointers, keeping the logical columnar identifier independent of physical
//! location as the catalog design requires.
//!
//! # Crash safety
//!
//! Staged slices are buffered in a single storage transaction and become
//! durable only at commit; generation publication happens after commit. A
//! crash or injected failure before publication therefore leaves either no
//! trace (transaction never committed) or a fully committed but unpublished
//! payload that no reader discovers, because readers enumerate published
//! generations through [`GenerationManager`] and load slices only for listed
//! segments. No columnar-specific recovery exists.

use crate::column::ColumnType;
use crate::flush::{flush, FlushConfig, FlushedSegment};
use crate::layout::{corruption, invalid};
use crate::read::SegmentReader;
use plomid_core::{
    BlockId, CatalogVersion, ColumnId, GenerationId, Lsn, ObjectId, PageId, Result, SchemaId,
    SegmentId, TableIdentity,
};
use plomid_storage::{
    GenerationManager, ObjectChange, PhysicalReference, PhysicalStructure, PublicationRequest, Row,
    SchemaColumn, SchemaMetadata, StorageEngine, StorageEngineTransaction,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Key prefix distinguishing columnar payloads from Hot Row Store keys.
pub const COLUMNAR_KEY_PREFIX: [u8; 4] = [0x7E, b'c', b's', 0x02];
/// Key kind byte of the segment manifest record.
pub const COLUMNAR_MANIFEST_KIND: u8 = 0x00;
/// Key kind byte of a payload slice record.
pub const COLUMNAR_SLICE_KIND: u8 = 0x01;
/// Size of one persisted payload slice.
///
/// The slice is sized against the page budget of the existing B+Tree: a slice
/// plus its key/value framing must fit a page payload alongside the entries the
/// tree's byte-aware leaf split places next to it. With a 16 KiB page the
/// payload is 16332 bytes and a slice key is 17 bytes. 4 KiB keeps several
/// slices per page while leaving every slice page-aligned in size.
pub const COLUMNAR_STORAGE_SLICE: usize = 4 * 1024;
/// Magic of the persisted segment manifest value.
pub const COLUMNAR_MANIFEST_MAGIC: [u8; 4] = *b"PLCM";
/// Version of the persisted segment manifest value.
pub const COLUMNAR_MANIFEST_VERSION: u32 = 1;
/// Encoded length of the segment manifest value in bytes.
pub const COLUMNAR_MANIFEST_LEN: usize = 44;

/// Injection point for flush-boundary failure tests.
///
/// Variants name the durability boundary the failure precedes, so a test can
/// prove that an interruption at any step leaves readers on the last
/// published generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnarFailPoint {
    /// No failure; normal production path.
    None,
    /// Fail after the segment image is built, before anything is staged.
    AfterBuild,
    /// Fail after slices are staged in the transaction, before verification.
    AfterFlush,
    /// Fail during verification of the staged image.
    DuringVerify,
    /// Fail after verification, before synchronization.
    BeforeSync,
    /// Fail after synchronization, before the staging transaction commits.
    AfterSync,
    /// Fail before generation publication.
    DuringPublish,
    /// Fail after the publication pointer is replaced.
    ///
    /// The segment IS published and reader-visible in this case; the error
    /// only reports that the caller did not observe the success.
    AfterPublish,
}

/// Publication coordinates for one columnar flush.
///
/// The caller names the catalog object, schema, and generation identity; the
/// store fills in the payload-derived structures. Storage generation and WAL
/// boundary come from the caller's checkpoint flow, exactly as any other
/// generation publication.
#[derive(Clone, Debug)]
pub struct ColumnarPublish {
    /// Catalog object this segment is published for.
    pub object_id: ObjectId,
    /// Owning `Database → Schema → Table` location of the object.
    ///
    /// Binds the publication to the SQL-facing logical tree so the new
    /// generation resolves inside
    /// `objects/databases/DB-*/schemas/S-*/tables/T-*/generations/GEN-*`.
    /// Callers that know the real hierarchy pass it through
    /// [`Self::with_identity`]; otherwise it defaults to the historical flat
    /// mapping so pre-hierarchy callers keep working.
    pub table_identity: TableIdentity,
    /// Schema state the segment is published against.
    pub schema: SchemaMetadata,
    /// Immutable generation identity of the new segment.
    pub generation: GenerationId,
    /// Durable storage generation of the publishing state.
    pub storage_generation: GenerationId,
    /// WAL boundary of the publishing state.
    pub checkpoint_lsn: Lsn,
}

impl ColumnarPublish {
    /// Creates publication coordinates for `generation`.
    ///
    /// The owning identity defaults to the compatibility mapping derived from
    /// the object and schema identities. SQL-driven writers should chain
    /// [`Self::with_identity`] to publish under the real hierarchy.
    pub fn new(
        object_id: ObjectId,
        schema: SchemaMetadata,
        generation: GenerationId,
        storage_generation: GenerationId,
        checkpoint_lsn: Lsn,
    ) -> Self {
        let table_identity = TableIdentity::from_object_and_schema(object_id, schema.schema_id);
        Self {
            object_id,
            table_identity,
            schema,
            generation,
            storage_generation,
            checkpoint_lsn,
        }
    }

    /// Sets the explicit owning `Database → Schema → Table` identity.
    ///
    /// Used by writers that know the real hierarchy location of the object, so
    /// the published generation lands in the correct table directory instead of
    /// being inferred from the object identity.
    #[must_use]
    pub fn with_identity(mut self, table_identity: TableIdentity) -> Self {
        self.table_identity = table_identity;
        self
    }
}

/// A published immutable columnar segment.
#[derive(Clone, Debug)]
pub struct PublishedColumnarSegment {
    /// Segment identity chosen at flush time.
    pub segment_id: SegmentId,
    /// Generation the segment was published under.
    pub generation_id: GenerationId,
    /// Decoded segment header.
    pub header: crate::segment::SegmentHeader,
    /// Number of persisted payload slices.
    pub slice_count: u32,
    /// Exact persisted image bytes.
    pub bytes: Vec<u8>,
}

/// Durable Hot Row Store → Immutable Columnar Segment flush.
///
/// The store holds the generation root (shared with the storage engine's
/// root) and the authoritative [`GenerationManager`]. Segment identities are
/// allocated from a process-local monotonic counter; a fresh store over an
/// empty root starts at 1.
pub struct ColumnarStore {
    root: PathBuf,
    generations: Arc<GenerationManager>,
    next_segment_id: AtomicU64,
}

impl ColumnarStore {
    /// Opens (creating when absent) the generation state under `root`.
    ///
    /// `root` is normally the same directory given to the storage engine, so
    /// columnar payloads and generation metadata share one filesystem root
    /// without either subsystem owning a private directory layout.
    pub fn open(root: &std::path::Path) -> Result<Self> {
        Ok(Self {
            root: root.to_path_buf(),
            generations: Arc::new(GenerationManager::open(root)?),
            next_segment_id: AtomicU64::new(1),
        })
    }

    /// Recovers the published state without modifying it.
    pub fn recover(root: &std::path::Path) -> Result<plomid_storage::RecoveryOutcome> {
        GenerationManager::recover(root)
    }

    /// Returns the authoritative generation manager.
    #[must_use]
    pub fn generations(&self) -> &Arc<GenerationManager> {
        &self.generations
    }

    /// Allocates the next columnar segment identity.
    pub fn allocate_segment_id(&self) -> SegmentId {
        SegmentId::new(self.next_segment_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Advances the segment counter past every segment already persisted.
    ///
    /// Segment identities must be unique for as long as a payload that names
    /// them exists in durable storage, but the in-process counter starts at one
    /// on every [`Self::open`]. A store reopened over a root that already holds
    /// segments would otherwise hand out identities that are still in use and
    /// overwrite another generation's payload. This reads the persisted segment
    /// manifests through the existing read path and moves the counter beyond
    /// them, so a store opened at any point in the database's life allocates
    /// identities that were never used before.
    ///
    /// Over an empty root the counter is unchanged, so callers that have always
    /// opened a fresh store observe exactly the same identities as before.
    pub fn recover_segment_ids<E>(&self, engine: &mut E) -> Result<()>
    where
        E: StorageEngine,
    {
        let next = self
            .list_segments(engine)?
            .last()
            .map_or(1, |segment| segment.get() + 1);
        let _ = self.next_segment_id.fetch_max(next, Ordering::SeqCst);
        Ok(())
    }

    /// Builds the schema metadata for a columnar publication.
    ///
    /// Column identities are positional (column `i` of the flushed row) and
    /// type codes reuse the columnar format tags, which the catalog stores
    /// opaquely.
    pub fn columnar_schema(
        schema_id: SchemaId,
        version: CatalogVersion,
        column_types: &[ColumnType],
    ) -> Result<SchemaMetadata> {
        let columns = column_types
            .iter()
            .enumerate()
            .map(|(index, column_type)| SchemaColumn {
                column_id: ColumnId::new(index as u64 + 1),
                type_code: column_type.tag() as u32,
            })
            .collect();
        SchemaMetadata::new(schema_id, version, columns)
    }

    /// Infers column types from rows: the first non-NULL field per position
    /// wins, all-NULL positions stay [`ColumnType::Null`].
    #[must_use]
    pub fn infer_column_types(rows: &[Row]) -> Vec<ColumnType> {
        let width = rows.iter().map(|row| row.fields().len()).max().unwrap_or(0);
        let mut types = vec![ColumnType::Null; width];
        for row in rows {
            for (index, field) in row.fields().iter().enumerate() {
                if types[index] == ColumnType::Null {
                    let candidate = ColumnType::from_field(field);
                    if candidate != ColumnType::Null {
                        types[index] = candidate;
                    }
                }
            }
        }
        types
    }

    /// Reads the committed snapshot of rows over the half-open key range
    /// `[start, end)`, decoding each entry through `decode`.
    ///
    /// This is the range-scoped form of [`Self::snapshot_hot_rows`]. SQL tables
    /// live under `"{table}:{row_id}"` keys rather than the Hot Row Store
    /// prefix, so a table-scoped materialization passes the table's own key
    /// range and the SQL row decoder. [`StorageEngine::scan`] observes the last
    /// committed MVCC state, so the result is one consistent snapshot rather
    /// than a mixture of uncommitted versions, and rows decode in key order, so
    /// segment row order is deterministic.
    ///
    /// `decode` receives the key and the value, and returns `Ok(None)` for an
    /// entry that must not become a row (a superseded version, a tombstone, or
    /// a key shape this range is not meant to cover). Skipping is therefore
    /// decided by the caller's existing decoding rules rather than by a second
    /// visibility engine.
    pub fn snapshot_rows_in_range<E, F>(
        engine: &mut E,
        start: &[u8],
        end: &[u8],
        decode: F,
    ) -> Result<Vec<Row>>
    where
        E: StorageEngine,
        F: Fn(&[u8], &[u8]) -> Result<Option<Row>>,
    {
        let mut rows = Vec::new();
        for (key, value) in engine.scan(Some(start), Some(end))? {
            if let Some(row) = decode(&key, &value)? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// Reads the committed snapshot of Hot Row Store rows.
    ///
    /// [`StorageEngine::scan`] observes the last committed MVCC state, so a
    /// flush always materializes one consistent snapshot rather than a mixture
    /// of uncommitted versions. Only keys carrying the Hot Row Store prefix
    /// participate; columnar payload slices stored by earlier flushes never
    /// re-enter a segment. Rows decode in key order, so segment row order is
    /// deterministic.
    pub fn snapshot_hot_rows<E>(engine: &mut E) -> Result<Vec<Row>>
    where
        E: StorageEngine,
    {
        let prefix = hot_row_prefix();
        let end = prefix_end(&prefix);
        Self::snapshot_rows_in_range(engine, &prefix, &end, |key, payload| {
            // Only the 12-byte data keys participate. The row-id watermark
            // carries a different prefix and is outside the range already;
            // the length check keeps any other key shape out too.
            if key.len() != 12 {
                return Ok(None);
            }
            decode_hot_row(payload)
        })
    }

    /// Flushes Hot Row Store rows visible to the committed snapshot.
    pub fn flush_hot_rows<E>(
        &self,
        engine: &mut E,
        column_types: &[ColumnType],
        config: &FlushConfig,
        publish: &ColumnarPublish,
        fail_at: ColumnarFailPoint,
    ) -> Result<PublishedColumnarSegment>
    where
        E: StorageEngine,
    {
        let prefix = hot_row_prefix();
        let end = prefix_end(&prefix);
        self.flush_rows_in_range(
            engine,
            &prefix,
            &end,
            |key, payload| {
                if key.len() != 12 {
                    return Ok(None);
                }
                decode_hot_row(payload)
            },
            column_types,
            config,
            publish,
            fail_at,
        )
    }

    /// Flushes the rows of a key range visible to the committed snapshot.
    ///
    /// The range-scoped form of [`Self::flush_hot_rows`], used to materialize
    /// one SQL table into an immutable generation. The caller supplies the
    /// table's key range and the SQL row decoder, so a flush selects exactly
    /// that table's committed rows. Everything after materialization is the
    /// single existing pipeline: build, stage, verify, sync, publish.
    #[allow(clippy::too_many_arguments)]
    pub fn flush_rows_in_range<E, F>(
        &self,
        engine: &mut E,
        start: &[u8],
        end: &[u8],
        decode: F,
        column_types: &[ColumnType],
        config: &FlushConfig,
        publish: &ColumnarPublish,
        fail_at: ColumnarFailPoint,
    ) -> Result<PublishedColumnarSegment>
    where
        E: StorageEngine,
        F: Fn(&[u8], &[u8]) -> Result<Option<Row>>,
    {
        let rows = Self::snapshot_rows_in_range(engine, start, end, decode)?;
        let segment_id = self.allocate_segment_id();
        self.flush_rows_with_id(
            engine,
            &rows,
            column_types,
            segment_id,
            config,
            publish,
            fail_at,
        )
    }

    /// Flushes an explicit row set under a caller-chosen identity.
    ///
    /// Exists for tests and callers holding a consistent snapshot already;
    /// production flushes use [`Self::flush_hot_rows`].
    // Eight inputs travel together (engine/rows/types/identity/config/
    // publish/fail-point); bundling would churn every caller and test.
    #[allow(clippy::too_many_arguments)]
    pub fn flush_rows_with_id<E>(
        &self,
        engine: &mut E,
        rows: &[Row],
        column_types: &[ColumnType],
        segment_id: SegmentId,
        config: &FlushConfig,
        publish: &ColumnarPublish,
        fail_at: ColumnarFailPoint,
    ) -> Result<PublishedColumnarSegment>
    where
        E: StorageEngine,
    {
        let flushed: FlushedSegment =
            flush(rows, column_types, publish.generation, segment_id, config)?;
        if fail_at == ColumnarFailPoint::AfterBuild {
            return Err(injected("after columnar build"));
        }
        let slices = split_slices(&flushed.bytes);
        let manifest = encode_manifest(
            segment_id,
            flushed.bytes.len() as u64,
            slices.len() as u32,
            flushed.header.row_count,
            flushed.header.column_count,
            &flushed.bytes,
        );
        let mut txn = engine.begin()?;
        txn.put(&manifest_key(segment_id), &manifest)?;
        for (index, slice) in slices.iter().enumerate() {
            txn.put(&slice_key(segment_id, index as u32), slice)?;
        }
        if fail_at == ColumnarFailPoint::AfterFlush {
            return Err(injected("after columnar flush"));
        }
        if fail_at == ColumnarFailPoint::DuringVerify {
            return Err(injected("during columnar verify"));
        }
        let staged = join_slices(&slices);
        verify_staged(&staged, &flushed)?;
        if fail_at == ColumnarFailPoint::BeforeSync {
            return Err(injected("before columnar sync"));
        }
        // SYNC + commit: the transaction commit durably establishes the WAL
        // record and syncs storage, which is the durability boundary for the
        // staged payload.
        txn.commit()?;
        drop(txn);
        if fail_at == ColumnarFailPoint::AfterSync {
            return Err(injected("after columnar sync"));
        }
        if fail_at == ColumnarFailPoint::DuringPublish {
            return Err(injected("during columnar publish"));
        }
        let references = slice_references(
            publish.object_id,
            publish.generation,
            segment_id,
            slices.len(),
        );
        let change = ObjectChange::with_identity(
            publish.object_id,
            publish.table_identity,
            publish.schema.clone(),
            publish.generation,
            references,
        )?;
        let request = PublicationRequest::write(
            publish.storage_generation,
            publish.checkpoint_lsn,
            vec![change],
        )?;
        self.generations.publish(request)?;
        if fail_at == ColumnarFailPoint::AfterPublish {
            return Err(injected("after columnar publish"));
        }
        Ok(PublishedColumnarSegment {
            segment_id,
            generation_id: publish.generation,
            header: flushed.header,
            slice_count: slices.len() as u32,
            bytes: flushed.bytes,
        })
    }

    /// Loads a published segment's exact image bytes from durable storage.
    pub fn read_segment<E>(&self, engine: &mut E, segment_id: SegmentId) -> Result<Vec<u8>>
    where
        E: StorageEngine,
    {
        // Segment identities and slice ordinals are already encoded in the
        // authoritative storage keys. Read only this segment's manifest and
        // slices instead of scanning unrelated Hot Row Store and columnar
        // keys on every SQL query.
        let manifest = engine
            .get(&manifest_key(segment_id))?
            .ok_or_else(|| corruption("columnar segment manifest is absent"))?;
        let expected = decode_manifest(&manifest, segment_id)?;
        let mut slices = Vec::with_capacity(expected.slice_count as usize);
        for index in 0..expected.slice_count {
            let value = engine
                .get(&slice_key(segment_id, index))?
                .ok_or_else(|| corruption("columnar segment slice is absent"))?;
            slices.push((index, value));
        }
        finish_read(manifest, slices, segment_id)
    }

    /// Lists the segment identities that have manifests in storage.
    pub fn list_segments<E>(&self, engine: &mut E) -> Result<Vec<SegmentId>>
    where
        E: StorageEngine,
    {
        let mut ids = Vec::new();
        for (key, _) in engine.scan(None, None)? {
            if key.len() == 13
                && key[..4] == COLUMNAR_KEY_PREFIX
                && key[12] == COLUMNAR_MANIFEST_KIND
            {
                let id = u64::from_be_bytes(
                    key[4..12]
                        .try_into()
                        .map_err(|_| corruption("columnar key segment ID is truncated"))?,
                );
                ids.push(SegmentId::new(id));
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Deletes one persisted segment's manifest and payload slices.
    ///
    /// This is the data half of generation reclamation: the metadata half
    /// ([`GenerationManager::gc`]) deletes generation files only after
    /// proving them unreachable, and the caller deletes slices only for
    /// generations that pass reported as reclaimed. Crash ordering: the
    /// catalog dereference is already durable when this runs, so an
    /// interruption only leaves orphaned slice keys behind (invisible: no
    /// metadata references them) and never removes data a reader can reach.
    /// A missing manifest means the segment is already gone; that is
    /// reported, not an error, so repeated runs are idempotent.
    pub fn delete_segment<E>(&self, engine: &mut E, segment_id: SegmentId) -> Result<DeletedSegment>
    where
        E: StorageEngine,
    {
        let Some(manifest) = engine.get(&manifest_key(segment_id))? else {
            return Ok(DeletedSegment::already_gone());
        };
        let expected = decode_manifest(&manifest, segment_id)?;
        let slice_keys: Vec<Vec<u8>> = (0..expected.slice_count)
            .map(|index| slice_key(segment_id, index))
            .collect();
        let values = engine.get_many(&slice_keys)?;
        let mut bytes: u64 = manifest.len() as u64;
        for value in values.iter().flatten() {
            bytes = bytes.saturating_add(value.len() as u64);
        }
        let mut txn = engine.begin()?;
        txn.delete(&manifest_key(segment_id))?;
        for key in &slice_keys {
            txn.delete(key)?;
        }
        txn.commit()?;
        Ok(DeletedSegment {
            slices_removed: expected.slice_count,
            bytes_removed: bytes,
            manifest_absent: false,
        })
    }

    /// Returns the storage root this store was opened for.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

/// Outcome of deleting one persisted columnar segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeletedSegment {
    /// Payload slices removed (manifest excluded).
    pub slices_removed: u32,
    /// Manifest plus payload bytes removed, as measured before deletion.
    pub bytes_removed: u64,
    /// True when the manifest was already absent (nothing was deleted).
    pub manifest_absent: bool,
}

impl DeletedSegment {
    fn already_gone() -> Self {
        Self {
            slices_removed: 0,
            bytes_removed: 0,
            manifest_absent: true,
        }
    }
}

/// Builds a [`ColumnarPublish`] with schema derived from flushed types.
pub fn publish_for(
    object_id: ObjectId,
    schema_id: SchemaId,
    schema_version: CatalogVersion,
    column_types: &[ColumnType],
    generation: GenerationId,
    storage_generation: GenerationId,
    checkpoint_lsn: Lsn,
) -> Result<ColumnarPublish> {
    Ok(ColumnarPublish::new(
        object_id,
        ColumnarStore::columnar_schema(schema_id, schema_version, column_types)?,
        generation,
        storage_generation,
        checkpoint_lsn,
    ))
}

/// Decoded segment manifest.
struct ManifestView {
    total_len: u64,
    slice_count: u32,
    checksum: u32,
}

/// Splits segment bytes into fixed-size persisted slices.
fn split_slices(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .chunks(COLUMNAR_STORAGE_SLICE)
        .map(<[u8]>::to_vec)
        .collect()
}

/// Joins staged slices back into one image.
fn join_slices(slices: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = slices.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for slice in slices {
        out.extend_from_slice(slice);
    }
    out
}

/// Verifies a staged image against the in-memory build it came from.
fn verify_staged(staged: &[u8], flushed: &FlushedSegment) -> Result<SegmentReader> {
    let reader = SegmentReader::decode(staged)?;
    if reader.row_count != flushed.header.row_count {
        return Err(corruption("staged segment row count disagrees with build"));
    }
    if reader.columns.len() != flushed.segment.column_count() {
        return Err(corruption(
            "staged segment column count disagrees with build",
        ));
    }
    Ok(reader)
}

/// Encodes the manifest key for `segment_id`.
fn manifest_key(segment_id: SegmentId) -> Vec<u8> {
    let mut key = Vec::with_capacity(13);
    key.extend_from_slice(&COLUMNAR_KEY_PREFIX);
    key.extend_from_slice(&segment_id.get().to_be_bytes());
    key.push(COLUMNAR_MANIFEST_KIND);
    key
}

/// Encodes the payload-slice key for (`segment_id`, `index`).
fn slice_key(segment_id: SegmentId, index: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(17);
    key.extend_from_slice(&COLUMNAR_KEY_PREFIX);
    key.extend_from_slice(&segment_id.get().to_be_bytes());
    key.push(COLUMNAR_SLICE_KIND);
    key.extend_from_slice(&index.to_be_bytes());
    key
}

/// Encodes the manifest value describing one persisted segment.
fn encode_manifest(
    segment_id: SegmentId,
    total_len: u64,
    slice_count: u32,
    row_count: u64,
    column_count: u32,
    bytes: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(COLUMNAR_MANIFEST_LEN);
    out.extend_from_slice(&COLUMNAR_MANIFEST_MAGIC);
    out.extend_from_slice(&COLUMNAR_MANIFEST_VERSION.to_le_bytes());
    out.extend_from_slice(&segment_id.get().to_le_bytes());
    out.extend_from_slice(&total_len.to_le_bytes());
    out.extend_from_slice(&slice_count.to_le_bytes());
    out.extend_from_slice(&row_count.to_le_bytes());
    out.extend_from_slice(&column_count.to_le_bytes());
    out.extend_from_slice(&plomid_storage::compute_checksum(bytes).to_le_bytes());
    debug_assert_eq!(out.len(), COLUMNAR_MANIFEST_LEN);
    out
}

/// Decodes and validates a manifest value for `segment_id`.
fn decode_manifest(bytes: &[u8], segment_id: SegmentId) -> Result<ManifestView> {
    if bytes.len() != COLUMNAR_MANIFEST_LEN {
        return Err(corruption("columnar manifest has an invalid length"));
    }
    if bytes[..4] != COLUMNAR_MANIFEST_MAGIC {
        return Err(corruption("columnar manifest magic does not match"));
    }
    let version = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| corruption("columnar manifest version is truncated"))?,
    );
    if version != COLUMNAR_MANIFEST_VERSION {
        return Err(invalid(format!(
            "unsupported columnar manifest version {version}"
        )));
    }
    let id = u64::from_le_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| corruption("columnar manifest identity is truncated"))?,
    );
    if id != segment_id.get() {
        return Err(corruption("columnar manifest names a different segment"));
    }
    let total_len = u64::from_le_bytes(
        bytes[16..24]
            .try_into()
            .map_err(|_| corruption("columnar manifest length is truncated"))?,
    );
    let slice_count = u32::from_le_bytes(
        bytes[24..28]
            .try_into()
            .map_err(|_| corruption("columnar manifest slice count is truncated"))?,
    );
    if slice_count == 0 {
        return Err(corruption("columnar manifest claims zero slices"));
    }
    Ok(ManifestView {
        total_len,
        slice_count,
        checksum: u32::from_le_bytes(
            bytes[40..44]
                .try_into()
                .map_err(|_| corruption("columnar manifest checksum is truncated"))?,
        ),
    })
}

/// Builds one generation reference per payload slice.
///
/// Positions are deterministic in the existing
/// segment → pack → block → page naming scheme: sixteen slices share a
/// block, each slice names its page. The manifest needs no reference; it is
/// re-derived from the segment identity.
fn slice_references(
    object_id: ObjectId,
    generation: GenerationId,
    segment_id: SegmentId,
    slice_count: usize,
) -> Vec<PhysicalReference> {
    (0..slice_count)
        .map(|index| {
            PhysicalReference::new(
                object_id,
                generation,
                PhysicalStructure::new(
                    segment_id,
                    plomid_core::PackId::new(1),
                    BlockId::new(index as u64 / 16 + 1),
                    PageId::new(index as u64 + 1),
                    None,
                ),
            )
        })
        .collect()
}

/// Reassembles and integrity-checks one segment image from its manifest.
fn finish_read(
    manifest: Vec<u8>,
    mut slices: Vec<(u32, Vec<u8>)>,
    segment_id: SegmentId,
) -> Result<Vec<u8>> {
    let expected = decode_manifest(&manifest, segment_id)?;
    slices.sort_by_key(|(index, _)| *index);
    if slices.len() as u32 != expected.slice_count {
        return Err(corruption(
            "columnar slice count disagrees with its manifest",
        ));
    }
    for (position, (index, _)) in slices.iter().enumerate() {
        if *index != position as u32 {
            return Err(corruption("columnar slices are not contiguous"));
        }
    }
    let bytes: Vec<u8> = slices.into_iter().flat_map(|(_, data)| data).collect();
    if bytes.len() as u64 != expected.total_len {
        return Err(corruption("columnar length disagrees with its manifest"));
    }
    if plomid_storage::compute_checksum(&bytes) != expected.checksum {
        return Err(corruption("columnar segment checksum mismatch"));
    }
    Ok(bytes)
}

/// Returns the Hot Row Store key prefix via the public key function.
fn hot_row_prefix() -> [u8; 4] {
    let key = plomid_txn::row_key(plomid_core::RowId::new(0));
    [key[0], key[1], key[2], key[3]]
}

/// Exclusive upper bound of every key that starts with `prefix`.
///
/// Bytewise, incrementing the last byte that is not `0xFF` yields the smallest
/// key that sorts strictly after every key carrying `prefix`, so a range scan
/// `[prefix, end)` selects exactly the keys under that prefix and nothing else.
/// A prefix of all `0xFF` has no upper bound and yields an empty bound, which
/// callers treat as "to the end of the key space".
fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < u8::MAX {
            end.push(last + 1);
            return end;
        }
    }
    end
}

/// Decodes one Hot Row Store payload, tolerating the MVCC envelope that the
/// versioned commit path may have left on recovered rows.
fn decode_hot_row(payload: &[u8]) -> Result<Option<Row>> {
    if let Ok(row) = Row::decode(payload) {
        return Ok(Some(row));
    }
    if plomid_mvcc::is_versioned(payload) {
        if let Some(version) = plomid_mvcc::decode_versioned(payload) {
            match version.payload {
                Some(raw) => return Row::decode(&raw).map(Some),
                None => return Ok(None),
            }
        }
    }
    Err(corruption("hot row payload is not a decodable row"))
}

/// Builds an injected-failure error for fail-point tests.
fn injected(what: &str) -> plomid_core::PlomidError {
    plomid_core::PlomidError::new(
        plomid_core::ErrorKind::Internal,
        format!("injected columnar failure {what}"),
    )
}
