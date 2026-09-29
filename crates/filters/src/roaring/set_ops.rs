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
//! Set arithmetic between whole bitmaps.
//!
//! Every operation merges two sorted key lists and delegates each shared key to
//! the container-level operation in [`containers`](super::containers). Three
//! rules keep results canonical and cheap:
//!
//! * a bucket missing on one side counts as an empty bucket, which the
//!   container operations already handle;
//! * a bucket that comes out empty is dropped, so no result ever carries an
//!   empty container;
//! * each resulting bucket is re-canonicalized, so `a ∩ b` can be an array
//!   container even when `a` is a bitmap.
//!
//! Nothing here mutates in place: each operation returns a new bitmap, which is
//! the only form that can be checked against a set model without aliasing
//! hazards.

use crate::roaring::container::Container;
use crate::roaring::containers;
use crate::roaring::RoaringBitmap;

/// The set relation between two bitmaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationalOp {
    /// Every value of the first bitmap is also in the second (proper or equal).
    Subset,
    /// Every value of the second bitmap is also in the first (proper or equal).
    Superset,
    /// The two bitmaps hold exactly the same values.
    Equal,
    /// The bitmaps overlap without one containing the other.
    Intersects,
    /// The bitmaps share no value at all.
    Disjoint,
}

impl RelationalOp {
    /// Returns the label used in diagnostics and explanations.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subset => "subset",
            Self::Superset => "superset",
            Self::Equal => "equal",
            Self::Intersects => "intersects",
            Self::Disjoint => "disjoint",
        }
    }
}

impl std::fmt::Display for RelationalOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Builds the result of merging `left` and `right` bucket by bucket.
///
/// `combine` receives one bucket from each side, with an empty container
/// standing in for a bucket the side does not have.
fn merge(
    left: &RoaringBitmap,
    right: &RoaringBitmap,
    combine: impl Fn(&Container, &Container) -> Container,
) -> RoaringBitmap {
    let mut result = RoaringBitmap::new();
    let empty = Container::Array { values: Vec::new() };
    let (mut i, mut j) = (0_usize, 0_usize);
    while i < left.container_count() || j < right.container_count() {
        let left_bucket = left.bucket(i);
        let right_bucket = right.bucket(j);
        let (key, step) = match (left_bucket, right_bucket) {
            (None, Some((key, _))) => (key, 1),
            (Some((key, _)), None) => (key, 0),
            (Some((left_key, _)), Some((right_key, _))) => {
                if left_key < right_key {
                    (left_key, 0)
                } else if right_key < left_key {
                    (right_key, 1)
                } else {
                    (left_key, 2)
                }
            }
            (None, None) => break,
        };
        let left_side = left.container(key).unwrap_or(&empty);
        let right_side = right.container(key).unwrap_or(&empty);
        let merged = combine(left_side, right_side);
        if !merged.is_empty() {
            result.push_bucket(key, merged);
        }
        match step {
            0 => i += 1,
            1 => j += 1,
            _ => {
                i += 1;
                j += 1;
            }
        }
    }
    result
}

impl RoaringBitmap {
    /// Returns a new bitmap holding every value of either bitmap.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        merge(self, other, containers::union)
    }

    /// Returns a new bitmap holding the values present in both bitmaps.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::new();
        }
        merge(self, other, containers::intersect)
    }

    /// Returns a new bitmap holding the values of `self` that are not in
    /// `other`.
    #[must_use]
    pub fn difference(&self, other: &Self) -> Self {
        if self.is_empty() {
            return Self::new();
        }
        if other.is_empty() {
            return self.clone();
        }
        merge(self, other, containers::difference)
    }

    /// Returns a new bitmap holding the values in exactly one of the bitmaps.
    #[must_use]
    pub fn symmetric_difference(&self, other: &Self) -> Self {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        merge(self, other, containers::symmetric_difference)
    }

    /// Returns whether the two bitmaps share at least one value.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        // Walk the sorted key lists together; only equal keys can meet.
        let (mut i, mut j) = (0_usize, 0_usize);
        while i < self.container_count() && j < other.container_count() {
            let (Some((left_key, left_bucket)), Some((right_key, right_bucket))) =
                (self.bucket(i), other.bucket(j))
            else {
                break;
            };
            match left_key.cmp(&right_key) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    if containers::intersects(left_bucket, right_bucket) {
                        return true;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        false
    }

    /// Returns whether every value of `self` is also in `other`.
    #[must_use]
    pub fn is_subset(&self, other: &Self) -> bool {
        // A superset cannot be smaller; this rejects most answers immediately.
        if self.cardinality() > other.cardinality() {
            return false;
        }
        let (mut i, mut j) = (0_usize, 0_usize);
        while i < self.container_count() {
            let Some((left_key, left_bucket)) = self.bucket(i) else {
                break;
            };
            let mut contained = false;
            while j < other.container_count() {
                let Some((right_key, right_bucket)) = other.bucket(j) else {
                    break;
                };
                if right_key < left_key {
                    j += 1;
                    continue;
                }
                // A bucket of `self` with no counterpart in `other` breaks the
                // subset relation; so does one that is only partly contained.
                if right_key == left_key {
                    contained = containers::is_subset(left_bucket, right_bucket);
                }
                break;
            }
            if !contained {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Returns whether every value of `other` is also in `self`.
    #[must_use]
    pub fn is_superset(&self, other: &Self) -> bool {
        other.is_subset(self)
    }

    /// Returns the exact set relation between the two bitmaps.
    #[must_use]
    pub fn relation(&self, other: &Self) -> RelationalOp {
        if self.cardinality() == other.cardinality() && self.is_subset(other) {
            return RelationalOp::Equal;
        }
        if self.is_subset(other) {
            return RelationalOp::Subset;
        }
        if self.is_superset(other) {
            return RelationalOp::Superset;
        }
        if self.intersects(other) {
            return RelationalOp::Intersects;
        }
        RelationalOp::Disjoint
    }
}
