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
//! The three-way answer of metadata evaluation.
//!
//! * [`PruneVerdict::Prune`] — the metadata *proves* no row in the range can
//!   satisfy the predicate. Only this verdict lets a caller skip rows.
//! * [`PruneVerdict::Keep`] — applicable metadata exists and it does **not**
//!   prove emptiness. The range may hold matches; scan it.
//! * [`PruneVerdict::Unknown`] — there was no applicable evidence at all:
//!   missing bounds, an unsupported type, a foreign column, an unsupported
//!   literal. The range may hold matches; scan it.
//!
//! `Keep` and `Unknown` both mean "scan"; they are kept distinct so callers
//! and tests can tell "the metadata looked and could not decide" from "the
//! metadata had nothing to say". Uncertainty only ever *downgrades* to a
//! scan — the combinators in [`super::predicate`] never upgrade a non-pruning
//! branch into a pruning verdict.

use std::fmt;

/// The outcome of evaluating a predicate against one range's metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PruneVerdict {
    /// Metadata proves the range holds no matching row; it can be skipped.
    Prune,
    /// Applicable metadata exists and does not prove emptiness; scan.
    Keep,
    /// No applicable metadata; scan.
    Unknown,
}

impl PruneVerdict {
    /// Returns true only for [`PruneVerdict::Prune`], the sole skippable
    /// verdict.
    #[must_use]
    pub fn prunes(self) -> bool {
        matches!(self, Self::Prune)
    }

    /// Returns true for every verdict that requires the rows to be scanned.
    #[must_use]
    pub fn keep(self) -> bool {
        !matches!(self, Self::Prune)
    }
}

impl fmt::Display for PruneVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prune => write!(f, "PRUNE"),
            Self::Keep => write!(f, "KEEP"),
            Self::Unknown => write!(f, "UNKNOWN"),
        }
    }
}
