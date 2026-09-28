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
//! The [`LifecycleManager`] type, its read-only accessors, and the internal
//! handle accessors used by the mount, demount, and operation modules.

use crate::config::LifecycleConfig;
use crate::stages::{DemountStage, LifecycleState, MountStage};
use plomid_core::{ErrorKind, GenerationId, PlomidError, Result};
use plomid_storage::catalog::CatalogState;
use plomid_storage::generation::GenerationManager;
use plomid_storage::StorageManager;
use plomid_wal::{CheckpointReplayReport, SegmentedWal};
use std::path::{Path, PathBuf};

/// The authoritative mount/demount lifecycle coordinator.
///
/// Composes the existing storage components; owns only lifecycle ordering,
/// the explicit state machine, and the mutation gate. On-disk formats, WAL
/// records, checksums, and durability semantics are owned (unchanged) by the
/// components themselves.
pub struct LifecycleManager {
    pub(crate) config: LifecycleConfig,
    pub(crate) state: LifecycleState,
    pub(crate) root: Option<PathBuf>,
    pub(crate) storage: Option<StorageManager>,
    pub(crate) wal: Option<SegmentedWal>,
    pub(crate) generations: Option<GenerationManager>,
    pub(crate) active_generation: Option<GenerationId>,
    pub(crate) catalog: Option<CatalogState>,
    pub(crate) recovery: Option<CheckpointReplayReport>,
    pub(crate) mount_trace: Vec<MountStage>,
    pub(crate) demount_trace: Vec<DemountStage>,
}

impl LifecycleManager {
    /// Creates a manager in the [`LifecycleState::Closed`] state.
    #[must_use]
    pub fn new(config: LifecycleConfig) -> Self {
        Self {
            config,
            state: LifecycleState::Closed,
            root: None,
            storage: None,
            wal: None,
            generations: None,
            active_generation: None,
            catalog: None,
            recovery: None,
            mount_trace: Vec::new(),
            demount_trace: Vec::new(),
        }
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub fn state(&self) -> LifecycleState {
        self.state
    }

    /// Returns the storage root of the last mount attempt, if any.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Returns `true` when the storage is mounted and ready.
    #[must_use]
    pub fn is_mounted(&self) -> bool {
        self.state == LifecycleState::Ready
    }

    /// Stages completed by the last mount, in execution order.
    #[must_use]
    pub fn mount_trace(&self) -> &[MountStage] {
        &self.mount_trace
    }

    /// Stages completed by the last demount, in execution order.
    #[must_use]
    pub fn demount_trace(&self) -> &[DemountStage] {
        &self.demount_trace
    }

    /// Active generation established at mount, if mounted.
    #[must_use]
    pub fn active_generation(&self) -> Option<GenerationId> {
        self.active_generation
    }

    /// Authoritative catalog loaded at mount, when one was published.
    #[must_use]
    pub fn catalog(&self) -> Option<&CatalogState> {
        self.catalog.as_ref()
    }

    /// Recovery report of the last mount, when recovery ran.
    #[must_use]
    pub fn recovery_report(&self) -> Option<&CheckpointReplayReport> {
        self.recovery.as_ref()
    }
}

impl LifecycleManager {
    /// Returns the mounted storage, or an internal error when absent.
    pub(crate) fn required_storage(&mut self) -> Result<&mut StorageManager> {
        self.storage
            .as_mut()
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "storage manager is not open"))
    }

    /// Returns the mounted WAL, or an internal error when absent.
    pub(crate) fn required_wal(&mut self) -> Result<&mut SegmentedWal> {
        self.wal
            .as_mut()
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "write-ahead log is not open"))
    }

    /// Returns the storage root, or an internal error when absent.
    pub(crate) fn required_root(&self) -> Result<&Path> {
        self.root
            .as_deref()
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "storage root is not set"))
    }

    /// Mutation gate: rejects any storage operation outside [`LifecycleState::Ready`].
    pub(crate) fn require_ready(&self, operation: &str) -> Result<()> {
        if self.state == LifecycleState::Ready {
            return Ok(());
        }
        Err(PlomidError::with_detail(
            ErrorKind::Conflict,
            "storage operation requires a mounted (Ready) lifecycle",
            format!("state={} operation={operation}", self.state),
        ))
    }
}
