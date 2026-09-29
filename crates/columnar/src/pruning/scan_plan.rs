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
//! The pruning pipeline: predicate in, candidate physical ranges out.
//!
//! This is the executable boundary of 1F. [`SegmentPruning`] holds the
//! metadata attached to one decoded segment; `prune_candidates` turns a
//! [`PrunePredicate`] into the row ranges that must actually be scanned,
//! and `prune_chunks` answers the same question at chunk granularity. Both
//! reuse [`BrinIndex::evaluate_extent`], whose proof rules guarantee that
//! only provably-empty ranges are ever skipped.

use super::brin::BrinIndex;
use super::predicate::{prune_extent_with_zones, PrunePredicate};
use super::verdict::PruneVerdict;
use plomid_core::{ColumnId, Result};
use std::fmt;

/// One candidate physical row range surviving pruning.
///
/// Logical row coordinates only: physical offsets stay inside the storage
/// layer, keeping logical identity independent of physical placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidateRange {
    /// First row to scan (inclusive).
    pub start_row: u64,
    /// First row not to scan (exclusive).
    pub end_row: u64,
}

impl CandidateRange {
    /// Creates a candidate range.
    #[must_use]
    pub fn new(start_row: u64, end_row: u64) -> Self {
        Self { start_row, end_row }
    }

    /// Number of rows in this candidate.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.end_row.saturating_sub(self.start_row)
    }
}

/// Pruning metadata attached to one decoded segment.
///
/// The BRIN index (when present) is the persisted form: its ranges tile
/// `[0, row_count)` and each carries per-column zone maps. `row_count`
/// records the row count the metadata was written for, so a caller can
/// verify the metadata describes *this* segment before trusting it
/// (see [`SegmentPruning::describes`]).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SegmentPruning {
    /// BRIN ranges with per-column zone maps, when the segment was flushed
    /// with pruning metadata.
    pub brin: Option<BrinIndex>,
    /// Row count the metadata describes.
    pub row_count: u64,
}

impl SegmentPruning {
    /// Creates metadata for a segment with `row_count` rows and no ranges.
    #[must_use]
    pub fn empty(row_count: u64) -> Self {
        Self {
            brin: None,
            row_count,
        }
    }

    /// Creates metadata from a validated BRIN index.
    #[must_use]
    pub fn from_brin(brin: BrinIndex) -> Self {
        Self {
            row_count: brin.row_count,
            brin: Some(brin),
        }
    }

    /// Returns true when no pruning metadata is attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.brin.as_ref().is_none_or(|brin| brin.ranges.is_empty())
    }

    /// Returns true when this metadata describes a segment of `row_count`
    /// rows.
    ///
    /// A mismatch means the metadata was written for a different segment
    /// layout; the caller must then ignore it entirely and scan everything.
    #[must_use]
    pub fn describes(&self, row_count: u64) -> bool {
        match &self.brin {
            Some(brin) => brin.row_count == row_count,
            // No ranges: nothing to misapply, but also nothing to prove.
            None => false,
        }
    }

    /// Returns the candidate row ranges surviving `predicate`.
    ///
    /// Candidates are the BRIN row ranges that were not proven empty. The
    /// result always covers every row that could contain a match: ranges
    /// proven empty are dropped, everything else — including all rows when
    /// metadata is missing or the predicate is unsupported — is kept.
    /// Uncertainty therefore never removes a row from the scan.
    #[must_use]
    pub fn prune_candidates(&self, predicate: &PrunePredicate) -> Vec<CandidateRange> {
        let Some(brin) = &self.brin else {
            // No metadata: scan the whole segment.
            return self.unpruned_candidates();
        };
        let mut candidates = Vec::with_capacity(brin.ranges.len());
        for range in &brin.ranges {
            if range.evaluate(predicate) == PruneVerdict::Prune {
                continue; // metadata proves no row in this range can match
            }
            if range.end_row > range.start_row {
                candidates.push(CandidateRange::new(range.start_row, range.end_row));
            }
        }
        candidates
    }

    /// Returns the whole segment as one candidate (when there is nothing to
    /// prune with).
    #[must_use]
    fn unpruned_candidates(&self) -> Vec<CandidateRange> {
        if self.row_count == 0 {
            Vec::new()
        } else {
            vec![CandidateRange::new(0, self.row_count)]
        }
    }

    /// Evaluates one predicate over `[start_row, end_row)`.
    #[must_use]
    pub fn prune_extent(
        &self,
        predicate: &PrunePredicate,
        start_row: u64,
        end_row: u64,
    ) -> PruneVerdict {
        match &self.brin {
            Some(brin) => brin.evaluate_extent(predicate, start_row, end_row),
            None => {
                if start_row >= end_row {
                    PruneVerdict::Prune
                } else {
                    PruneVerdict::Unknown
                }
            }
        }
    }

    /// Prunes whole chunks: returns the chunk indexes that must be scanned.
    ///
    /// `chunks` are `(first_row, row_count)` extents in scan order (the
    /// column chunk table's physical extents). A chunk is pruned only when
    /// the metadata proves no row in it can match; every other chunk
    /// survives. With no metadata, every chunk survives.
    #[must_use]
    pub fn prune_chunks(&self, predicate: &PrunePredicate, chunks: &[(u64, u64)]) -> Vec<usize> {
        let Some(brin) = &self.brin else {
            return (0..chunks.len()).collect();
        };
        let mut survivors = Vec::new();
        for (index, (first_row, row_count)) in chunks.iter().enumerate() {
            let end_row = first_row.saturating_add(*row_count);
            let verdict = brin.evaluate_extent(predicate, *first_row, end_row);
            if verdict != PruneVerdict::Prune {
                survivors.push(index);
            }
        }
        survivors
    }

    /// Storage overhead of the persisted trailer in bytes (for reporting).
    #[must_use]
    pub fn storage_bytes(&self) -> usize {
        match &self.brin {
            Some(brin) => super::trailer::encode_trailer(brin).len(),
            None => 0,
        }
    }

    /// Validates the pruning metadata.
    /// Reuses the BRIN index validation, which checks:
    /// - Contiguous, non-overlapping ranges tiling [0, row_count)
    /// - Zone maps within their parent range
    /// - No empty ranges when row_count > 0
    pub fn validate(&self) -> Result<()> {
        if let Some(brin) = &self.brin {
            brin.validate()
        } else {
            Ok(())
        }
    }
}

/// An executable pruning outcome: what to scan and what was skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanPlan {
    /// Surviving row ranges in ascending order.
    pub candidates: Vec<CandidateRange>,
    /// Total rows in the segment.
    pub total_rows: u64,
    /// Rows covered by `candidates`.
    pub scanned_rows: u64,
}

/// Builds the scan plan for `predicate`: candidate ranges plus coverage
/// statistics a caller can log without re-scanning.
///
/// When the metadata does not describe a segment of `row_count` rows, the
/// plan scans everything (metadata is ignored, never applied).
#[must_use]
pub fn plan_scan(pruning: &SegmentPruning, row_count: u64, predicate: &PrunePredicate) -> ScanPlan {
    let candidates = if pruning.describes(row_count) {
        pruning.prune_candidates(predicate)
    } else {
        SegmentPruning::empty(row_count).unpruned_candidates()
    };
    let scanned_rows = candidates.iter().map(CandidateRange::row_count).sum();
    ScanPlan {
        candidates,
        total_rows: row_count,
        scanned_rows,
    }
}

impl ScanPlan {
    /// Rows skipped by pruning.
    #[must_use]
    pub fn pruned_rows(&self) -> u64 {
        self.total_rows.saturating_sub(self.scanned_rows)
    }

    /// Fraction of rows skipped (`0.0` for an empty segment).
    #[must_use]
    pub fn pruning_ratio(&self) -> f64 {
        if self.total_rows == 0 {
            0.0
        } else {
            self.pruned_rows() as f64 / self.total_rows as f64
        }
    }
}

impl fmt::Display for ScanPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ScanPlan {{ candidates: {}, scanned: {}/{}, pruned: {:.1}% }}",
            self.candidates.len(),
            self.scanned_rows,
            self.total_rows,
            self.pruning_ratio() * 100.0
        )
    }
}

/// Returns the chunk indexes a pruned scan must decode for `predicate`.
///
/// A thin wrapper over [`SegmentPruning::prune_chunks`] that keeps the
/// "no metadata → scan everything" behavior in one place for callers.
#[must_use]
pub fn candidate_chunks(
    pruning: &SegmentPruning,
    predicate: &PrunePredicate,
    chunks: &[(u64, u64)],
) -> Vec<usize> {
    pruning.prune_chunks(predicate, chunks)
}

/// Column ids referenced by a predicate (convenience for callers building
/// read plans).
#[must_use]
pub fn predicate_column_ids(predicate: &PrunePredicate) -> Vec<ColumnId> {
    super::predicate::predicate_columns(predicate)
}

/// Evaluates an extent directly against a raw zone-map list (used by the
/// property tests and by callers holding zone maps without a full index).
#[must_use]
pub fn prune_with_zones(
    zones: &[super::zonemap::ZoneMap],
    predicate: &PrunePredicate,
    start_row: u64,
    end_row: u64,
) -> PruneVerdict {
    prune_extent_with_zones(zones, predicate, start_row, end_row)
}
