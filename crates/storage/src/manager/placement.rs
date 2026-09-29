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
//! Physical placement of database storage segments inside registered devices.
//!
//! Segments are the durable unit of the key-value storage engine: each segment
//! is a B+Tree file whose bytes live in exactly one device's `packs/` directory.
//! Placement is derived from the device layout itself, so there is no root-level
//! manifest and no separate volume registry; a restart rediscovers the durable
//! segments by scanning the devices the database owns. A table never names a
//! device: the allocator chooses the device for each new segment, which keeps
//! logical objects independent from physical placement.
//!
//! Segments use `SEG-<20 digits>.dat` names, which are neither produced nor
//! read outside this module. Roots that still carry the legacy layout are
//! rejected by `crate::layout::legacy` instead of being silently used.

use crate::layout::DatabaseLayout;
use plomid_core::{DeviceId, ErrorKind, GenerationId, PlomidError, Result, SegmentId};
use std::path::{Path, PathBuf};

const SEGMENT_ID_DIGITS: usize = 20;
const SEGMENT_FILE_PREFIX: &str = "SEG-";
const SEGMENT_FILE_SUFFIX: &str = ".dat";

#[must_use]
pub(crate) fn segment_file_name(id: SegmentId) -> String {
    format!(
        "{SEGMENT_FILE_PREFIX}{:0width$}{SEGMENT_FILE_SUFFIX}",
        id.get(),
        width = SEGMENT_ID_DIGITS
    )
}

#[must_use]
pub(crate) fn segment_from_file_name(name: &str) -> Option<SegmentId> {
    let without_suffix = name.strip_suffix(SEGMENT_FILE_SUFFIX)?;
    let digits = without_suffix.strip_prefix(SEGMENT_FILE_PREFIX)?;
    if digits.len() != SEGMENT_ID_DIGITS || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let id = digits.parse::<u64>().ok()?;
    (id != 0).then_some(SegmentId::new(id))
}

/// Returns the pack path of a segment placed on a device.
///
/// The physical bytes live under `devices/D-…/packs/`, never under the logical
/// table tree in `objects/`.
#[must_use]
pub(crate) fn segment_path(layout: &DatabaseLayout, device: DeviceId, id: SegmentId) -> PathBuf {
    layout.device_packs_dir(device).join(segment_file_name(id))
}

/// One durable segment and the device that owns its bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct SegmentPlacement {
    pub id: SegmentId,
    pub device: DeviceId,
}

/// Discovers the durable segments of a database root, in identifier order.
///
/// Every registered device directory is scanned once per call; the result is a
/// deterministic function of the filesystem, so recovery and validation agree
/// without any auxiliary registry file.
pub(crate) fn discover_segment_placements(root: &Path) -> Result<Vec<SegmentPlacement>> {
    let layout = DatabaseLayout::new(root);
    let devices_dir = layout.devices_dir();
    if !devices_dir.exists() {
        return Ok(Vec::new());
    }
    let mut placements = Vec::new();
    for entry in std::fs::read_dir(&devices_dir).map_err(PlomidError::from)? {
        let entry = entry.map_err(PlomidError::from)?;
        let device = match device_from_dir_name(&entry.file_name().to_string_lossy()) {
            Some(device) => device,
            None => continue,
        };
        let packs = layout.device_packs_dir(device);
        if !packs.exists() {
            continue;
        }
        for pack in std::fs::read_dir(&packs).map_err(PlomidError::from)? {
            let pack = pack.map_err(PlomidError::from)?;
            let name = pack.file_name().to_string_lossy().into_owned();
            if let Some(id) = segment_from_file_name(&name) {
                placements.push(SegmentPlacement { id, device });
            }
        }
    }
    placements.sort_unstable();
    placements.dedup();
    Ok(placements)
}

/// Deterministic list of durable segment IDs present in a storage root.
///
/// Catalog and generation metadata reference physical structures by segment ID.
/// Validation resolves a reference against this listing so a reference can never
/// name a segment the durable image does not contain.
pub(crate) fn durable_segment_ids(root: &std::path::Path) -> Result<Vec<u64>> {
    let mut ids: Vec<u64> = discover_segment_placements(root)?
        .into_iter()
        .map(|placement| placement.id.get())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Durable storage-image generation of a storage root on disk.
///
/// Reads only structural metadata: the device pack directory listings. No page,
/// block, or pack payload is read, so the cost is independent of database size
/// and of the number of stored records.
pub fn storage_generation(root: &std::path::Path) -> Result<GenerationId> {
    let highest = durable_segment_ids(root)?.into_iter().max();
    Ok(GenerationId::new(highest.unwrap_or(0)))
}

/// Parses a device directory name of the form `D-<20 digits>`.
fn device_from_dir_name(name: &str) -> Option<DeviceId> {
    let digits = name.strip_prefix("D-")?;
    if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let id = digits.parse::<u64>().ok()?;
    (id != 0).then_some(DeviceId::new(id))
}

/// Builds an error for a segment identifier that exhausted its space.
pub(crate) fn segment_id_exhausted() -> PlomidError {
    PlomidError::new(ErrorKind::Internal, "storage segment ID exhausted")
}
