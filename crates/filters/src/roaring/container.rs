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
//! The [`Container`] type and the shared per-container dispatch.

use super::containers::{array, bitmap, run};

/// u64 word type used by bitmap containers.
pub type RunWord = u64;

/// One bucket of a Roaring bitmap: the values sharing a 16-bit high key.
///
/// All three variants hold the same logical content — a set of offsets in
/// `0..=65535` — and the variant only records *how* those offsets are stored.
/// `PartialEq` and `Debug` are derived for testing and diagnostics; two
/// containers holding the same offsets in different families compare unequal,
/// so tests that mean set equality should compare
/// [`offsets`](Container::offsets). New containers are always produced in the
/// canonical family chosen by
/// [`from_offsets`](super::containers::from_offsets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Container {
    /// Sorted `u16` values; at most 4 096 of them by construction.
    Array {
        /// The values, strictly increasing.
        values: Vec<u16>,
    },
    /// Bit set of 1 024 `u64` words (65 536 bits, 8 KiB).
    Bitmap(
        /// One bit per offset, least significant bit first.
        Vec<RunWord>,
    ),
    /// Run-length encoded pairs; `length` is the offset after `start`.
    Run {
        /// Run starts, strictly increasing and separated by at least one gap.
        offsets: Vec<u16>,
        /// Number of offsets covered after each start (so a run covers
        /// `start ..= start + length`).
        lengths: Vec<u16>,
    },
}

impl Container {
    /// Returns the number of values in the container.
    #[must_use]
    pub fn cardinality(&self) -> u32 {
        match self {
            Self::Array { values } => values.len() as u32,
            Self::Bitmap(words) => bitmap::cardinality(words),
            Self::Run { offsets, lengths } => run::count(offsets, lengths),
        }
    }

    /// Returns whether the container holds no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cardinality() == 0
    }

    /// Returns whether `offset` is in the container.
    #[must_use]
    pub fn contains(&self, offset: u16) -> bool {
        match self {
            Self::Array { values } => array::contains(values, offset),
            Self::Bitmap(words) => bitmap::contains(words, offset),
            Self::Run { offsets, lengths } => run::contains(offsets, lengths, offset),
        }
    }

    /// Expands the container into its ascending offsets.
    ///
    /// This is the normalizing step every set operation and every value
    /// iteration goes through, so no algorithm has to know which family it is
    /// looking at.
    #[must_use]
    pub fn offsets(&self) -> Vec<u16> {
        match self {
            Self::Array { values } => values.clone(),
            Self::Bitmap(words) => bitmap::to_offsets(words),
            Self::Run { offsets, lengths } => run::to_offsets(offsets, lengths),
        }
    }

    /// Returns the smallest value, or `None` when the container is empty.
    #[must_use]
    pub fn min_offset(&self) -> Option<u16> {
        match self {
            Self::Array { values } => array::min_offset(values),
            Self::Bitmap(words) => bitmap::min_offset(words),
            Self::Run { offsets, .. } => run::min_offset(offsets),
        }
    }

    /// Returns the largest value, or `None` when the container is empty.
    #[must_use]
    pub fn max_offset(&self) -> Option<u16> {
        match self {
            Self::Array { values } => array::max_offset(values),
            Self::Bitmap(words) => bitmap::max_offset(words),
            Self::Run { offsets, lengths } => run::max_offset(offsets, lengths),
        }
    }

    /// Returns how many values are `<= offset`.
    #[must_use]
    pub fn count_less_equal(&self, offset: u16) -> u32 {
        match self {
            Self::Array { values } => array::count_less_equal(values, offset),
            Self::Bitmap(words) => bitmap::count_less_equal(words, offset),
            Self::Run { offsets, lengths } => run::count_less_equal(offsets, lengths, offset),
        }
    }

    /// Returns how many values fall inside the inclusive range `[start, end]`.
    #[must_use]
    pub fn count_in_range(&self, start: u16, end: u16) -> u32 {
        match self {
            Self::Array { values } => array::count_in_range(values, start, end),
            Self::Bitmap(words) => bitmap::count_in_range(words, start, end),
            Self::Run { offsets, lengths } => run::count_in_range(offsets, lengths, start, end),
        }
    }

    /// Returns the container that results from adding `offset`.
    #[must_use]
    pub fn insert(&self, offset: u16) -> Self {
        match self {
            Self::Array { values } => array::insert(values, offset),
            Self::Bitmap(words) => bitmap::insert(words, offset),
            Self::Run { offsets, lengths } => run::insert(offsets, lengths, offset),
        }
    }

    /// Returns the container that results from removing `offset`.
    #[must_use]
    pub fn remove(&self, offset: u16) -> Self {
        match self {
            Self::Array { values } => array::remove(values, offset),
            Self::Bitmap(words) => bitmap::remove(words, offset),
            Self::Run { offsets, lengths } => run::remove(offsets, lengths, offset),
        }
    }

    /// Checks every invariant of the stored representation.
    ///
    /// # Errors
    ///
    /// Returns the representation's own error: unsorted or non-increasing
    /// array values, a bitmap that is not exactly 1 024 words, or run arrays
    /// that are not parallel, ordered, separated, and in range.
    pub fn validate(&self) -> Result<(), crate::roaring::RoaringError> {
        match self {
            Self::Array { values } => array::validate(values),
            Self::Bitmap(words) => bitmap::validate(words),
            Self::Run { offsets, lengths } => run::validate(offsets, lengths),
        }
    }
}
