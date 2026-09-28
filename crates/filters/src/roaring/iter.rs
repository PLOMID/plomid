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
//! Value and bucket iteration.

use crate::roaring::container::Container;
use crate::roaring::RoaringBitmap;

/// Iterator over the buckets of a bitmap, in ascending key order.
///
/// Yields each 16-bit key with the [`Container`] that holds its offsets, so a
/// caller can consume a bitmap bucket by bucket instead of value by value.
pub struct ContainerIter<'a> {
    pub(crate) keys: std::slice::Iter<'a, u16>,
    pub(crate) containers: std::slice::Iter<'a, Container>,
}

impl<'a> Iterator for ContainerIter<'a> {
    type Item = (u16, &'a Container);

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.keys.next()?;
        let container = self.containers.next()?;
        Some((*key, container))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.keys.size_hint()
    }
}

impl ExactSizeIterator for ContainerIter<'_> {}

/// Iterator over the values of a bitmap, in ascending order.
///
/// The current bucket's offsets are fully expanded when the iterator reaches
/// that bucket, so iteration costs one allocation per non-empty bucket and
/// never depends on the container family. Values are always strictly
/// increasing and each value is yielded exactly once.
pub struct Iter<'a> {
    buckets: ContainerIter<'a>,
    offsets: std::vec::IntoIter<u16>,
    key: u16,
}

impl<'a> Iter<'a> {
    /// Creates an iterator over `bitmap`.
    #[must_use]
    pub(crate) fn new(bitmap: &'a RoaringBitmap) -> Self {
        Self {
            buckets: bitmap.containers(),
            offsets: Vec::new().into_iter(),
            key: 0,
        }
    }
}

impl Iterator for Iter<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(offset) = self.offsets.next() {
                return Some((u32::from(self.key) << 16) | u32::from(offset));
            }
            let (key, container) = self.buckets.next()?;
            if container.is_empty() {
                continue;
            }
            self.key = key;
            self.offsets = container.offsets().into_iter();
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // Expanded values are exact; each remaining bucket holds between one
        // and 65 536 values.
        let buckets = self.buckets.len();
        let lower = self.offsets.len() + buckets;
        let upper = self.offsets.len() + buckets * (usize::from(u16::MAX) + 1);
        (lower, Some(upper))
    }
}

impl std::iter::FusedIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a RoaringBitmap {
    type Item = u32;
    type IntoIter = Iter<'a>;

    /// Iterates the values of the bitmap in ascending order.
    fn into_iter(self) -> Self::IntoIter {
        Iter::new(self)
    }
}
