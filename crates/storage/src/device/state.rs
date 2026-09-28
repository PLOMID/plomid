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
//! Durable device lifecycle state record.
//!
//! `DeviceStateRecord` lives in `devices/D-*/state/STATE.dat`. It mirrors the
//! lifecycle stored in the physical device header, but is kept here as the
//! registry-side durable marker so the device lifecycle survives a restart
//! before the physical device is opened. This avoids depending on reading the
//! full physical header just to know whether a device is online.
//!
//! Record format (little-endian). Every field has an explicit offset and width:
//!
//! ```text
//! offset  size  field
//!      0     4  magic       "PLS1", device state tag
//!      4     4  version     device state format version
//!      8     4  header_len  total bytes of this record (28)
//!     12     4  state       Lifecycle discriminant
//!     16     8  updated_at  update stamp (seconds since the Unix epoch)
//!     24     4  checksum    CRC32C over bytes[0..24]
//! ```
//!
//! The lifecycle values are exactly those of [`Lifecycle`]: the state record
//! and the physical header use the same on-disk discriminants, so the two
//! never disagree about what a number means.

use crate::checksum::{compute_checksum, verify_checksum};
use crate::codec::{put_u32, put_u64};
use crate::device::physical::Lifecycle;
use crate::layout::metadata as meta;
use plomid_core::Result;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Magic for every `STATE.dat` record.
pub const DEVICE_STATE_MAGIC: [u8; 4] = *b"PLS1";
/// Format version of the `STATE.dat` record.
pub const DEVICE_STATE_VERSION: u32 = 1;
/// Total on-disk length of a `STATE.dat` record.
pub const DEVICE_STATE_HEADER_LEN: usize = 28;
/// Byte offset of the checksum field within a `STATE.dat` record.
const CHECKSUM_OFFSET: usize = 24;

/// Durable marker of a device's lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceStateRecord {
    /// Lifecycle state of the device.
    pub state: Lifecycle,
    /// Update stamp (seconds since the Unix epoch).
    pub updated_at: u64,
}

impl DeviceStateRecord {
    /// Creates a record capturing the current moment in `state`.
    pub fn new(state: Lifecycle) -> Self {
        let updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self { state, updated_at }
    }

    /// Encodes the record deterministically.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(DEVICE_STATE_HEADER_LEN);
        meta::encode_prefix(
            &mut out,
            DEVICE_STATE_MAGIC,
            DEVICE_STATE_VERSION,
            DEVICE_STATE_HEADER_LEN as u32,
        );
        put_u32(&mut out, self.state as u32);
        put_u64(&mut out, self.updated_at);
        // The checksum is the last field and covers every preceding byte.
        let checksum = compute_checksum(&out[..CHECKSUM_OFFSET]);
        put_u32(&mut out, checksum);
        Ok(out)
    }

    /// Decodes and validates a record image.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (mut cursor, header_len) = meta::decode_prefix(
            bytes,
            DEVICE_STATE_MAGIC,
            DEVICE_STATE_VERSION,
            "device state",
        )?;
        meta::ensure_header_len(bytes, header_len, "device state")?;
        let state = Lifecycle::from_u32(cursor.u32("device state")?)?;
        let updated_at = cursor.u64("device state")?;
        let stored = cursor.u32("device state")?;
        verify_checksum(&bytes[..CHECKSUM_OFFSET], stored)?;
        meta::ensure_reserved_zero(&bytes[cursor.position()..header_len], "device state")?;
        Ok(Self { state, updated_at })
    }

    /// Publishes the record atomically at `path`.
    pub fn publish(&self, path: &Path) -> Result<()> {
        let bytes = self.encode()?;
        // VERIFY: a record that cannot be decoded is never published.
        Self::decode(&bytes)?;
        meta::publish(path, &bytes)
    }
}

/// Reads a device state record, reporting absence as `ErrorKind::NotFound`.
pub fn read_device_state(path: &Path) -> Result<DeviceStateRecord> {
    let bytes = meta::read(path, "device state")?;
    DeviceStateRecord::decode(&bytes)
}

/// Reads a device state record, reporting absence as `Ok(None)`.
pub fn read_device_state_optional(path: &Path) -> Result<Option<DeviceStateRecord>> {
    match meta::read_optional(path)? {
        Some(bytes) => DeviceStateRecord::decode(&bytes).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        read_device_state, DeviceStateRecord, DEVICE_STATE_HEADER_LEN, DEVICE_STATE_MAGIC,
        DEVICE_STATE_VERSION,
    };
    use crate::checksum::compute_checksum;
    use crate::device::physical::Lifecycle;
    use plomid_core::ErrorKind;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "plomid-device-state-{label}-{}-{id}",
            std::process::id(),
        ))
    }

    #[test]
    fn record_round_trips() {
        let record = DeviceStateRecord::new(Lifecycle::Online);
        let bytes = record.encode().expect("encode");
        assert_eq!(bytes.len(), DEVICE_STATE_HEADER_LEN);
        assert_eq!(DeviceStateRecord::decode(&bytes).expect("decode"), record);
    }

    #[test]
    fn corruption_is_detected() {
        let record = DeviceStateRecord::new(Lifecycle::Quiescing);
        let bytes = record.encode().expect("encode");

        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0xFF;
        assert!(DeviceStateRecord::decode(&bad_magic).is_err());

        let mut bad_version = bytes.clone();
        bad_version[4] = 0xFF;
        assert!(DeviceStateRecord::decode(&bad_version).is_err());

        let mut bad_checksum = bytes.clone();
        bad_checksum[24] ^= 0x01;
        assert!(DeviceStateRecord::decode(&bad_checksum).is_err());

        assert!(DeviceStateRecord::decode(&bytes[..10]).is_err());
    }

    #[test]
    fn invalid_state_value_is_rejected() {
        let mut bytes = vec![0u8; DEVICE_STATE_HEADER_LEN];
        bytes[0..4].copy_from_slice(&DEVICE_STATE_MAGIC);
        bytes[4..8].copy_from_slice(&DEVICE_STATE_VERSION.to_le_bytes());
        bytes[8..12].copy_from_slice(&(DEVICE_STATE_HEADER_LEN as u32).to_le_bytes());
        bytes[12..16].copy_from_slice(&999_u32.to_le_bytes());
        bytes[16..24].copy_from_slice(&1000_u64.to_le_bytes());
        let checksum = compute_checksum(&bytes[..24]);
        bytes[24..28].copy_from_slice(&checksum.to_le_bytes());
        assert!(DeviceStateRecord::decode(&bytes).is_err());
    }

    #[test]
    fn publish_and_read_round_trips() {
        let dir = scratch("publish");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("STATE.dat");
        let record = DeviceStateRecord::new(Lifecycle::Offline);
        record.publish(&path).expect("publish");
        let loaded = read_device_state(&path).expect("read");
        assert_eq!(loaded, record);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_state_reports_not_found() {
        let dir = scratch("missing");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("STATE.dat");
        let error = read_device_state(&path).expect_err("absent");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
