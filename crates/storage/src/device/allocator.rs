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
//! Device allocator: which physical device receives new storage.
//!
//! The allocator is the *only* component that picks a device for new physical
//! storage. It takes a [DeviceRegistry] (the set of known devices) and chooses
//! an eligible device for an allocation request. It never writes pages itself:
//! that is the physical storage layer's job — it returns a device handle so the
//! caller can allocate within that device.
//!
//! Selection is capacity-first: the device with the most free space that can
//! satisfy the request wins. This keeps large allocations on large devices and
//! avoids fragmenting small devices when larger ones are available.

use crate::device::metadata::DeviceRecord;
use crate::device::physical::StorageDevice;
use crate::device::registry::DeviceRegistry;
use crate::physical::EXTENT_SIZE;
use plomid_core::{DeviceId, ErrorKind, PlomidError, Result};

/// Rounds a byte request up to the granularity the device allocator supports.
///
/// A device allocates contiguous extents, so a placement request commits a
/// whole number of extents. Rounding up here keeps one rule in one place: a
/// caller asks for the bytes it needs and the device layer decides what a
/// satisfiable request is.
pub fn round_up_to_extent(bytes: u64) -> Result<u64> {
    if bytes == 0 {
        return Err(PlomidError::new(
            ErrorKind::InvalidArgument,
            "allocation request is zero",
        ));
    }
    bytes
        .div_ceil(EXTENT_SIZE)
        .max(1)
        .checked_mul(EXTENT_SIZE)
        .ok_or_else(|| {
            PlomidError::new(
                ErrorKind::InvalidArgument,
                "allocation request exceeds the addressable device space",
            )
        })
}

/// A device chosen by the allocator for a new allocation.
pub struct AllocationTarget {
    /// Logical identity of the chosen device.
    pub device_id: DeviceId,
    /// Durable record of the chosen device.
    pub record: DeviceRecord,
    /// The opened physical device handle, ready for allocation.
    pub device: StorageDevice,
}

impl std::fmt::Debug for AllocationTarget {
    /// Reports the logical choice without exposing the open physical handle,
    /// which has no meaningful textual form.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AllocationTarget")
            .field("device_id", &self.device_id)
            .field("capacity", &self.record.capacity)
            .finish_non_exhaustive()
    }
}

/// Chooses which registered device receives new physical storage.
///
/// The allocator consults the registry for the current device set and the
/// physical device for live free-space accounting. It never assumes a fixed
/// device: all devices are discovered from the registry, and a full device
/// simply makes another eligible device the selection.
#[derive(Clone, Debug, Default)]
pub struct DeviceAllocator;

impl DeviceAllocator {
    /// Creates a new allocator.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Selects a device that can satisfy an allocation of `length` bytes.
    ///
    /// The registry provides the set of known devices and where each one lives;
    /// the physical device provides live free-space. Selection spans every
    /// usable device: the one with the most remaining free bytes that can fit
    /// the request wins, so a full device simply makes another eligible device
    /// the selection. If no device can satisfy the request, a controlled
    /// capacity error is returned.
    pub fn select_device(
        &self,
        registry: &DeviceRegistry,
        length: u64,
    ) -> Result<AllocationTarget> {
        if length == 0 {
            return Err(PlomidError::new(
                ErrorKind::InvalidArgument,
                "allocation length is zero",
            ));
        }
        let mut best: Option<AllocationTarget> = None;
        let mut best_free = 0u64;
        for entry in registry.iter() {
            if !entry.is_usable() {
                continue;
            }
            if entry.record.capacity < length {
                // Device cannot hold the request even in theory.
                continue;
            }
            if !entry.physical_file.is_file() {
                // A registered device whose physical container is missing cannot
                // be accounted for: skipping it silently would make the database
                // appear smaller than it is, so placement fails loudly with the
                // device and the path the operator must restore.
                return Err(PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "registered device has no physical container",
                    format!(
                        "device_id={} path={}",
                        entry.device_id.get(),
                        entry.physical_file.display()
                    ),
                ));
            }
            let device = StorageDevice::open(&entry.physical_file).map_err(|error| {
                PlomidError::with_detail(
                    ErrorKind::Corruption,
                    "device could not be opened for allocation",
                    format!(
                        "device_id={} error={}",
                        entry.device_id.get(),
                        error.message()
                    ),
                )
            })?;
            let free = device.free_bytes();
            if free >= length && free > best_free {
                best_free = free;
                best = Some(AllocationTarget {
                    device_id: entry.device_id,
                    record: entry.record.clone(),
                    device,
                });
            }
        }
        match best {
            Some(target) => Ok(target),
            None => Err(PlomidError::with_detail(
                // Exhausting physical capacity is a conflict with current
                // device state, not an I/O fault: the request is well formed
                // but no registered device can satisfy it.
                ErrorKind::Conflict,
                "insufficient physical capacity across all devices",
                format!(
                    "requested_bytes={length} total_usable_bytes={}",
                    registry.usable_capacity()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::round_up_to_extent;
    use crate::physical::EXTENT_SIZE;
    use plomid_core::ErrorKind;

    #[test]
    fn requests_are_rounded_up_to_whole_extents() {
        assert_eq!(round_up_to_extent(1).expect("small"), EXTENT_SIZE);
        assert_eq!(round_up_to_extent(EXTENT_SIZE).expect("exact"), EXTENT_SIZE);
        assert_eq!(
            round_up_to_extent(EXTENT_SIZE + 1).expect("over"),
            2 * EXTENT_SIZE
        );
    }

    #[test]
    fn unsatisfiable_requests_are_rejected() {
        assert_eq!(
            round_up_to_extent(0).expect_err("zero").kind(),
            ErrorKind::InvalidArgument
        );
        assert!(round_up_to_extent(u64::MAX).is_err());
    }
}
