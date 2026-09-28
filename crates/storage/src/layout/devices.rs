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
//! Device directories: `devices/D-<device id>/`.
//!
//! A device directory is the physical storage boundary of one device:
//!
//! ```text
//! devices/D-0000000000000001/
//! ├── DEVICE.dat   durable device identity and capacity
//! ├── packs/       physical storage containers owned by the device
//! ├── free/        free-space accounting of the device allocator
//! └── state/       device lifecycle state
//! ```
//!
//! The layout owns the names and paths of a device directory, and nothing more.
//! Which device receives new physical storage is decided by the device
//! allocator, the bytes themselves are written by the physical storage layer,
//! and the meaning of `DEVICE.dat` and `state/STATE.dat` belongs to the device
//! module. No path helper here hard-codes a device: every function is a pure
//! function of the device identity it is given.

use super::names::{parse_layout_id, render_prefixed_id};
use super::DatabaseLayout;
use crate::durable;
use plomid_core::{DeviceId, PlomidError, Result};
use std::path::PathBuf;

/// Prefix of a device directory name.
pub const DEVICE_DIR_PREFIX: &str = "D-";
/// Device identity and capacity record.
pub const DEVICE_META_FILE_NAME: &str = "DEVICE.dat";
/// Directory holding a device's physical storage containers.
pub const DEVICE_PACKS_DIR_NAME: &str = "packs";
/// Directory holding a device's free-space accounting.
pub const DEVICE_FREE_DIR_NAME: &str = "free";
/// Directory holding a device's lifecycle state.
pub const DEVICE_STATE_DIR_NAME: &str = "state";
/// Device lifecycle state record.
pub const DEVICE_STATE_FILE_NAME: &str = "STATE.dat";
/// Pre-sized physical container file of a device, inside its pack directory.
///
/// The physical database bytes live in `packs/`, never under `objects/`: the
/// device directory is the physical storage boundary, and `objects/` only
/// describes logical structure.
pub const DEVICE_PHYSICAL_FILE_NAME: &str = "PHYSICAL.dat";

/// Renders the directory name of a device.
#[must_use]
pub fn device_dir_name(device_id: DeviceId) -> String {
    render_prefixed_id(DEVICE_DIR_PREFIX, device_id.get())
}

/// Parses a device directory name. Only the deterministic form is accepted.
#[must_use]
pub fn device_from_dir_name(name: &str) -> Option<DeviceId> {
    parse_layout_id(name, DEVICE_DIR_PREFIX).map(DeviceId::new)
}

impl DatabaseLayout {
    /// Logical directory of one device.
    #[must_use]
    pub fn device_dir(&self, device_id: DeviceId) -> PathBuf {
        self.devices_dir().join(device_dir_name(device_id))
    }

    /// Identity and capacity record of one device.
    #[must_use]
    pub fn device_meta_path(&self, device_id: DeviceId) -> PathBuf {
        self.device_dir(device_id).join(DEVICE_META_FILE_NAME)
    }

    /// Directory holding the physical storage containers of one device.
    #[must_use]
    pub fn device_packs_dir(&self, device_id: DeviceId) -> PathBuf {
        self.device_dir(device_id).join(DEVICE_PACKS_DIR_NAME)
    }

    /// Directory holding the free-space accounting of one device.
    #[must_use]
    pub fn device_free_dir(&self, device_id: DeviceId) -> PathBuf {
        self.device_dir(device_id).join(DEVICE_FREE_DIR_NAME)
    }

    /// Directory holding the lifecycle state of one device.
    #[must_use]
    pub fn device_state_dir(&self, device_id: DeviceId) -> PathBuf {
        self.device_dir(device_id).join(DEVICE_STATE_DIR_NAME)
    }

    /// Lifecycle state record of one device.
    #[must_use]
    pub fn device_state_path(&self, device_id: DeviceId) -> PathBuf {
        self.device_state_dir(device_id)
            .join(DEVICE_STATE_FILE_NAME)
    }

    /// Physical container file of one device, inside its pack directory.
    ///
    /// This is the file that owns the device's actual database bytes; logical
    /// metadata never lives here.
    #[must_use]
    pub fn device_physical_path(&self, device_id: DeviceId) -> PathBuf {
        self.device_packs_dir(device_id)
            .join(DEVICE_PHYSICAL_FILE_NAME)
    }

    /// Creates the directory structure of one device, idempotently.
    ///
    /// Only directories are created; the device records themselves are written
    /// by the device module, which owns their format and their semantics.
    pub fn ensure_device_dirs(&self, device_id: DeviceId) -> Result<()> {
        durable::ensure_dir(&self.device_dir(device_id))?;
        durable::ensure_dir(&self.device_packs_dir(device_id))?;
        durable::ensure_dir(&self.device_free_dir(device_id))?;
        durable::ensure_dir(&self.device_state_dir(device_id))?;
        Ok(())
    }

    /// Enumerates registered device directories in deterministic identity order.
    pub fn discover_device_ids(&self) -> Result<Vec<DeviceId>> {
        let dir = self.devices_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(PlomidError::from)? {
            let entry = entry.map_err(PlomidError::from)?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(device_id) = device_from_dir_name(&name) {
                ids.push(device_id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::{device_dir_name, device_from_dir_name};
    use plomid_core::DeviceId;

    #[test]
    fn directory_names_round_trip_through_identity() {
        assert_eq!(device_dir_name(DeviceId::new(1)), "D-00000000000000000001");
        assert_eq!(
            device_from_dir_name("D-00000000000000000002"),
            Some(DeviceId::new(2))
        );
        assert_eq!(device_from_dir_name("D-1"), None);
        assert_eq!(device_from_dir_name("T-00000000000000000001"), None);
    }
}
