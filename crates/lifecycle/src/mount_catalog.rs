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
//! Mount stages 6 (catalog) and 7 (active generation).

use crate::errors::MountError;
use crate::manager::LifecycleManager;
use crate::stages::MountStage;
use plomid_core::{ErrorKind, GenerationId, PlomidError};
use plomid_storage::catalog::latest_valid_catalog;
use plomid_storage::generation::{GenerationManager, PublicationPointer};
use std::path::Path;

impl LifecycleManager {
    /// Mount stages 6 (catalog) and 7 (generation).
    pub(crate) fn mount_steps_catalog_and_generation(
        &mut self,
        root: &Path,
        image_generation: GenerationId,
        pointer: Option<PublicationPointer>,
    ) -> std::result::Result<(), MountError> {
        // 6. LOAD CATALOG: the published state is recovered and validated by
        //    the existing generation manager. A store that never published a
        //    catalog and holds no checkpoint that names one has no catalog
        //    metadata at all; that is a fresh store, not a corruption.
        let has_published_state = pointer.is_some()
            || latest_valid_catalog(root)
                .map_err(|error| MountError::new(MountStage::Catalog, error))?
                .is_some();
        if has_published_state {
            let generations = GenerationManager::open(root)
                .map_err(|error| MountError::new(MountStage::Catalog, error))?;
            let catalog = match &pointer {
                Some(published) => {
                    if published.storage_generation > image_generation {
                        return Err(MountError::new(
                            MountStage::Catalog,
                            PlomidError::with_detail(
                                ErrorKind::Corruption,
                                "published catalog references a storage generation newer than the image",
                                format!(
                                    "published={} image={}",
                                    published.storage_generation.get(),
                                    image_generation.get()
                                ),
                            ),
                        ));
                    }
                    Some(
                        generations
                            .load()
                            .map_err(|error| MountError::new(MountStage::Catalog, error))?,
                    )
                }
                None => None,
            };
            self.catalog = catalog;
            self.generations = Some(generations);
        }
        self.mount_trace.push(MountStage::Catalog);

        // 7. ESTABLISH ACTIVE GENERATION: the durable image generation; a
        //    mount never creates a new generation.
        if image_generation.is_zero() {
            return Err(MountError::new(
                MountStage::Generation,
                PlomidError::new(
                    ErrorKind::Corruption,
                    "mounted image has no durable segment",
                ),
            ));
        }
        self.mount_trace.push(MountStage::Generation);

        self.active_generation = Some(image_generation);
        Ok(())
    }
}
