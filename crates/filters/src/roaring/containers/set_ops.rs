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
//! Container-level set arithmetic.
//!
//! Each function takes the two containers of one bucket and returns the bucket
//! that results. The work is done on sorted offsets
//! ([`offsets`](super::offsets)) and re-encoded with
//! [`from_offsets`](super::containers::from_offsets), so the result is always
//! in the canonical representation for its content.
//!
//! When one side is empty the other side's **clone** is returned untouched:
//! cloning preserves the family the bucket already had instead of spending
//! time re-encoding an identical value set, and `union(x, ∅) == x` and
//! `difference(x, ∅) == x` hold byte-for-byte.

use crate::roaring::container::Container;
use crate::roaring::containers::from_offsets;
use crate::roaring::offsets;

/// Returns the union of two containers.
#[must_use]
pub(crate) fn union(left: &Container, right: &Container) -> Container {
    if left.is_empty() {
        return right.clone();
    }
    if right.is_empty() {
        return left.clone();
    }
    // A superset already *is* the union, so skip the merge.
    let left_offsets = left.offsets();
    let right_offsets = right.offsets();
    if offsets::is_subset(&right_offsets, &left_offsets) {
        return left.clone();
    }
    if offsets::is_subset(&left_offsets, &right_offsets) {
        return right.clone();
    }
    from_offsets(&offsets::union(&left_offsets, &right_offsets))
}

/// Returns the intersection of two containers.
#[must_use]
pub(crate) fn intersect(left: &Container, right: &Container) -> Container {
    if left.is_empty() || right.is_empty() {
        return Container::Array { values: Vec::new() };
    }
    let left_offsets = left.offsets();
    let right_offsets = right.offsets();
    if offsets::is_subset(&left_offsets, &right_offsets) {
        return left.clone();
    }
    if offsets::is_subset(&right_offsets, &left_offsets) {
        return right.clone();
    }
    from_offsets(&offsets::intersect(&left_offsets, &right_offsets))
}

/// Returns the values of `left` that are not in `right`.
#[must_use]
pub(crate) fn difference(left: &Container, right: &Container) -> Container {
    if left.is_empty() {
        return Container::Array { values: Vec::new() };
    }
    if right.is_empty() {
        return left.clone();
    }
    let left_offsets = left.offsets();
    let right_offsets = right.offsets();
    if !offsets::overlaps(&left_offsets, &right_offsets) {
        return left.clone();
    }
    from_offsets(&offsets::difference(&left_offsets, &right_offsets))
}

/// Returns the values in exactly one of the two containers.
#[must_use]
pub(crate) fn symmetric_difference(left: &Container, right: &Container) -> Container {
    if left.is_empty() {
        return right.clone();
    }
    if right.is_empty() {
        return left.clone();
    }
    let left_offsets = left.offsets();
    let right_offsets = right.offsets();
    from_offsets(&offsets::symmetric_difference(
        &left_offsets,
        &right_offsets,
    ))
}

/// Returns whether the two containers share at least one value.
#[must_use]
pub(crate) fn intersects(left: &Container, right: &Container) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    // Range tests are cheap for every family, so reject disjoint buckets
    // before expanding either side.
    let overlap_start = left.min_offset().max(right.min_offset());
    let overlap_end = left.max_offset().min(right.max_offset());
    match (overlap_start, overlap_end) {
        (Some(start), Some(end)) if start <= end => {}
        _ => return false,
    }
    offsets::overlaps(&left.offsets(), &right.offsets())
}

/// Returns whether every value of `left` is also in `right`.
#[must_use]
pub(crate) fn is_subset(left: &Container, right: &Container) -> bool {
    if left.cardinality() > right.cardinality() {
        return false;
    }
    if left.is_empty() {
        return true;
    }
    // A subset must sit inside the other container's value span.
    if left.min_offset() < right.min_offset() || left.max_offset() > right.max_offset() {
        return false;
    }
    offsets::is_subset(&left.offsets(), &right.offsets())
}
