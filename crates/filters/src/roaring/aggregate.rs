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
//! Range aggregates over a whole bitmap: counting and testing value ranges.
//!
//! These functions are why a bitmap beats a sorted list for wide row sets: a
//! range query never materializes the values it touches. Each container family
//! answers [`count_in_range`](super::container::Container::count_in_range) in
//! its own representation — a word pop count for bitmaps, a pair scan for runs,
//! a binary search for arrays — and only the buckets that can overlap the range
//! are asked.

use crate::roaring::RoaringBitmap;

impl RoaringBitmap {
    /// Returns how many values fall inside the inclusive range `[start, end]`.
    ///
    /// A range that wraps (`start > end`) matches nothing, which keeps the
    /// method total instead of panicking on a caller's empty range.
    #[must_use]
    pub fn cardinality_in_range(&self, start: u32, end: u32) -> u32 {
        if start > end {
            return 0;
        }
        let (start_key, start_offset) = Self::split(start);
        let (end_key, end_offset) = Self::split(end);

        // Single bucket: ask that bucket directly.
        if start_key == end_key {
            return match self.container(start_key) {
                Some(container) => container.count_in_range(start_offset, end_offset),
                None => 0,
            };
        }

        let mut total = 0u32;
        if let Some(container) = self.container(start_key) {
            total = total.saturating_add(container.count_in_range(start_offset, u16::MAX));
        }
        if let Some(container) = self.container(end_key) {
            total = total.saturating_add(container.count_in_range(0, end_offset));
        }
        // Whole buckets strictly between the two ends are fully contained.
        let between = self
            .containers()
            .filter(|(key, _)| *key > start_key && *key < end_key)
            .map(|(_, container)| container.cardinality())
            .fold(0u32, u32::saturating_add);
        total.saturating_add(between)
    }

    /// Returns whether any value falls inside `[start, end]`.
    ///
    /// Cheaper than `cardinality_in_range(..) > 0`: the scan stops at the first
    /// bucket that holds a value in range.
    #[must_use]
    pub fn contains_range(&self, start: u32, end: u32) -> bool {
        if start > end {
            return false;
        }
        let (start_key, start_offset) = Self::split(start);
        let (end_key, end_offset) = Self::split(end);
        let lower = self.keys().partition_point(|key| *key < start_key);
        let upper = self.keys().partition_point(|key| *key <= end_key);
        // Bucket keys are strictly increasing, so `lower..upper` is exactly the
        // bucket range that can intersect `[start, end]`.
        for position in lower..upper {
            let Some(key) = self.keys().get(position).copied() else {
                continue;
            };
            let Some(container) = self.container(key) else {
                continue;
            };
            let first = if position == lower { start_offset } else { 0 };
            let last = if position + 1 == upper {
                end_offset
            } else {
                u16::MAX
            };
            if container.count_in_range(first, last) > 0 {
                return true;
            }
        }
        false
    }
}
