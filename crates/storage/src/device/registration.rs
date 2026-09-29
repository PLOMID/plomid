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
//! Device registration: materializing one device directory.
//!
//! Registering a device creates its complete durable structure:
//!
//! ```text
//! devices/D-0000000000000001/
//! ├── DEVICE.dat   identity and capacity, declared here
//! ├── packs/       physical storage of the device (container and pack files)
//! ├── free/        allocator free-space accounting
//! └── state/       lifecycle state, published as online
//! ```
//!
//! # Idempotence and identity
//!
//! Registration is idempotent and never rewrites durable state. An existing
//! `DEVICE.dat` is validated against the requested identity and capacity; a
//! mismatch is a controlled error rather than a silent overwrite, because
//! rewriting the record of a device that already holds data would change what
//! the database believes about its own physical capacity.
//!
//! A device identity is logical: it comes from the caller and is recorded in
//! `DEVICE.dat`, so the same bytes moved to another path remain the same device
//! and a renamed directory can never be mistaken for a different one.
//!
//! The device container is created here too, so a registered device is
//! immediately usable for physical placement. It is created once and reopened
//! afterwards, never truncated.

use crate::device::metadata::{read_device_record_optional, DeviceRecord};
use crate::device::physical::{capacity_for_extents, Lifecycle, StorageDevice};
use crate::device::state::{read_device_state_optional, DeviceStateRecord};
use crate::layout::DatabaseLayout;
use plomid_core::{DeviceId, ErrorKind, PlomidError, Result};

/// Extents of the device a database creates for itself when it is created.
///
/// A database cannot store anything without physical capacity, so creation
/// registers one device. The capacity is declared rather than discovered: a
/// device is the physical capacity the operator grants to the database, and
/// further devices are registered explicitly with their own capacity. The
/// default is expressed in extents so it always satisfies the device
/// container's own alignment rule.
const DEFAULT_DEVICE_EXTENTS: u64 = 1 << 13;

/// Capacity of the device a database creates for itself when it is created.
///
/// The value follows the device container's alignment rule, so creating the
/// default device can never fail for an alignment reason.
pub fn default_device_capacity() -> Result<u64> {
    capacity_for_extents(DEFAULT_DEVICE_EXTENTS)
}

/// Registers a device and its storage, or validates an existing registration.
///
/// Returns a controlled error when the directory already holds a different
/// device, when the recorded capacity disagrees with the requested one, or when
/// the device structure cannot be created.
pub fn register_device(layout: &DatabaseLayout, device_id: DeviceId, capacity: u64) -> Result<()> {
    if device_id.is_zero() {
        return Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "device identity is zero",
        ));
    }
    let meta_path = layout.device_meta_path(device_id);
    match read_device_record_optional(&meta_path)? {
        Some(existing) => {
            if existing.device_id != device_id {
                return Err(PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "device directory already holds a different device",
                    format!(
                        "device_id={} on_disk={}",
                        device_id.get(),
                        existing.device_id.get()
                    ),
                ));
            }
            if existing.capacity != capacity {
                return Err(PlomidError::with_detail(
                    ErrorKind::Conflict,
                    "device is already registered with a different capacity",
                    format!(
                        "device_id={} registered={} requested={}",
                        device_id.get(),
                        existing.capacity,
                        capacity
                    ),
                ));
            }
        }
        None => {
            // Directories are created before the record is published, so a
            // recorded device can never lack the directories it requires.
            layout.ensure_device_dirs(device_id)?;
            DeviceRecord::new(device_id, capacity)?.publish(&meta_path)?;
        }
    }

    // A freshly registered device starts online, which is what makes it
    // eligible for placement without a separate activation step.
    let state_path = layout.device_state_path(device_id);
    if read_device_state_optional(&state_path)?.is_none() {
        DeviceStateRecord::new(Lifecycle::Online).publish(&state_path)?;
    }

    // The device container is the device's own pre-sized physical file. It is
    // created once: an existing container is left untouched, including every
    // extent it already owns.
    let container = layout.device_physical_path(device_id);
    if !container.exists() {
        StorageDevice::create(&container, device_id, capacity)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::{default_device_capacity, register_device};
    use crate::device::physical::capacity_for_extents;
    use crate::device::registry::DeviceRegistry;
    use crate::layout::DatabaseLayout;
    use plomid_core::{DeviceId, ErrorKind};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-device-registration-{label}-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn registration_is_idempotent() {
        let root = scratch("idempotent");
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        let device_id = DeviceId::new(1);
        let capacity = default_device_capacity().expect("default capacity");
        register_device(&layout, device_id, capacity).expect("first");
        register_device(&layout, device_id, capacity).expect("second");
        let registry = DeviceRegistry::discover(&layout).expect("discover");
        assert_eq!(registry.len(), 1);
        assert!(layout.device_physical_path(device_id).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_device_keeps_its_identity_and_capacity() {
        let root = scratch("conflict");
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        let device_id = DeviceId::new(1);
        let capacity = capacity_for_extents(1).expect("capacity");
        register_device(&layout, device_id, capacity).expect("register");
        let error = register_device(&layout, device_id, capacity * 4).expect_err("resize");
        assert_eq!(error.kind(), ErrorKind::Conflict);
        let registry = DeviceRegistry::discover(&layout).expect("discover");
        assert_eq!(registry.get(device_id).expect("entry").capacity(), capacity);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_zero_device_identity_is_rejected() {
        let root = scratch("zero");
        let layout = DatabaseLayout::new(&root);
        layout.initialize().expect("initialize");
        assert!(register_device(
            &layout,
            DeviceId::new(0),
            default_device_capacity().expect("cap")
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
