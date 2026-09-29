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
//! Consistent read snapshots for MVCC visibility.
use plomid_core::TxnId;
use std::collections::BTreeSet;

/// Consistent read snapshot for one transaction.
#[derive(Debug, Clone)]
pub struct Snapshot {
    owner: TxnId,
    watermark: u64,
    active: BTreeSet<u64>,
}

impl Snapshot {
    /// Captures a snapshot for `owner`.
    #[must_use]
    pub fn new(owner: TxnId, watermark: u64, mut active: BTreeSet<u64>) -> Self {
        active.remove(&owner.get());
        Self {
            owner,
            watermark,
            active,
        }
    }

    /// Snapshot that observes every committed version (used by tests).
    #[must_use]
    pub fn bootstrap() -> Self {
        Self {
            owner: TxnId::new(0),
            watermark: u64::MAX,
            active: BTreeSet::new(),
        }
    }

    /// Fresh read snapshot observing the last committed state.
    #[must_use]
    pub fn fresh_committed(last_committed: u64, active: BTreeSet<u64>) -> Self {
        Self::new(TxnId::new(0), last_committed, active)
    }

    /// Owner of this snapshot; own writes are always visible to it.
    #[must_use]
    pub fn owner(&self) -> TxnId {
        self.owner
    }

    /// Highest committed timestamp visible to this snapshot.
    #[must_use]
    pub fn watermark(&self) -> u64 {
        self.watermark
    }

    /// Transactions that were active when the snapshot was taken.
    #[must_use]
    pub fn active(&self) -> &BTreeSet<u64> {
        &self.active
    }

    /// Returns true when `txn_id` was active at snapshot time.
    #[must_use]
    pub fn is_active(&self, txn_id: u64) -> bool {
        self.active.contains(&txn_id)
    }
}
