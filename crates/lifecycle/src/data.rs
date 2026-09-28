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
//! Storage and WAL operations served by a mounted (Ready) manager.

use crate::manager::LifecycleManager;
use plomid_core::{CatalogVersion, GenerationId, Lsn, Result};
use plomid_storage::checkpoint::{create_checkpoint, latest_valid, CheckpointRequest};
use plomid_storage::StorageManager;
use plomid_wal::RecordType;

impl LifecycleManager {
    /// Inserts or replaces `value` for `key`, flushing the segment durably.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.require_ready("insert")?;
        self.required_storage()?.insert(key, value)
    }

    /// Applies an insert without flushing the segment; see
    /// [`StorageManager::insert_buffered`]. Used by the WAL-before-data write
    /// path, which is responsible for its own flush ordering.
    pub fn insert_buffered(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.require_ready("insert_buffered")?;
        self.required_storage()?.insert_buffered(key, value)
    }

    /// Returns the value for `key`, if present.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.require_ready("get")?;
        self.required_storage()?.get(key)
    }

    /// Removes `key`; returns whether it was present.
    pub fn delete(&mut self, key: &[u8]) -> Result<bool> {
        self.require_ready("delete")?;
        self.required_storage()?.delete(key)
    }

    /// Returns the key/value pairs in the given range.
    pub fn range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.require_ready("range")?;
        self.required_storage()?.range(start, end)
    }

    /// Flushes all data segments durably.
    pub fn sync(&mut self) -> Result<()> {
        self.require_ready("sync")?;
        self.required_storage()?.sync()
    }

    /// Appends one record to the WAL and makes it durable through the
    /// existing group-commit barrier. Callers own the record encoding.
    pub fn wal_append(&mut self, record_type: RecordType, payload: &[u8]) -> Result<Lsn> {
        self.require_ready("wal_append")?;
        let wal = self.required_wal()?;
        let lsn = wal.append(record_type, payload)?;
        wal.commit(lsn)?;
        Ok(lsn)
    }

    /// Makes every record through `lsn` durable (group-commit barrier).
    pub fn wal_commit(&mut self, lsn: Lsn) -> Result<()> {
        self.require_ready("wal_commit")?;
        self.required_wal()?.commit(lsn)
    }

    /// Runs a full checkpoint: flush the storage, publish the durable WAL
    /// boundary, and capture it in a storage checkpoint. Mirrors the existing
    /// engine checkpoint ordering without retention policy.
    pub fn checkpoint(&mut self) -> Result<Lsn> {
        self.require_ready("checkpoint")?;
        self.required_storage()?.sync()?;
        let boundary = self.required_wal()?.checkpoint_with_watermarks(0, 0)?;
        let root = self.required_root()?;
        let catalog_generation = latest_valid(root)?
            .map_or(CatalogVersion::new(0), |(_, previous)| {
                previous.catalog_generation
            });
        let storage_generation = self
            .storage
            .as_ref()
            .map(StorageManager::storage_generation)
            .unwrap_or_else(|| GenerationId::new(1));
        create_checkpoint(
            root,
            CheckpointRequest {
                checkpoint_lsn: boundary,
                storage_generation,
                catalog_generation,
                metadata: Vec::new(),
            },
        )?;
        Ok(boundary)
    }

    /// Durable storage-image generation of the mounted storage.
    pub fn storage_generation(&mut self) -> Result<GenerationId> {
        self.require_ready("storage_generation")?;
        Ok(self.required_storage()?.storage_generation())
    }
}
