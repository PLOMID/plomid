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
//! Mount and demount lifecycle coordination for PLOMID storage.
//!
//! [`LifecycleManager`] is the single authoritative lifecycle coordinator. It
//! composes the existing storage components and owns only orchestration and
//! lifecycle ordering; each component keeps its own responsibility:
//!
//! ```text
//! LifecycleManager
//! ├── StorageManager        (segments / B+Tree pages)
//! ├── SegmentedWal          (WAL append / commit / sync)
//! ├── checkpoint module     (checkpoint discovery / creation)
//! ├── recover_storage       (WAL replay / recovery)
//! ├── GenerationManager     (catalog + generation publication)
//! └── lifecycle state       (Closed → Mounting → Ready → Demounting → Closed)
//! ```
//!
//! # Mount order
//!
//! ```text
//! open device
//! → validate device
//! → validate physical format
//! → validate metadata
//! → locate checkpoint
//! → recover WAL
//! → load catalog
//! → establish active generation
//! → Ready
//! ```
//!
//! # Demount order
//!
//! ```text
//! stop mutations
//! → flush required state
//! → checkpoint if required
//! → sync
//! → close resources
//! ```
//!
//! The completed stages of the last mount / demount are recorded in
//! [`LifecycleManager::mount_trace`] and [`LifecycleManager::demount_trace`],
//! so ordering is observable without production logging.

mod config;
mod data;
mod demount;
mod errors;
mod manager;
mod mount;
mod mount_catalog;
mod mount_metadata;
mod mount_recovery;
mod recovery_files;
mod stages;

pub use config::LifecycleConfig;
pub use errors::{DemountError, MountError};
pub use manager::LifecycleManager;
pub use stages::{DemountStage, LifecycleState, MountStage};

// Lifecycle defaults are defined once in `plomid_core::constants`.
pub use plomid_core::{
    DEFAULT_POOL_CAPACITY, DEFAULT_STORAGE_SEGMENT_SIZE_BYTES, DEFAULT_WAL_SEGMENT_SIZE_BYTES,
};
