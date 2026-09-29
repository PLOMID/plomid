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
//! Crash-safe checkpoint creation and startup recovery for PLOMID.
//!
//! A checkpoint is a small versioned file that pins a durable recovery
//! boundary for the storage root:
//!
//! ```text
//! durable storage
//!       -> checkpoint BUILD -> FLUSH -> VERIFY -> SYNC -> PUBLISH
//!       -> process failure -> open -> physical validation
//!       -> latest valid checkpoint -> required WAL replay -> READY
//! ```
//!
//! # Storage layout
//!
//! All checkpoint state lives under the storage root:
//!
//! ```text
//! <root>/checkpoints/checkpoint-<generation:020>.ckpt
//! ```
//!
//! Files are never overwritten in place. Creation writes a sibling temporary
//! file (`<final>.tmp`), flushes it, reads it back and validates it (VERIFY),
//! fsyncs it (SYNC), then atomically renames it to its final name (PUBLISH)
//! and fsyncs the parent directory. A crash before the rename leaves only a
//! `<final>.tmp` artifact, which discovery ignores, so the previous valid
//! checkpoint remains usable.
//!
//! # Binary format
//!
//! All integers are little-endian. The 64-byte header is followed by exactly
//! `metadata_len` opaque metadata bytes; no trailing data is permitted:
//!
//! ```text
//! magic[4] = PLCK | version[u32] = 1 | header_len[u32] = 64
//! checkpoint_generation[u64] | checkpoint_lsn[u64]
//! storage_generation[u64] | catalog_generation[u64]
//! metadata_len[u32] | checksum[u32] | reserved[12] = 0
//! metadata[metadata_len]
//! ```
//!
//! The checksum is CRC32C over `header[0..48]` followed by the metadata
//! bytes. The checksum field and reserved bytes are not covered. Readers
//! recompute and compare it before trusting any other field beyond
//! magic/version/length framing.
//!
//! Logical identifiers never encode paths, offsets, or platform information:
//! `checkpoint_generation` and `storage_generation` are [`GenerationId`],
//! `catalog_generation` is [`CatalogVersion`], and the WAL boundary is an
//! [`Lsn`].
//!
//! # Generation rules
//!
//! * `checkpoint_generation` is one plus the greatest valid checkpoint
//!   generation present at creation (1 when none exists); strictly monotonic.
//! * `storage_generation` / `catalog_generation` describe the durable state
//!   captured. A new checkpoint must not move `checkpoint_lsn`,
//!   `storage_generation`, or `catalog_generation` backwards relative to the
//!   latest valid checkpoint; doing so is rejected as stale.
//! * A checkpoint whose `checkpoint_lsn` is beyond the WAL durability
//!   watermark known at recovery claims undurable state and is rejected as
//!   stale rather than used as a replay boundary.
//!
//! # WAL boundary
//!
//! `checkpoint_lsn` is the last WAL LSN already represented by the
//! checkpoint. Recovery replays only records with `lsn > checkpoint_lsn`
//! through the existing WAL reader and LSN ordering guarantees, so recovery
//! work is bounded by WAL written after the checkpoint, not by database size.
//!
//! # Synchronization
//!
//! Creation must be serialized with the WAL durability watermark and the
//! storage flush it captures: the caller must hold whatever lock protects
//! `append -> sync WAL -> flush storage` (for the V1 engine, exclusive `&mut`
//! access) across "observe durable LSN, flush storage, create checkpoint" so
//! that `checkpoint_lsn` always names state already represented in the
//! checkpoint file. This module allocates generations deterministically from
//! directory contents and performs no global locking of its own.

use crate::checksum::{crc_finalize, crc_init, crc_update};
use plomid_core::{CatalogVersion, ErrorKind, GenerationId, Lsn, PlomidError, Result};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Checkpoint constants are defined once in `plomid_core::constants` and
/// re-exported here; module-local aliases keep the body unchanged.
pub use plomid_core::{
    CHECKPOINT_CHECKSUMMED_PREFIX_LEN as CHECKSUMMED_PREFIX_LEN, CHECKPOINT_DIR_NAME,
    CHECKPOINT_FILE_PREFIX as FILE_PREFIX, CHECKPOINT_FILE_SUFFIX as FILE_SUFFIX,
    CHECKPOINT_FORMAT_VERSION, CHECKPOINT_GENERATION_DIGITS, CHECKPOINT_HEADER_SIZE,
    CHECKPOINT_MAGIC, CHECKPOINT_OFF_CATALOG_GEN as OFF_CATALOG_GEN,
    CHECKPOINT_OFF_CHECKSUM as OFF_CHECKSUM, CHECKPOINT_OFF_CKPT_GEN as OFF_CKPT_GEN,
    CHECKPOINT_OFF_CKPT_LSN as OFF_CKPT_LSN, CHECKPOINT_OFF_HEADER_LEN as OFF_HEADER_LEN,
    CHECKPOINT_OFF_META_LEN as OFF_META_LEN, CHECKPOINT_OFF_RESERVED as OFF_RESERVED,
    CHECKPOINT_OFF_STORAGE_GEN as OFF_STORAGE_GEN, CHECKPOINT_OFF_VERSION as OFF_VERSION,
    CHECKPOINT_TMP_SUFFIX as TMP_SUFFIX, MAX_METADATA_SIZE,
};

/// Durable checkpoint identity and recovery boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointMetadata {
    /// Strictly monotonic checkpoint generation allocated at creation.
    pub checkpoint_generation: GenerationId,
    /// Last WAL LSN already represented by the checkpointed state.
    pub checkpoint_lsn: Lsn,
    /// Durable storage generation captured by the checkpoint.
    pub storage_generation: GenerationId,
    /// Durable catalog version captured by the checkpoint.
    pub catalog_generation: CatalogVersion,
    /// Opaque caller metadata (for example allocator watermarks).
    pub metadata: Vec<u8>,
}

/// Caller-observed durable state to capture in a new checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointRequest {
    /// Durable WAL LSN already represented by the flushed storage state.
    pub checkpoint_lsn: Lsn,
    /// Durable storage generation of the flushed state.
    pub storage_generation: GenerationId,
    /// Durable catalog version of the flushed state.
    pub catalog_generation: CatalogVersion,
    /// Opaque metadata to persist with the checkpoint.
    pub metadata: Vec<u8>,
}

/// Phase-boundary timings of one checkpoint publication.
///
/// Each field measures one durability boundary of checkpoint creation. The
/// values are observability data only: they never affect the durability
/// guarantees of [`create_checkpoint`] and are never persisted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckpointPhaseTimings {
    /// BUILD: constructing and write-through of the complete checkpoint image.
    pub build: std::time::Duration,
    /// FLUSH: pushing staged bytes through the buffered I/O layer.
    pub flush: std::time::Duration,
    /// VERIFY: re-reading and validating the staged image.
    pub verify: std::time::Duration,
    /// SYNC: fsync of the staged file (and directory, for publication).
    pub sync: std::time::Duration,
    /// PUBLISH: atomic rename plus directory durability.
    pub publish: std::time::Duration,
}

/// Injection point for durability-boundary failure tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailPoint {
    /// No failure; normal production path.
    None,
    /// Fail before any checkpoint bytes are written.
    BeforeBuild,
    /// Fail after bytes are written but before flush.
    AfterBuild,
    /// Fail after flush but before verification.
    AfterFlush,
    /// Fail during verification.
    DuringVerify,
    /// Fail after verification but before fsync.
    BeforeSync,
    /// Fail after fsync but before atomic publication.
    AfterSync,
    /// Fail during atomic publication.
    DuringPublish,
}

fn corruption(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::Corruption, message)
}

fn invalid(message: impl Into<String>) -> PlomidError {
    PlomidError::new(ErrorKind::InvalidArgument, message)
}

fn checksum_of(prefix: &[u8], metadata: &[u8]) -> u32 {
    let mut crc = crc_init();
    crc = crc_update(crc, prefix);
    crc = crc_update(crc, metadata);
    crc_finalize(crc)
}

impl CheckpointMetadata {
    /// Encodes the checkpoint deterministically (header + metadata).
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.metadata.len() > MAX_METADATA_SIZE {
            return Err(invalid("checkpoint metadata is too large"));
        }
        let meta_len_u32 = u32::try_from(self.metadata.len())
            .map_err(|_| invalid("checkpoint metadata length overflow"))?;
        let total_len = CHECKPOINT_HEADER_SIZE
            .checked_add(self.metadata.len())
            .ok_or_else(|| invalid("checkpoint length overflow"))?;
        let mut bytes = vec![0_u8; total_len];
        bytes[0..4].copy_from_slice(&CHECKPOINT_MAGIC);
        bytes[OFF_VERSION..OFF_VERSION + 4]
            .copy_from_slice(&CHECKPOINT_FORMAT_VERSION.to_le_bytes());
        bytes[OFF_HEADER_LEN..OFF_HEADER_LEN + 4]
            .copy_from_slice(&(CHECKPOINT_HEADER_SIZE as u32).to_le_bytes());
        bytes[OFF_CKPT_GEN..OFF_CKPT_GEN + 8]
            .copy_from_slice(&self.checkpoint_generation.get().to_le_bytes());
        bytes[OFF_CKPT_LSN..OFF_CKPT_LSN + 8]
            .copy_from_slice(&self.checkpoint_lsn.get().to_le_bytes());
        bytes[OFF_STORAGE_GEN..OFF_STORAGE_GEN + 8]
            .copy_from_slice(&self.storage_generation.get().to_le_bytes());
        bytes[OFF_CATALOG_GEN..OFF_CATALOG_GEN + 8]
            .copy_from_slice(&self.catalog_generation.get().to_le_bytes());
        bytes[OFF_META_LEN..OFF_META_LEN + 4].copy_from_slice(&meta_len_u32.to_le_bytes());
        let checksum = checksum_of(&bytes[..CHECKSUMMED_PREFIX_LEN], &self.metadata);
        bytes[OFF_CHECKSUM..OFF_CHECKSUM + 4].copy_from_slice(&checksum.to_le_bytes());
        bytes[OFF_RESERVED..CHECKPOINT_HEADER_SIZE].fill(0);
        bytes[CHECKPOINT_HEADER_SIZE..].copy_from_slice(&self.metadata);
        Ok(bytes)
    }
}

impl CheckpointMetadata {
    /// Decodes and fully validates one checkpoint image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < CHECKPOINT_HEADER_SIZE {
            return Err(corruption("checkpoint header is truncated"));
        }
        if bytes[0..4] != CHECKPOINT_MAGIC {
            return Err(corruption("invalid checkpoint magic"));
        }
        let version = u32::from_le_bytes(
            bytes[OFF_VERSION..OFF_VERSION + 4]
                .try_into()
                .map_err(|_| corruption("checkpoint version is truncated"))?,
        );
        if version != CHECKPOINT_FORMAT_VERSION {
            if version > CHECKPOINT_FORMAT_VERSION {
                return Err(PlomidError::new(
                    ErrorKind::Unsupported,
                    "unsupported checkpoint version",
                ));
            }
            return Err(corruption("unsupported checkpoint version"));
        }
        let header_len = u32::from_le_bytes(
            bytes[OFF_HEADER_LEN..OFF_HEADER_LEN + 4]
                .try_into()
                .map_err(|_| corruption("checkpoint header length is truncated"))?,
        ) as usize;
        if header_len != CHECKPOINT_HEADER_SIZE {
            return Err(corruption("checkpoint header length is invalid"));
        }
        let ckpt_gen = u64::from_le_bytes(
            bytes[OFF_CKPT_GEN..OFF_CKPT_GEN + 8]
                .try_into()
                .map_err(|_| corruption("checkpoint generation is truncated"))?,
        );
        let ckpt_lsn = u64::from_le_bytes(
            bytes[OFF_CKPT_LSN..OFF_CKPT_LSN + 8]
                .try_into()
                .map_err(|_| corruption("checkpoint LSN is truncated"))?,
        );
        let storage_gen = u64::from_le_bytes(
            bytes[OFF_STORAGE_GEN..OFF_STORAGE_GEN + 8]
                .try_into()
                .map_err(|_| corruption("checkpoint storage generation is truncated"))?,
        );
        let catalog_gen = u64::from_le_bytes(
            bytes[OFF_CATALOG_GEN..OFF_CATALOG_GEN + 8]
                .try_into()
                .map_err(|_| corruption("checkpoint catalog generation is truncated"))?,
        );
        let metadata_len = u32::from_le_bytes(
            bytes[OFF_META_LEN..OFF_META_LEN + 4]
                .try_into()
                .map_err(|_| corruption("checkpoint metadata length is truncated"))?,
        ) as usize;
        if metadata_len > MAX_METADATA_SIZE {
            return Err(corruption("checkpoint metadata length is invalid"));
        }
        let expected_len = CHECKPOINT_HEADER_SIZE
            .checked_add(metadata_len)
            .ok_or_else(|| corruption("checkpoint length overflow"))?;
        if bytes.len() < expected_len {
            return Err(corruption("checkpoint metadata is truncated"));
        }
        if bytes.len() != expected_len {
            return Err(corruption("checkpoint has unexpected trailing data"));
        }
        Self::decode_validated(
            ckpt_gen,
            ckpt_lsn,
            storage_gen,
            catalog_gen,
            bytes,
            expected_len,
        )
    }

    fn decode_validated(
        ckpt_gen: u64,
        ckpt_lsn: u64,
        storage_gen: u64,
        catalog_gen: u64,
        bytes: &[u8],
        expected_len: usize,
    ) -> Result<Self> {
        if bytes[OFF_RESERVED..CHECKPOINT_HEADER_SIZE]
            .iter()
            .any(|b| *b != 0)
        {
            return Err(corruption("checkpoint has non-zero reserved bytes"));
        }
        if ckpt_gen == 0 || ckpt_gen == u64::MAX {
            return Err(corruption("checkpoint has an impossible generation"));
        }
        if storage_gen == 0 || storage_gen == u64::MAX {
            return Err(corruption(
                "checkpoint has an impossible storage generation",
            ));
        }
        if catalog_gen == u64::MAX {
            return Err(corruption(
                "checkpoint has an impossible catalog generation",
            ));
        }
        if ckpt_lsn == u64::MAX {
            return Err(corruption("checkpoint has an invalid LSN"));
        }
        let expected = u32::from_le_bytes(
            bytes[OFF_CHECKSUM..OFF_CHECKSUM + 4]
                .try_into()
                .map_err(|_| corruption("checkpoint checksum is truncated"))?,
        );
        let actual = checksum_of(
            &bytes[..CHECKSUMMED_PREFIX_LEN],
            &bytes[CHECKPOINT_HEADER_SIZE..expected_len],
        );
        if actual != expected {
            return Err(corruption(format!(
                "checkpoint checksum mismatch (expected {expected:#010x}, got {actual:#010x})"
            )));
        }
        Ok(Self {
            checkpoint_generation: GenerationId::new(ckpt_gen),
            checkpoint_lsn: Lsn::new(ckpt_lsn),
            storage_generation: GenerationId::new(storage_gen),
            catalog_generation: CatalogVersion::new(catalog_gen),
            metadata: bytes[CHECKPOINT_HEADER_SIZE..expected_len].to_vec(),
        })
    }
}

impl CheckpointMetadata {
    /// Rejects candidates that move durable state backwards.
    pub fn check_not_stale_against(&self, previous: &CheckpointMetadata) -> Result<()> {
        if self.checkpoint_generation.get() <= previous.checkpoint_generation.get() {
            return Err(corruption("stale checkpoint generation"));
        }
        if self.checkpoint_lsn.get() < previous.checkpoint_lsn.get() {
            return Err(corruption("stale checkpoint LSN"));
        }
        if self.storage_generation.get() < previous.storage_generation.get() {
            return Err(corruption("stale checkpoint storage generation"));
        }
        if self.catalog_generation.get() < previous.catalog_generation.get() {
            return Err(corruption("stale checkpoint catalog generation"));
        }
        Ok(())
    }

    /// Rejects checkpoints claiming LSNs beyond the durable WAL watermark.
    pub fn check_durable(&self, durable_lsn: Lsn) -> Result<()> {
        if self.checkpoint_lsn.get() > durable_lsn.get() {
            return Err(corruption("checkpoint LSN is beyond the durable WAL"));
        }
        Ok(())
    }
}

/// Returns the checkpoint directory for a storage root, creating it.
fn checkpoint_dir(root: &Path) -> Result<PathBuf> {
    let dir = root.join(CHECKPOINT_DIR_NAME);
    fs::create_dir_all(&dir).map_err(PlomidError::from)?;
    Ok(dir)
}

/// Deterministic published file name for a checkpoint generation.
fn file_name_for(generation: u64) -> String {
    format!("{FILE_PREFIX}{generation:020}{FILE_SUFFIX}")
}

/// Parses a published checkpoint file name into its generation.
///
/// Only the deterministic published form is accepted; temporary artifacts and
/// unrelated files return `None` so discovery never treats them as candidates.
/// The generation must be zero-padded to exactly `CHECKPOINT_GENERATION_DIGITS`
/// digits and must not be zero, so a half-written `.tmp` artifact can never be
/// mistaken for a published checkpoint.
#[must_use]
pub fn generation_from_file_name(name: &str) -> Option<u64> {
    let number = name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;
    if number.len() != CHECKPOINT_GENERATION_DIGITS || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let generation = number.parse::<u64>().ok()?;
    if generation == 0 {
        return None;
    }
    Some(generation)
}

/// Parses a published checkpoint file name into its generation.
fn parse_file_name(name: &str) -> Option<u64> {
    generation_from_file_name(name)
}

/// Enumerates candidate checkpoint files in deterministic generation order.
pub fn discover(root: &Path) -> Result<Vec<PathBuf>> {
    let dir = root.join(CHECKPOINT_DIR_NAME);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&dir).map_err(PlomidError::from)? {
        let entry = entry.map_err(PlomidError::from)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(generation) = parse_file_name(&name) {
            candidates.push((generation, entry.path()));
        }
    }
    candidates.sort_unstable_by_key(|(generation, _)| *generation);
    Ok(candidates.into_iter().map(|(_, path)| path).collect())
}

/// Reads and validates one checkpoint file.
pub fn load_checkpoint(path: &Path) -> Result<CheckpointMetadata> {
    let bytes = fs::read(path).map_err(PlomidError::from)?;
    CheckpointMetadata::decode(&bytes)
}

/// Returns the newest valid checkpoint, falling back past corrupt candidates.
pub fn latest_valid(root: &Path) -> Result<Option<(PathBuf, CheckpointMetadata)>> {
    let candidates = discover(root)?;
    let mut ordered: Vec<(u64, PathBuf)> = Vec::with_capacity(candidates.len());
    for path in candidates {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Some(generation) = parse_file_name(name) {
                ordered.push((generation, path));
            }
        }
    }
    ordered.sort_unstable_by_key(|(generation, _)| std::cmp::Reverse(*generation));
    for (generation, path) in ordered {
        match load_checkpoint(&path) {
            Ok(metadata) => {
                if metadata.checkpoint_generation.get() != generation {
                    continue;
                }
                return Ok(Some((path, metadata)));
            }
            Err(_) => continue,
        }
    }
    Ok(None)
}

/// Creates and publishes a checkpoint: BUILD -> FLUSH -> VERIFY -> SYNC ->
/// PUBLISH. Success means the new checkpoint is discoverable; any failure
/// before publication leaves the previous checkpoint intact.
pub fn create_checkpoint(root: &Path, request: CheckpointRequest) -> Result<CheckpointMetadata> {
    create_checkpoint_inner(root, request, FailPoint::None, None)
}

/// Creates and publishes a checkpoint, reporting phase-boundary timings.
///
/// Durability semantics are identical to [`create_checkpoint`]; the timings
/// describe how long each durability boundary took and are not persisted.
pub fn create_checkpoint_with_timings(
    root: &Path,
    request: CheckpointRequest,
) -> Result<(CheckpointMetadata, CheckpointPhaseTimings)> {
    let mut timings = CheckpointPhaseTimings::default();
    let metadata = create_checkpoint_inner(root, request, FailPoint::None, Some(&mut timings))?;
    Ok((metadata, timings))
}

/// Failure-injection entry point for durability-boundary tests.
pub fn create_checkpoint_with_fail_point(
    root: &Path,
    request: CheckpointRequest,
    fail_at: FailPoint,
) -> Result<CheckpointMetadata> {
    create_checkpoint_inner(root, request, fail_at, None)
}

fn create_checkpoint_inner(
    root: &Path,
    request: CheckpointRequest,
    fail_at: FailPoint,
    mut timings: Option<&mut CheckpointPhaseTimings>,
) -> Result<CheckpointMetadata> {
    validate_request(&request)?;
    if fail_at == FailPoint::BeforeBuild {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure before checkpoint build",
        ));
    }
    let dir = checkpoint_dir(root)?;
    let previous = latest_valid(root)?;
    check_request_not_stale(&request, previous.as_ref().map(|(_, m)| m))?;
    let next_generation = previous
        .as_ref()
        .map(|(_, m)| m.checkpoint_generation.get())
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid("checkpoint generation exhausted"))?;
    if next_generation == 0 || next_generation == u64::MAX {
        return Err(invalid("checkpoint generation exhausted"));
    }
    let metadata = CheckpointMetadata {
        checkpoint_generation: GenerationId::new(next_generation),
        checkpoint_lsn: request.checkpoint_lsn,
        storage_generation: request.storage_generation,
        catalog_generation: request.catalog_generation,
        metadata: request.metadata,
    };
    if let Some((_, ref prev)) = previous {
        metadata.check_not_stale_against(prev)?;
    }
    // BUILD: encode the complete image into a new temporary file.
    let build_start = std::time::Instant::now();
    let bytes = metadata.encode()?;
    let final_path = dir.join(file_name_for(next_generation));
    let tmp_path = dir.join(format!("{}{}", file_name_for(next_generation), TMP_SUFFIX));
    let _ = fs::remove_file(&tmp_path);
    build_and_publish(
        &tmp_path,
        &final_path,
        &dir,
        &bytes,
        &metadata,
        fail_at,
        build_start,
        &mut timings,
    )?;
    let published = load_checkpoint(&final_path)?;
    if published != metadata {
        return Err(corruption("published checkpoint mismatch"));
    }
    Ok(metadata)
}

fn validate_request(request: &CheckpointRequest) -> Result<()> {
    if request.metadata.len() > MAX_METADATA_SIZE {
        return Err(invalid("checkpoint metadata is too large"));
    }
    if request.storage_generation.is_zero() || request.storage_generation.get() == u64::MAX {
        return Err(invalid("checkpoint storage generation is impossible"));
    }
    if request.catalog_generation.get() == u64::MAX {
        return Err(invalid("checkpoint catalog generation is impossible"));
    }
    if request.checkpoint_lsn.get() == u64::MAX {
        return Err(invalid("checkpoint LSN is invalid"));
    }
    Ok(())
}

fn check_request_not_stale(
    request: &CheckpointRequest,
    previous: Option<&CheckpointMetadata>,
) -> Result<()> {
    if let Some(prev) = previous {
        let moves_backward = request.checkpoint_lsn.get() < prev.checkpoint_lsn.get()
            || request.storage_generation.get() < prev.storage_generation.get()
            || request.catalog_generation.get() < prev.catalog_generation.get();
        let advances = request.checkpoint_lsn.get() > prev.checkpoint_lsn.get()
            || request.storage_generation.get() > prev.storage_generation.get()
            || request.catalog_generation.get() > prev.catalog_generation.get();
        if moves_backward || !advances {
            return Err(corruption("stale checkpoint state"));
        }
    }
    Ok(())
}

/// All eight inputs are required at once (paths, bytes, metadata, failure
/// point, timings); bundling them into a struct would churn every caller
/// for no behavioral gain.
#[allow(clippy::too_many_arguments)]
fn build_and_publish(
    tmp_path: &Path,
    final_path: &Path,
    dir: &Path,
    bytes: &[u8],
    metadata: &CheckpointMetadata,
    fail_at: FailPoint,
    build_start: std::time::Instant,
    timings: &mut Option<&mut CheckpointPhaseTimings>,
) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(tmp_path)
        .map_err(PlomidError::from)?;
    let mut written = 0_usize;
    while written < bytes.len() {
        let count = file.write(&bytes[written..]).map_err(PlomidError::from)?;
        if count == 0 {
            return Err(PlomidError::new(
                ErrorKind::Io,
                "checkpoint file accepted no bytes",
            ));
        }
        written = written
            .checked_add(count)
            .ok_or_else(|| invalid("checkpoint write overflow"))?;
    }
    if fail_at == FailPoint::AfterBuild {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure after checkpoint build",
        ));
    }
    if let Some(record) = timings.as_deref_mut() {
        record.build = build_start.elapsed();
    }
    // FLUSH: push bytes through buffering; durability is not claimed here.
    let flush_start = std::time::Instant::now();
    file.flush().map_err(PlomidError::from)?;
    if let Some(record) = timings.as_deref_mut() {
        record.flush = flush_start.elapsed();
    }
    if fail_at == FailPoint::AfterFlush {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure after checkpoint flush",
        ));
    }
    // VERIFY: re-read the staged image and validate it before it can publish.
    let verify_start = std::time::Instant::now();
    file.seek(SeekFrom::Start(0)).map_err(PlomidError::from)?;
    let mut staged = vec![0_u8; bytes.len()];
    let mut read = 0_usize;
    while read < staged.len() {
        let count = file.read(&mut staged[read..]).map_err(PlomidError::from)?;
        if count == 0 {
            break;
        }
        read = read
            .checked_add(count)
            .ok_or_else(|| corruption("checkpoint read overflow"))?;
    }
    if read != bytes.len() || staged != bytes {
        return Err(corruption(
            "checkpoint verification found unstable contents",
        ));
    }
    drop(file);
    let verified = CheckpointMetadata::decode(&staged)?;
    if verified != *metadata {
        return Err(corruption("checkpoint verification mismatch"));
    }
    if fail_at == FailPoint::DuringVerify {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure during checkpoint verify",
        ));
    }
    if let Some(record) = timings.as_deref_mut() {
        record.verify = verify_start.elapsed();
    }
    // SYNC: file durability boundary before publication.
    if fail_at == FailPoint::BeforeSync {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure before checkpoint sync",
        ));
    }
    let sync_start = std::time::Instant::now();
    sync_file(tmp_path)?;
    if let Some(record) = timings.as_deref_mut() {
        record.sync = sync_start.elapsed();
    }
    if fail_at == FailPoint::AfterSync {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure after checkpoint sync",
        ));
    }
    // PUBLISH: atomic rename; previous checkpoint untouched until now.
    if fail_at == FailPoint::DuringPublish {
        return Err(PlomidError::new(
            ErrorKind::Io,
            "injected failure during checkpoint publish",
        ));
    }
    let publish_start = std::time::Instant::now();
    fs::rename(tmp_path, final_path).map_err(PlomidError::from)?;
    // Directory durability so the rename survives a crash.
    sync_dir(dir)?;
    if let Some(record) = timings.as_deref_mut() {
        record.publish = publish_start.elapsed();
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    let file = File::open(path).map_err(PlomidError::from)?;
    file.sync_all().map_err(PlomidError::from)?;
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    let file = File::open(dir).map_err(PlomidError::from)?;
    file.sync_all().map_err(PlomidError::from)?;
    Ok(())
}

/// Validates the physical storage environment before checkpoints are trusted.
///
/// Startup establishes that the storage image is structurally sound before any
/// checkpoint state is considered. Only structural metadata is inspected: the
/// volume registry, the segment manifest, the segment directory listing, and
/// the presence of at least one durable segment. No page, block, pack, or
/// record payload is read, so the cost is independent of database size.
///
/// WAL availability and WAL structural validity are validated by the WAL
/// layer, which owns the segment format.
///
/// Returns the durable storage generation of the validated image.
pub fn validate_storage_physical(root: &Path) -> Result<GenerationId> {
    if !root.exists() {
        return Err(PlomidError::with_detail(
            ErrorKind::NotFound,
            "storage root is missing",
            format!("root={}", root.display()),
        ));
    }
    if !root.is_dir() {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "storage root is not a directory",
            format!("root={}", root.display()),
        ));
    }
    let generation = crate::storage_generation(root)?;
    if generation.is_zero() {
        return Err(PlomidError::with_detail(
            ErrorKind::Corruption,
            "storage root has no durable segment",
            format!("root={}", root.display()),
        ));
    }
    Ok(generation)
}

// WAL structural validation and checkpoint-bounded replay live in the WAL
// layer, which owns the segment and record formats. This module deliberately
// contains no WAL decoding logic so that exactly one decoder exists in the
// workspace.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_is_deterministic_and_round_trips() {
        let meta = CheckpointMetadata {
            checkpoint_generation: GenerationId::new(7),
            checkpoint_lsn: Lsn::new(9),
            storage_generation: GenerationId::new(3),
            catalog_generation: CatalogVersion::new(4),
            metadata: vec![9, 8, 7],
        };
        assert_eq!(
            meta.encode().expect("encode"),
            meta.encode().expect("encode")
        );
        assert_eq!(
            CheckpointMetadata::decode(&meta.encode().expect("encode")).expect("decode"),
            meta
        );
    }

    #[test]
    fn rejects_corruption_matrix() {
        let valid = CheckpointMetadata {
            checkpoint_generation: GenerationId::new(1),
            checkpoint_lsn: Lsn::new(1),
            storage_generation: GenerationId::new(1),
            catalog_generation: CatalogVersion::new(0),
            metadata: vec![5, 6],
        }
        .encode()
        .expect("encode");
        let mut bad = valid.clone();
        bad[0] = b'X';
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        bad[OFF_VERSION] = 0xFF;
        assert!(CheckpointMetadata::decode(&bad).is_err());
        assert!(CheckpointMetadata::decode(&valid[..10]).is_err());
        assert!(CheckpointMetadata::decode(&valid[..CHECKPOINT_HEADER_SIZE + 1]).is_err());
        let mut bad = valid.clone();
        bad[OFF_META_LEN] = 0xFF;
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        bad[OFF_CHECKSUM] ^= 0xFF;
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        bad[OFF_RESERVED] = 1;
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        bad.push(0);
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        bad[OFF_CKPT_GEN..OFF_CKPT_GEN + 8].copy_from_slice(&0u64.to_le_bytes());
        assert!(CheckpointMetadata::decode(&bad).is_err());
        let mut bad = valid.clone();
        bad[OFF_CKPT_LSN..OFF_CKPT_LSN + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(CheckpointMetadata::decode(&bad).is_err());
    }
}

#[cfg(test)]
mod publish_tests {
    use super::super::*;
    use plomid_core::{CatalogVersion, GenerationId, Lsn, PlomidError};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("plomid-ckpt-{label}-{}-{id}", std::process::id()))
    }

    fn request(lsn: u64, storage: u64, catalog: u64) -> CheckpointRequest {
        CheckpointRequest {
            checkpoint_lsn: Lsn::new(lsn),
            storage_generation: GenerationId::new(storage),
            catalog_generation: CatalogVersion::new(catalog),
            metadata: vec![1, 2, 3],
        }
    }

    #[test]
    fn publication_is_atomic_and_falls_back() {
        let root = scratch("publish");
        let result = (|| {
            let first = create_checkpoint(&root, request(1, 1, 0))?;
            assert_eq!(first.checkpoint_generation.get(), 1);
            let dir = root.join(CHECKPOINT_DIR_NAME);
            std::fs::write(
                dir.join("checkpoint-00000000000000000002.ckpt.tmp"),
                b"partial",
            )?;
            std::fs::write(dir.join("garbage.txt"), b"nope")?;
            let selected = latest_valid(&root)?.expect("checkpoint");
            assert_eq!(selected.1.checkpoint_generation.get(), 1);
            let second = create_checkpoint(&root, request(2, 2, 1))?;
            assert_eq!(second.checkpoint_generation.get(), 2);
            let path = dir.join("checkpoint-00000000000000000002.ckpt");
            let mut bytes = std::fs::read(&path)?;
            bytes[10] ^= 0xFF;
            std::fs::write(&path, bytes)?;
            let fallback = latest_valid(&root)?.expect("fallback");
            assert_eq!(fallback.1.checkpoint_generation.get(), 1);
            Ok::<(), PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(&root);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn stale_state_is_rejected() {
        let root = scratch("stale");
        let result = (|| {
            create_checkpoint(&root, request(5, 3, 2))?;
            assert!(create_checkpoint(&root, request(4, 3, 2)).is_err());
            assert!(create_checkpoint(&root, request(5, 2, 2)).is_err());
            Ok::<(), PlomidError>(())
        })();
        let _ = std::fs::remove_dir_all(&root);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn failure_at_every_boundary_keeps_previous() {
        for point in [
            FailPoint::BeforeBuild,
            FailPoint::AfterBuild,
            FailPoint::AfterFlush,
            FailPoint::DuringVerify,
            FailPoint::BeforeSync,
            FailPoint::AfterSync,
            FailPoint::DuringPublish,
        ] {
            let root = scratch("fail");
            let result = (|| {
                create_checkpoint(&root, request(1, 1, 0))?;
                assert!(create_checkpoint_with_fail_point(&root, request(2, 2, 1), point).is_err());
                let selected = latest_valid(&root)?.expect("previous survives");
                assert_eq!(selected.1.checkpoint_generation.get(), 1);
                Ok::<(), PlomidError>(())
            })();
            let _ = std::fs::remove_dir_all(&root);
            assert!(result.is_ok(), "point {point:?}: {result:?}");
        }
    }

    #[test]
    fn undurable_lsn_claim_is_rejected() {
        let checkpoint = CheckpointMetadata {
            checkpoint_generation: GenerationId::new(1),
            checkpoint_lsn: Lsn::new(10),
            storage_generation: GenerationId::new(1),
            catalog_generation: CatalogVersion::new(0),
            metadata: Vec::new(),
        };
        assert!(checkpoint.check_durable(Lsn::new(9)).is_err());
        assert!(checkpoint.check_durable(Lsn::new(10)).is_ok());
    }
}
