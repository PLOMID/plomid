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
//! Mount orchestration: fresh database creation and the fixed mount order.
//!
//! Mount order: open device → validate device → validate physical format →
//! validate metadata → locate checkpoint → recover WAL → load catalog →
//! establish active generation → Ready
//!
//! Stages 3-7 live in `mount_metadata`, `mount_recovery`, and `mount_catalog`.

use crate::errors::MountError;
use crate::manager::LifecycleManager;
use crate::stages::{LifecycleState, MountStage};
use plomid_core::{ErrorKind, PlomidError};
use plomid_storage::checkpoint::validate_storage_physical;
use plomid_storage::DatabaseLayout;
use plomid_storage::StorageManager;
use plomid_wal::{SegmentedWal, WAL_DIR_NAME};
use std::path::Path;

impl LifecycleManager {
    /// Creates a fresh database at `root` and mounts it.
    ///
    /// This is the only creation path; plain [`Self::mount`] never creates a
    /// database. Creation delegates device and WAL creation to the existing
    /// components and then runs the same authoritative mount sequence.
    pub fn create(&mut self, root: &Path) -> std::result::Result<(), MountError> {
        self.require_mountable()?;
        StorageManager::create(
            root,
            self.config.pool_capacity,
            self.config.segment_size_bytes,
        )
        .map_err(|error| MountError::new(MountStage::Device, error))?;
        SegmentedWal::create(
            &root.join(WAL_DIR_NAME),
            self.config.wal_segment_size_bytes,
            self.config.wal_durability,
        )
        .map_err(|error| MountError::new(MountStage::Device, error))?;
        self.mount(root)
    }

    /// Mounts the existing database at `root`.
    ///
    /// Mount never creates a database. Every stage must succeed before the
    /// manager enters [`LifecycleState::Ready`]; on any failure the resources
    /// opened so far are released and the state becomes [`LifecycleState::Failed`].
    pub fn mount(&mut self, root: &Path) -> std::result::Result<(), MountError> {
        self.require_mountable()?;
        self.state = LifecycleState::Mounting;
        self.root = Some(root.to_path_buf());
        self.mount_trace.clear();
        self.active_generation = None;
        self.catalog = None;
        self.recovery = None;
        match self.mount_sequence(root) {
            Ok(()) => {
                self.state = LifecycleState::Ready;
                Ok(())
            }
            Err(error) => {
                // Transactional failure: release every resource that was
                // opened successfully and preserve the original error.
                self.storage = None;
                self.wal = None;
                self.generations = None;
                self.active_generation = None;
                self.catalog = None;
                self.recovery = None;
                self.state = LifecycleState::Failed;
                Err(error)
            }
        }
    }

    /// Returns an error unless the manager may enter a mount sequence.
    fn require_mountable(&self) -> std::result::Result<(), MountError> {
        if matches!(self.state, LifecycleState::Closed | LifecycleState::Failed) {
            return Ok(());
        }
        Err(MountError::new(
            MountStage::Lifecycle,
            PlomidError::with_detail(
                ErrorKind::Conflict,
                "storage lifecycle does not allow a mount in the current state",
                format!("state={}", self.state),
            ),
        ))
    }
}
impl LifecycleManager {
    /// The authoritative mount sequence. Stage order is fixed.
    fn mount_sequence(&mut self, root: &Path) -> std::result::Result<(), MountError> {
        // 1. DEVICE: mount never creates a database; the root must already exist
        //    as a valid storage layout. (The segment layer recovers an empty root
        //    by allocating an initial segment, which is creation semantics.)
        if !root.is_dir() {
            return Err(MountError::new(
                MountStage::Device,
                PlomidError::with_detail(
                    ErrorKind::NotFound,
                    "storage root is missing; mount does not create a database",
                    format!("root={}", root.display()),
                ),
            ));
        }
        // Validate the storage layout against the new PLOMID_DATA layout.
        let layout = DatabaseLayout::new(root);
        layout.validate().map_err(|error| {
            MountError::new(
                MountStage::Device,
                PlomidError::with_source(
                    ErrorKind::NotFound,
                    "storage layout validation failed",
                    error,
                ),
            )
        })?;
        let mut storage = StorageManager::open(
            root,
            self.config.pool_capacity,
            self.config.segment_size_bytes,
        )
        .map_err(|error| MountError::new(MountStage::Device, error))?;
        self.mount_trace.push(MountStage::Device);

        // 2. VALIDATE DEVICE / PHYSICAL FORMAT: structural validation owned
        //    by the checkpoint module; the result must agree with the image.
        let physical = validate_storage_physical(root)
            .map_err(|error| MountError::new(MountStage::FormatValidation, error))?;
        let image_generation = storage.storage_generation();
        if physical != image_generation {
            return Err(MountError::new(
                MountStage::FormatValidation,
                PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "validated physical generation disagrees with the open storage image",
                    format!(
                        "physical={} image={}",
                        physical.get(),
                        image_generation.get()
                    ),
                ),
            ));
        }
        self.mount_trace.push(MountStage::FormatValidation);

        // Stages 3-7 run after the device is open and validated.
        self.mount_steps_after_validation(root, &mut storage, image_generation)?;

        // All stages succeeded: publish the opened resources.
        self.storage = Some(storage);
        Ok(())
    }
}
