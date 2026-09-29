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
//! Transaction lifecycle and locking foundations for PLOMID.

mod allocator;
mod checkpoint_policy;
mod concurrent;
mod engine_impl;
mod group_commit;
mod hot_row;
mod transaction;

pub use allocator::TransactionManager;
pub use checkpoint_policy::{
    CheckpointOutcome, CheckpointPolicy, CheckpointStats, CheckpointTrigger,
};
pub use concurrent::{ConcurrentPlomidStorageEngine, ConcurrentTransaction};
pub use engine_impl::PlomidStorageEngine;
pub use hot_row::{row_key, HotRowStore, HotRowTransaction};
pub use plomid_core::CommitTimestamp;
pub use plomid_storage::{StorageEngine, StorageEngineTransaction};
pub use transaction::{
    CommitResult as TxnCommitResult, DataStore, LogStore, Transaction, TransactionState,
};
