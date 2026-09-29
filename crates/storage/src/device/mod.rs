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
//! Physical storage devices and how logical objects are placed on them.
//!
//! A device is a *directory* inside the database root:
//!
//! ```text
//! PLOMID_DATA/devices/D-0000000000000001/
//! ├── DEVICE.dat   durable device identity and capacity
//! ├── packs/       the physical storage containers of the device
//! ├── free/        free-space accounting used by device placement
//! └── state/       device lifecycle state
//! ```
//!
//! # Responsibilities
//!
//! * `metadata` – the durable `DEVICE.dat` record (identity and capacity).
//! * `state` – the durable `state/STATE.dat` record (device lifecycle state).
//! * `registration` – materializes a device directory and validates an
//!   existing registration without ever rewriting it.
//! * `registry` – which devices exist, where they live, and how much room they
//!   have left. It resolves `DeviceId → device`, and it never chooses a device
//!   for data.
//! * `allocator` – chooses which registered device receives new physical
//!   storage. It is the only component that decides placement.
//! * `physical` – the existing lower-level device container implementation
//!   (single pre-sized device file with a persistent extent allocator). It stays
//!   authoritative for its own format and allocator.
//!
//! # One database, many devices
//!
//! Nothing here assumes a particular device: devices are discovered from the
//! layout, a database may register any number of them, and one logical table can
//! span several devices. The capacity of the database is the sum of its device
//! capacities, so a full device never becomes the limit of the database: the
//! allocator simply selects another eligible device, and reports a controlled
//! capacity error only when every device is exhausted.

mod allocator;
mod metadata;
mod physical;
mod registration;
mod registry;
mod state;

pub use allocator::{round_up_to_extent, AllocationTarget, DeviceAllocator};
pub use metadata::{
    device_record_path, read_device_record, read_device_record_optional, DeviceRecord,
    DEVICE_META_HEADER_LEN, DEVICE_META_MAGIC, DEVICE_META_VERSION,
};
pub use registration::{default_device_capacity, register_device};
pub use registry::{DeviceEntry, DeviceRegistry};
pub use state::{
    read_device_state, read_device_state_optional, DeviceStateRecord, DEVICE_STATE_HEADER_LEN,
    DEVICE_STATE_MAGIC, DEVICE_STATE_VERSION,
};

// The lower-level device container keeps its public surface: storage code that
// already uses it, and this crate's public API, are unchanged by the move.
pub use physical::{
    capacity_for_extents, validate_capacity, DeviceMetadata, Extent, Lifecycle, StorageDevice,
    ALLOCATOR_FORMAT_VERSION, ALLOCATOR_MAGIC, DEVICE_FORMAT_VERSION, DEVICE_HEADER_LEN,
    DEVICE_MAGIC, MAX_DEVICE_CAPACITY, MIN_DEVICE_CAPACITY, SNAPSHOT_ENTRY_LEN, SNAPSHOT_PAGE_LEN,
    SNAPSHOT_PREFIX_LEN,
};
