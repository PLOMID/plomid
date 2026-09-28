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
//! Versioned envelope for the physical KV layer.
use plomid_core::TxnId;

use super::{RowVersion, VersionState};

/// True when `bytes` carry a versioned envelope.
#[must_use]
pub fn is_versioned(bytes: &[u8]) -> bool {
    bytes.len() >= 18 && bytes[0] == 0x56 && (1..=4).contains(&bytes[1])
}

/// Encodes a versioned value: magic 0x56 | state | creator LE | ts LE | payload.
pub fn encode_versioned(version: &RowVersion) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + version.payload.as_ref().map_or(0, Vec::len));
    out.push(0x56);
    out.push(match version.state {
        VersionState::Live => 1,
        VersionState::Deleted => 2,
        VersionState::Uncommitted => 3,
        VersionState::Invisible => 4,
    });
    out.extend_from_slice(&version.creator.get().to_le_bytes());
    out.extend_from_slice(&version.commit_ts.unwrap_or(0).to_le_bytes());
    if let Some(p) = &version.payload {
        out.extend_from_slice(p);
    }
    out
}

/// Decodes the envelope; rejects bad magic, bad state, truncation.
pub fn decode_versioned(bytes: &[u8]) -> Option<RowVersion> {
    if bytes.len() < 18 || bytes[0] != 0x56 {
        return None;
    }
    let state = match bytes[1] {
        1 => VersionState::Live,
        2 => VersionState::Deleted,
        3 => VersionState::Uncommitted,
        4 => VersionState::Invisible,
        _ => return None,
    };
    let creator = TxnId::new(u64::from_le_bytes(bytes[2..10].try_into().ok()?));
    let raw_ts = u64::from_le_bytes(bytes[10..18].try_into().ok()?);
    let payload = match state {
        VersionState::Deleted => None,
        _ => Some(bytes[18..].to_vec()),
    };
    Some(RowVersion {
        creator,
        commit_ts: Some(raw_ts),
        state,
        payload,
        prev: None,
    })
}
