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
//! Structural invariant checking for a whole bitmap.
//!
//! Validation is what makes a decoded bitmap trustworthy: it re-checks the
//! invariants that every mutating operation maintains, so an image produced by
//! another encoder (or by a damaged one) is rejected before any query is
//! answered from it. It never repairs: a bitmap that fails validation is
//! returned to the caller as an error.

use crate::roaring::{RoaringBitmap, RoaringError};

/// Re-checks every structural invariant of `bitmap`.
///
/// # Invariants
///
/// 1. the key vector and the container vector have equal length;
/// 2. keys are strictly increasing (so `2^16` buckets, each appearing once);
/// 3. no container is empty (an empty bucket would be a phantom key);
/// 4. each container satisfies its own representation's invariants.
///
/// # Errors
///
/// Returns the [`RoaringError`] naming the first invariant that fails, in the
/// order listed above. Checks run in that order so the cheapest structural
/// tests reject a damaged image before any per-container work happens.
pub(crate) fn check(bitmap: &RoaringBitmap) -> Result<(), RoaringError> {
    let count = bitmap.container_count();
    if bitmap.keys().len() != count {
        return Err(RoaringError::MismatchedKeysAndContainers);
    }
    if count > crate::constants::ROARING_MAX_CONTAINERS {
        return Err(RoaringError::MismatchedKeysAndContainers);
    }
    let mut previous: Option<u16> = None;
    for position in 0..count {
        let Some((key, container)) = bitmap.bucket(position) else {
            return Err(RoaringError::MismatchedKeysAndContainers);
        };
        if let Some(previous) = previous {
            if key <= previous {
                return Err(RoaringError::InvalidKeyOrder(key));
            }
        }
        if container.is_empty() {
            return Err(RoaringError::EmptyContainer);
        }
        container.validate()?;
        previous = Some(key);
    }
    Ok(())
}
