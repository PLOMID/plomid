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
//! Configuration for the composed storage components.

use plomid_core::{
    DEFAULT_POOL_CAPACITY, DEFAULT_STORAGE_SEGMENT_SIZE_BYTES, DEFAULT_WAL_SEGMENT_SIZE_BYTES,
};
use plomid_wal::DurabilityMode;

/// Configuration of the composed storage components.
#[derive(Clone, Debug)]
pub struct LifecycleConfig {
    /// Buffer pool capacity (pages) for each segment tree.
    pub pool_capacity: usize,
    /// Active durable segment rotation size in bytes.
    pub segment_size_bytes: u64,
    /// WAL segment rotation size in bytes.
    pub wal_segment_size_bytes: u64,
    /// WAL durability mode. `Force` is the production default; it is never
    /// weakened by the lifecycle coordinator.
    pub wal_durability: DurabilityMode,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            pool_capacity: DEFAULT_POOL_CAPACITY,
            segment_size_bytes: DEFAULT_STORAGE_SEGMENT_SIZE_BYTES,
            wal_segment_size_bytes: DEFAULT_WAL_SEGMENT_SIZE_BYTES,
            wal_durability: DurabilityMode::Force,
        }
    }
}
