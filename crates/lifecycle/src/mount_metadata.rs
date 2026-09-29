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
//! Mount stage 3: durable metadata validation, and dispatch to the remaining
//! mount stages.

use crate::errors::MountError;
use crate::manager::LifecycleManager;
use crate::stages::MountStage;
use plomid_core::{ErrorKind, GenerationId, Lsn};
use plomid_storage::generation::{load_publication_pointer, PublicationPointer};
use plomid_storage::StorageManager;
use plomid_wal::{durable_lsn, WAL_DIR_NAME};
use std::path::Path;

impl LifecycleManager {
    /// Mount stages 3-7, run after the device is open and validated.
    pub(crate) fn mount_steps_after_validation(
        &mut self,
        root: &Path,
        storage: &mut StorageManager,
        image_generation: GenerationId,
    ) -> std::result::Result<(), MountError> {
        // 3. VALIDATE METADATA: durable metadata files mount trusts. A WAL
        //    checkpoint marker or publication pointer with a broken checksum
        //    is corruption, never an absence.
        let wal_dir = root.join(WAL_DIR_NAME);
        let durable = durable_lsn(&wal_dir)
            .map_err(|error| MountError::new(MountStage::MetadataValidation, error))?;
        let pointer = match load_publication_pointer(root) {
            Ok(pointer) => Some(pointer),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(MountError::new(MountStage::MetadataValidation, error));
            }
        };
        self.mount_trace.push(MountStage::MetadataValidation);
        self.mount_steps_after_metadata(root, storage, image_generation, durable, pointer)
    }

    /// Mount stages 4-7, run after metadata validation.
    fn mount_steps_after_metadata(
        &mut self,
        root: &Path,
        storage: &mut StorageManager,
        image_generation: GenerationId,
        durable: Option<Lsn>,
        pointer: Option<PublicationPointer>,
    ) -> std::result::Result<(), MountError> {
        self.mount_steps_checkpoint_and_recovery(root, storage, image_generation, durable)?;
        self.mount_steps_catalog_and_generation(root, image_generation, pointer)
    }
}
