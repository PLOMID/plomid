#![forbid(unsafe_code)]
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
//! Multi-version concurrency control foundations for PLOMID.

pub mod codec;
pub mod context;
pub mod snapshot;
pub mod store;
pub mod version;

pub use codec::{decode_versioned, encode_versioned, is_versioned};
pub use context::{TransactionContext, TxnState};
pub use snapshot::Snapshot;
pub use store::{gc_horizon, VersionStore, SCAN_CHUNK_ROWS};
pub use version::{is_version_visible, RowVersion, VersionState};
