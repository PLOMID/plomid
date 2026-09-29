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
//! Inclusive value-range insertion and removal.
//!
//! Ranges are the natural unit of work for a filter built during a column scan
//! (a chunk's row positions, a segment's row ids), so they are supported
//! directly rather than only as loops of single-value calls. Both operations
//! share the same bucket walk, and a bucket that empties is dropped.

use crate::roaring::offsets;
use crate::roaring::{RoaringBitmap, RoaringError};

impl RoaringBitmap {
    /// Inserts every value of the inclusive range `[start, end]`.
    ///
    /// Returns how many values were newly added, so a caller can tell an
    /// overlap from a fresh range without a second pass over the values. A
    /// range that wraps (`start > end`) adds nothing, and the range may span
    /// any number of buckets.
    ///
    /// # Errors
    ///
    /// Fails when the result would violate a bitmap invariant. A well-formed
    /// bitmap cannot reach that state, so the check is a self-test of this
    /// operation rather than a caller-visible condition; it is reported instead
    /// of asserted so a damaged bitmap is never silently extended.
    pub fn insert_range(&mut self, start: u32, end: u32) -> Result<u32, RoaringError> {
        if start > end {
            return Ok(0);
        }
        let (start_key, end_key, first, last) = Self::range_bounds(start, end);
        let mut added = 0u32;
        for key in u32::from(start_key)..=u32::from(end_key) {
            let key = key as u16;
            let offsets = offsets::range_offsets(
                if key == start_key { first } else { 0 },
                if key == end_key { last } else { u16::MAX },
            );
            added = added.saturating_add(self.add_offsets(key, &offsets));
        }
        self.validate()?;
        Ok(added)
    }

    /// Removes every value of the inclusive range `[start, end]`.
    ///
    /// Returns how many values were actually present, and drops any bucket that
    /// becomes empty. A range that wraps (`start > end`) removes nothing.
    ///
    /// # Errors
    ///
    /// As [`insert_range`](Self::insert_range): a well-formed bitmap cannot
    /// fail, and the check exists to keep a damaged one from spreading.
    pub fn remove_range(&mut self, start: u32, end: u32) -> Result<u32, RoaringError> {
        if start > end {
            return Ok(0);
        }
        let (start_key, end_key, first, last) = Self::range_bounds(start, end);
        let mut removed = 0u32;
        for key in u32::from(start_key)..=u32::from(end_key) {
            let key = key as u16;
            let Some(index) = self.index_of_key(key) else {
                continue;
            };
            let bucket_first = if key == start_key { first } else { 0 };
            let bucket_last = if key == end_key { last } else { u16::MAX };
            let before = self.bucket_cardinality(index);
            let remaining: Vec<u16> = self
                .bucket_offsets(index)
                .into_iter()
                .filter(|offset| *offset < bucket_first || *offset > bucket_last)
                .collect();
            removed = removed.saturating_add(before - remaining.len() as u32);
            if remaining.is_empty() {
                self.drop_bucket(index);
            } else {
                self.set_bucket(index, &remaining);
            }
        }
        self.validate()?;
        Ok(removed)
    }

    /// Splits a range into its first/last bucket keys and their offsets.
    #[must_use]
    fn range_bounds(start: u32, end: u32) -> (u16, u16, u16, u16) {
        let (start_key, first) = Self::split(start);
        let (end_key, last) = Self::split(end);
        (start_key, end_key, first, last)
    }

    /// Adds one bucket's worth of ascending offsets to `key`.
    ///
    /// Returns how many values were newly added. A missing bucket is created
    /// from those offsets and canonicalized to the family its content
    /// justifies.
    fn add_offsets(&mut self, key: u16, offsets: &[u16]) -> u32 {
        if offsets.is_empty() {
            return 0;
        }
        match self.index_of_key(key) {
            Some(index) => {
                let before = self.bucket_cardinality(index);
                let merged = offsets::union(&self.bucket_offsets(index), offsets);
                let added = merged.len() as u32 - before;
                self.set_bucket(index, &merged);
                added
            }
            None => {
                self.insert_bucket(key, offsets);
                offsets.len() as u32
            }
        }
    }
}
