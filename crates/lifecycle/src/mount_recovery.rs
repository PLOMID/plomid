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
//! Mount stages 4 (checkpoint) and 5 (recovery).

use crate::errors::MountError;
use crate::manager::LifecycleManager;
use crate::recovery_files::{newest_checkpoint_path, truncate_wal_crash_tail};
use crate::stages::MountStage;
use plomid_core::{ErrorKind, GenerationId, Lsn, PlomidError};
use plomid_storage::checkpoint::load_checkpoint;
use plomid_storage::StorageManager;
use plomid_wal::{recover_storage, select_checkpoint, SegmentedWal, WAL_DIR_NAME};
use std::path::Path;

impl LifecycleManager {
    /// Mount stages 4 (checkpoint) and 5 (recovery).
    pub(crate) fn mount_steps_checkpoint_and_recovery(
        &mut self,
        root: &Path,
        storage: &mut StorageManager,
        image_generation: GenerationId,
        durable: Option<Lsn>,
    ) -> std::result::Result<(), MountError> {
        // 4. LOCATE CHECKPOINT: the newest published checkpoint must load;
        //    corruption there is reported instead of silently skipped. The
        //    usable replay boundary is selected by the existing WAL layer.
        if let Some(newest) = newest_checkpoint_path(root)
            .map_err(|error| MountError::new(MountStage::Checkpoint, error))?
        {
            load_checkpoint(&newest)
                .map_err(|error| MountError::new(MountStage::Checkpoint, error))?;
        }
        let selected = select_checkpoint(root, image_generation, durable)
            .map_err(|error| MountError::new(MountStage::Checkpoint, error))?;
        self.mount_trace.push(MountStage::Checkpoint);

        // 5. RECOVER WAL: replay is performed by the existing recovery path;
        //    the coordinator only orchestrates it.
        let report = recover_storage(root, storage)
            .map_err(|error| MountError::new(MountStage::Recovery, error))?;
        if let Some(checkpoint) = report
            .checkpoint
            .as_ref()
            .filter(|selection| !selection.is_none())
        {
            let selection_missing = selected.as_ref().is_none_or(|(_, metadata)| {
                metadata.checkpoint_generation.get() < checkpoint.checkpoint_generation.get()
            });
            if selection_missing {
                return Err(MountError::new(
                    MountStage::Recovery,
                    PlomidError::with_detail(
                        ErrorKind::Corruption,
                        "recovery used a checkpoint generation that failed pre-selection",
                        format!("used_generation={}", checkpoint.checkpoint_generation.get()),
                    ),
                ));
            }
        }
        // The recovery contract marks bytes at and beyond the crash tail as
        // never durable. Restoring the newest segment to that boundary brings
        // the log back to its durable prefix before the writer opens it, so
        // the writer continues exactly where durability ends.
        if let Some(boundary) = report.crash_tail_boundary {
            truncate_wal_crash_tail(root, boundary)
                .map_err(|error| MountError::new(MountStage::Recovery, error))?;
        }
        // Reopen the WAL writer for future appends after recovery completed.
        let wal = SegmentedWal::open(
            &root.join(WAL_DIR_NAME),
            self.config.wal_segment_size_bytes,
            self.config.wal_durability,
        )
        .map_err(|error| MountError::new(MountStage::Recovery, error))?;
        self.mount_trace.push(MountStage::Recovery);

        self.wal = Some(wal);
        self.recovery = Some(report);
        Ok(())
    }
}
