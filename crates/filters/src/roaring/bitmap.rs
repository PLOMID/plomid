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
//! [`RoaringBitmap`]: membership, mutation, and construction.
//!
//! A bitmap is a pair of parallel vectors — strictly increasing 16-bit keys and
//! one container per key — kept in lockstep by every mutating method. All
//! lookups are `O(log buckets)` on the key vector plus an `O(log container)`
//! test inside the bucket, and no operation ever leaves an empty container
//! behind.
//!
//! Values are plain `u32` logical row positions. The crate assigns no meaning
//! to them: a caller storing row positions, MVCC transaction ids, or document
//! ids uses the same API.

use crate::constants::CONTAINER_VALUES;
use crate::roaring::container::Container;
use crate::roaring::containers::{self, run};
use crate::roaring::iter::{ContainerIter, Iter};
use crate::roaring::RoaringError;

/// A Roaring bitmap over a 32-bit value space.
///
/// ```text
/// keys       = [3, 9]                        strictly increasing bucket keys
/// containers = [Array{[7,8]}, Bitmap{…}]     keys[i] owns containers[i]
///     value 0x0003_0007 → key 3, offset 7     → containers[0]
///     value 0x0009_1000 → key 9, offset 0x1000 → containers[1]
/// ```
///
/// `Default` is the empty bitmap; `Clone` is a deep copy of both vectors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoaringBitmap {
    keys: Vec<u16>,
    containers: Vec<Container>,
}

impl RoaringBitmap {
    /// Creates an empty bitmap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the bitmap holds no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Returns the number of buckets (non-empty containers).
    #[must_use]
    pub fn container_count(&self) -> usize {
        self.keys.len()
    }

    /// Returns the total number of values.
    #[must_use]
    pub fn cardinality(&self) -> u32 {
        self.containers
            .iter()
            .map(Container::cardinality)
            .fold(0u32, u32::saturating_add)
    }

    /// Splits a value into its bucket key and in-bucket offset.
    #[inline]
    #[must_use]
    pub(crate) fn split(value: u32) -> (u16, u16) {
        ((value >> 16) as u16, (value & 0xFFFF) as u16)
    }

    /// Returns the bucket index for `key`, if the bucket exists.
    #[inline]
    #[must_use]
    pub(crate) fn index_of_key(&self, key: u16) -> Option<usize> {
        self.keys.binary_search(&key).ok()
    }

    /// Returns the container for `key`, if it exists.
    #[must_use]
    pub fn container(&self, key: u16) -> Option<&Container> {
        self.index_of_key(key).map(|index| &self.containers[index])
    }

    /// Returns the container and key at `position` in bucket order.
    #[must_use]
    pub(crate) fn bucket(&self, position: usize) -> Option<(u16, &Container)> {
        let key = *self.keys.get(position)?;
        let container = self.containers.get(position)?;
        Some((key, container))
    }

    /// Returns whether `value` is in the set.
    #[must_use]
    pub fn contains(&self, value: u32) -> bool {
        let (key, offset) = Self::split(value);
        match self.index_of_key(key) {
            Some(index) => self.containers[index].contains(offset),
            None => false,
        }
    }

    /// Inserts `value` and returns whether it was newly added.
    pub fn insert(&mut self, value: u32) -> bool {
        let (key, offset) = Self::split(value);
        match self.keys.binary_search(&key) {
            Ok(index) => {
                let before = self.containers[index].cardinality();
                self.containers[index] = self.containers[index].insert(offset);
                self.containers[index].cardinality() > before
            }
            Err(index) => {
                self.keys.insert(index, key);
                self.containers
                    .insert(index, containers::from_offsets(&[offset]));
                true
            }
        }
    }

    /// Removes `value` and returns whether it was present.
    ///
    /// The bucket is dropped once its last value goes, so an empty container
    /// can never be observed.
    pub fn remove(&mut self, value: u32) -> bool {
        let (key, offset) = Self::split(value);
        let Some(index) = self.index_of_key(key) else {
            return false;
        };
        if !self.containers[index].contains(offset) {
            return false;
        }
        let next = self.containers[index].remove(offset);
        if next.is_empty() {
            self.keys.remove(index);
            self.containers.remove(index);
        } else {
            self.containers[index] = next;
        }
        true
    }

    /// Iterates the buckets in ascending key order.
    pub fn containers(&self) -> ContainerIter<'_> {
        ContainerIter {
            keys: self.keys.iter(),
            containers: self.containers.iter(),
        }
    }

    /// Iterates the values in ascending order.
    pub fn iter(&self) -> Iter<'_> {
        Iter::new(self)
    }

    /// Returns the bucket keys in ascending order.
    #[must_use]
    pub fn keys(&self) -> &[u16] {
        &self.keys
    }

    /// Returns the smallest value, or `None` when the bitmap is empty.
    #[must_use]
    pub fn min(&self) -> Option<u32> {
        let key = *self.keys.first()?;
        let offset = self.containers.first()?.min_offset()?;
        Some((u32::from(key) << 16) | u32::from(offset))
    }

    /// Returns the largest value, or `None` when the bitmap is empty.
    #[must_use]
    pub fn max(&self) -> Option<u32> {
        let key = *self.keys.last()?;
        let offset = self.containers.last()?.max_offset()?;
        Some((u32::from(key) << 16) | u32::from(offset))
    }

    /// Builds a bitmap from any value sequence.
    ///
    /// Duplicates collapse, so the result is the *set* of the given values.
    #[must_use]
    pub fn from_values(values: impl IntoIterator<Item = u32>) -> Self {
        let mut bitmap = Self::new();
        for value in values {
            bitmap.insert(value);
        }
        bitmap
    }

    /// Returns whether the bitmap holds every value of the 32-bit space.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.keys.len() == usize::from(u16::MAX) + 1
            && self.cardinality() == CONTAINER_VALUES.saturating_mul(CONTAINER_VALUES)
    }

    /// Checks every structural invariant of the bitmap.
    ///
    /// # Errors
    ///
    /// Returns [`RoaringError::MismatchedKeysAndContainers`] when the key and
    /// container vectors differ in length, [`RoaringError::InvalidKeyOrder`]
    /// when keys are not strictly increasing,
    /// [`RoaringError::EmptyContainer`] for a bucket with no values, and the
    /// container-level errors from [`Container::validate`].
    pub fn validate(&self) -> Result<(), RoaringError> {
        crate::roaring::validate::check(self)
    }

    /// Returns the run-length encoding of one bucket, if the bucket exists.
    ///
    /// A diagnostic helper for tests and tooling: it exposes the stored pairs
    /// (canonicalizing non-run buckets) without handing out access to the
    /// internal vectors.
    #[must_use]
    pub fn run_pairs(&self, key: u16) -> Option<(Vec<u16>, Vec<u16>)> {
        match self.container(key)? {
            Container::Run { offsets, lengths } => Some((offsets.clone(), lengths.clone())),
            other => Some(run::from_offsets(&other.offsets())),
        }
    }

    /// Returns the offsets of bucket `index`, or nothing when it does not exist.
    #[must_use]
    pub(crate) fn bucket_offsets(&self, index: usize) -> Vec<u16> {
        match self.containers.get(index) {
            Some(container) => container.offsets(),
            None => Vec::new(),
        }
    }

    /// Returns the cardinality of bucket `index`, or zero when it does not
    /// exist.
    #[must_use]
    pub(crate) fn bucket_cardinality(&self, index: usize) -> u32 {
        match self.containers.get(index) {
            Some(container) => container.cardinality(),
            None => 0,
        }
    }

    /// Inserts a new bucket for `key` holding exactly `offsets`.
    ///
    /// The caller guarantees the key is absent; the offsets are canonicalized
    /// to the family their content justifies.
    pub(crate) fn insert_bucket(&mut self, key: u16, offsets: &[u16]) {
        let position = match self.keys.binary_search(&key) {
            Ok(_) => return,
            Err(position) => position,
        };
        self.keys.insert(position, key);
        self.containers
            .insert(position, containers::from_offsets(offsets));
    }

    /// Rewrites bucket `index` to hold exactly `offsets`, in canonical form.
    pub(crate) fn set_bucket(&mut self, index: usize, offsets: &[u16]) {
        if let Some(slot) = self.containers.get_mut(index) {
            *slot = containers::from_offsets(offsets);
        }
    }

    /// Removes bucket `index`, keys and container together.
    pub(crate) fn drop_bucket(&mut self, index: usize) {
        if index < self.keys.len() {
            self.keys.remove(index);
            self.containers.remove(index);
        }
    }

    /// Appends a bucket to the end of the structures.
    ///
    /// Only for decoders that have already proven keys are strictly ascending;
    /// use [`insert_bucket`](Self::insert_bucket) for arbitrary keys.
    pub(crate) fn push_bucket(&mut self, key: u16, container: Container) {
        self.keys.push(key);
        self.containers.push(container);
    }
}
