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
//! Row-version model: one entry of a key's newest-first version chain.
use plomid_core::TxnId;

use super::Snapshot;

/// State of one row version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionState {
    /// Installed by a committed transaction; visibility follows visibility rules.
    Live,
    /// Installed delete tombstone; the key reads as absent when visible.
    Deleted,
    /// Transaction-local version that never enters the shared store.
    Uncommitted,
    /// Reserved for crash/incomplete writers; never visible.
    Invisible,
}

/// One entry of a key's newest-first version chain.
#[derive(Debug, Clone)]
pub struct RowVersion {
    /// Transaction that created this version.
    pub creator: TxnId,
    /// Commit timestamp once the creator commits; `None` while uncommitted.
    pub commit_ts: Option<u64>,
    /// Live / Deleted / Uncommitted / Invisible.
    pub state: VersionState,
    /// Encoded row payload; `None` is a delete tombstone.
    pub payload: Option<Vec<u8>>,
    /// Position of the previous (older) version in the chain, for debugging.
    pub prev: Option<usize>,
}

impl RowVersion {
    /// Builds a committed live version.
    #[must_use]
    pub fn committed(creator: TxnId, commit_ts: u64, payload: Vec<u8>) -> Self {
        Self {
            creator,
            commit_ts: Some(commit_ts),
            state: VersionState::Live,
            payload: Some(payload),
            prev: None,
        }
    }

    /// Builds a committed delete tombstone.
    #[must_use]
    pub fn deleted(creator: TxnId, commit_ts: u64) -> Self {
        Self {
            creator,
            commit_ts: Some(commit_ts),
            state: VersionState::Deleted,
            payload: None,
            prev: None,
        }
    }

    /// Builds a transaction-local uncommitted version (never shared directly).
    #[must_use]
    pub fn uncommitted(creator: TxnId, payload: Option<Vec<u8>>) -> Self {
        Self {
            creator,
            commit_ts: None,
            state: VersionState::Uncommitted,
            payload,
            prev: None,
        }
    }
}

/// Authoritative MVCC visibility: is `version` visible to `snapshot`?
#[must_use]
pub fn is_version_visible(version: &RowVersion, snapshot: &Snapshot) -> bool {
    if version.creator == snapshot.owner() {
        return true;
    }
    match version.state {
        VersionState::Uncommitted | VersionState::Invisible => return false,
        VersionState::Live | VersionState::Deleted => {}
    }
    let Some(commit_ts) = version.commit_ts else {
        return false;
    };
    if snapshot.is_active(version.creator.get()) {
        return false;
    }
    commit_ts <= snapshot.watermark()
}
