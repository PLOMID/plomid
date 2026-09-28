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
//! PLOMID 1F: Zone Maps + BRIN analytical pruning for immutable columnar
//! segments.
//!
//! ```text
//! predicate
//!     ↓  (PrunePredicate — safe, small predicate model)
//! metadata evaluation      (predicate.rs: the proof rules)
//!     ↓
//! prune impossible ranges  (scan_plan.rs / brin.rs: PRUNE vs KEEP vs UNKNOWN)
//!     ↓
//! candidate row ranges     (CandidateRange)
//!     ↓
//! scan remaining ranges    (SegmentReader::read_rows over candidates)
//! ```
//!
//! The fundamental correctness rule: a range may only be pruned when the
//! metadata *proves* the predicate cannot match any row in it. Missing,
//! uncertain, unsupported, stale, malformed, or insufficient metadata never
//! prunes — false positives (scanning a range with no matches) are
//! acceptable, false negatives (skipping a range with matches) are forbidden.
//!
//! Module map:
//!
//! * [`zonemap`] — `ZoneMap` (min/max/NULL state per row range) and its
//!   incremental builder.
//! * [`predicate`] — `PrunePredicate`, the proof rules per operator, and the
//!   row-level three-valued evaluator used as ground truth.
//! * [`brin`] — `BrinIndex`/`BrinRange`/`BrinBuilder`: row-interval summaries
//!   that tile a segment.
//! * [`trailer`] — deterministic little-endian persistence with CRC32C
//!   integrity, appended to the segment image.
//! * [`scan_plan`] — `SegmentPruning`, `CandidateRange`, `ScanPlan`,
//!   `plan_scan`: the executable pruning API.
//! * [`builders`] — construction from real materialized columns inside the
//!   flush pipeline.

pub mod brin;
pub mod builders;
pub mod predicate;
pub mod scan_plan;
pub mod sql_bridge;
pub mod trailer;
pub mod verdict;
pub mod zonemap;

pub use brin::{BrinBuilder, BrinIndex, BrinRange, DEFAULT_BRIN_ROWS_PER_RANGE};
pub use builders::{build_brin_from_materialized, build_segment_pruning, build_zones_for_chunks};
pub use predicate::{
    negate_predicate, predicate_columns, prune_extent_with_zones, row_matches, PruneOperator,
    PrunePredicate, Tri,
};
pub use scan_plan::{
    candidate_chunks, plan_scan, predicate_column_ids, prune_with_zones, CandidateRange, ScanPlan,
    SegmentPruning,
};
pub use sql_bridge::{explain_unprunable, lower_sql_expression, SqlLowering};
pub use trailer::{
    decode_trailer, decode_trailer_at, encode_trailer, has_trailer, strip_trailer,
    PRUNING_FORMAT_VERSION, PRUNING_MAGIC,
};
pub use verdict::PruneVerdict;
pub use zonemap::{supports_bounds, NullState, ZoneMap, ZoneMapBuilder};
