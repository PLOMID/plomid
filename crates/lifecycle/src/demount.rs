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
//! Demount orchestration: stop mutations → flush → checkpoint → sync → close.

use crate::errors::DemountError;
use crate::manager::LifecycleManager;
use crate::stages::{DemountStage, LifecycleState};
use plomid_core::{CatalogVersion, ErrorKind, GenerationId, PlomidError};
use plomid_storage::checkpoint::{create_checkpoint, latest_valid, CheckpointRequest};
use plomid_storage::StorageManager;
use plomid_wal::recover_storage;

impl LifecycleManager {
    /// Demounts the storage following the fixed durability ordering:
    ///
    /// stop mutations → flush required state → checkpoint if required →
    /// sync → close resources.
    ///
    /// The manager enters [`LifecycleState::Closed`] only after every stage
    /// succeeded; on failure it enters [`LifecycleState::Failed`] and the
    /// stage-attributed error is preserved. Owned handles are always dropped.
    pub fn demount(&mut self) -> std::result::Result<(), DemountError> {
        if self.state != LifecycleState::Ready {
            return Err(DemountError::new(
                DemountStage::Lifecycle,
                PlomidError::with_detail(
                    ErrorKind::Conflict,
                    "demount requires a mounted (Ready) storage lifecycle",
                    format!("state={}", self.state),
                ),
            ));
        }
        self.demount_trace.clear();

        // 1. STOP MUTATIONS: the mutation gate begins rejecting operations.
        self.state = LifecycleState::Demounting;
        self.demount_trace.push(DemountStage::StopMutations);

        match self.demount_sequence() {
            Ok(()) => {
                // 5. CLOSE RESOURCES: drop owned handles.
                self.wal = None;
                self.storage = None;
                self.generations = None;
                self.demount_trace.push(DemountStage::Close);
                self.state = LifecycleState::Closed;
                self.active_generation = None;
                self.catalog = None;
                self.recovery = None;
                Ok(())
            }
            Err(error) => {
                self.state = LifecycleState::Failed;
                Err(error)
            }
        }
    }

    /// Demount stages 2-4 (flush, checkpoint if required, sync).
    fn demount_sequence(&mut self) -> std::result::Result<(), DemountError> {
        // 2. FLUSH REQUIRED STATE through the existing mechanisms: first bring
        //    the storage image up to the durable WAL (replay is idempotent and
        //    converges), so the later checkpoint can never name WAL records
        //    that the storage does not yet represent; then flush the segment
        //    buffer pools and WAL writers. No new fsync policy.
        {
            let root = self
                .root
                .clone()
                .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "storage root is not set"))
                .map_err(|error| DemountError::new(DemountStage::Flush, error))?;
            let storage = self
                .required_storage()
                .map_err(|error| DemountError::new(DemountStage::Flush, error))?;
            recover_storage(&root, storage)
                .map_err(|error| DemountError::new(DemountStage::Flush, error))?;
            let wal = self
                .required_wal()
                .map_err(|error| DemountError::new(DemountStage::Flush, error))?;
            wal.sync()
                .map_err(|error| DemountError::new(DemountStage::Flush, error))?;
        }
        self.required_storage()
            .map_err(|error| DemountError::new(DemountStage::Flush, error))?
            .sync()
            .map_err(|error| DemountError::new(DemountStage::Flush, error))?;
        self.demount_trace.push(DemountStage::Flush);

        // 3. CHECKPOINT IF REQUIRED: while the WAL holds records, publish the
        //    durable WAL boundary first, then capture it in a storage
        //    checkpoint. The checkpoint LSN can never exceed the durable WAL.
        self.demount_checkpoint()?;
        self.demount_trace.push(DemountStage::Checkpoint);

        // 4. SYNC: final durability boundary after checkpoint publication.
        {
            let wal = self
                .required_wal()
                .map_err(|error| DemountError::new(DemountStage::Sync, error))?;
            wal.sync()
                .map_err(|error| DemountError::new(DemountStage::Sync, error))?;
        }
        self.required_storage()
            .map_err(|error| DemountError::new(DemountStage::Sync, error))?
            .sync()
            .map_err(|error| DemountError::new(DemountStage::Sync, error))?;
        self.demount_trace.push(DemountStage::Sync);
        Ok(())
    }
}

impl LifecycleManager {
    /// Checkpoint stage of demount. Skipped only when the WAL holds no
    /// records at all, because there is no durable state to bound.
    fn demount_checkpoint(&mut self) -> std::result::Result<(), DemountError> {
        let root = self
            .root
            .clone()
            .ok_or_else(|| PlomidError::new(ErrorKind::Internal, "storage root is not set"))
            .map_err(|error| DemountError::new(DemountStage::Checkpoint, error))?;
        let wal = self
            .required_wal()
            .map_err(|error| DemountError::new(DemountStage::Checkpoint, error))?;
        if wal.next_lsn().get() <= 1 {
            return Ok(());
        }
        let boundary = wal
            .checkpoint_with_watermarks(0, 0)
            .map_err(|error| DemountError::new(DemountStage::Checkpoint, error))?;
        // Carry the catalog identity forward: the catalog does not change
        // during demount, and checkpoint metadata must never move it
        // backwards relative to the latest valid checkpoint.
        let catalog_generation = latest_valid(&root)
            .map_err(|error| DemountError::new(DemountStage::Checkpoint, error))?
            .map_or(CatalogVersion::new(0), |(_, previous)| {
                previous.catalog_generation
            });
        let storage_generation = self
            .storage
            .as_ref()
            .map(StorageManager::storage_generation)
            .unwrap_or_else(|| GenerationId::new(1));
        create_checkpoint(
            &root,
            CheckpointRequest {
                checkpoint_lsn: boundary,
                storage_generation,
                catalog_generation,
                metadata: Vec::new(),
            },
        )
        .map(|_| ())
        .map_err(|error| DemountError::new(DemountStage::Checkpoint, error))?;
        Ok(())
    }
}
