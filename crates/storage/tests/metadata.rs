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
//! Metadata tests: deterministic explicit encoding, corruption detection, and
//! the atomic publication boundary.
use plomid_core::{DeviceId, ErrorKind, GenerationId, IndexId, ObjectId, TableId};
use plomid_storage::layout::{DatabaseLayout, META_FILE_NAME};
use plomid_storage::{
    read_device_record, DeviceRecord, DeviceStateRecord, IndexMetadata, Lifecycle, TableMetadata,
    DEVICE_META_HEADER_LEN, DEVICE_STATE_HEADER_LEN,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-metadata-it-{label}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Requires that a decode of `bytes` fails as corruption.
fn assert_corrupt(bytes: &[u8], what: &str) {
    let error = DeviceRecord::decode(bytes).expect_err(what);
    assert_eq!(error.kind(), ErrorKind::Corruption, "{what}");
}

#[test]
fn device_record_encoding_is_deterministic_and_explicit() {
    let record = DeviceRecord::new(DeviceId::new(1), 1u64 << 40).expect("record");
    let first = record.encode().expect("encode");
    let second = record.encode().expect("encode");

    // Deterministic: the same record always produces the same bytes.
    assert_eq!(first, second);
    // Explicit width: the declared header length is the encoded length.
    assert_eq!(first.len(), DEVICE_META_HEADER_LEN);
    // Explicit magic and little-endian version, not a memory image.
    assert_eq!(&first[0..4], b"PLDV");
    assert_eq!(
        u32::from_le_bytes(first[4..8].try_into().expect("version")),
        1
    );
    assert_eq!(
        u32::from_le_bytes(first[8..12].try_into().expect("header len")) as usize,
        DEVICE_META_HEADER_LEN
    );
    assert_eq!(
        u64::from_le_bytes(first[12..20].try_into().expect("device id")),
        1
    );
    assert_eq!(
        u64::from_le_bytes(first[20..28].try_into().expect("capacity")),
        1u64 << 40
    );
    // The checksum is the last field, so every preceding byte is covered.
    let stored = u32::from_le_bytes(first[36..40].try_into().expect("checksum"));
    assert_eq!(
        stored,
        plomid_storage::compute_checksum(&first[..36]),
        "the checksum covers every byte that precedes it"
    );
    assert_eq!(DeviceRecord::decode(&first).expect("decode"), record);
}

#[test]
fn truncated_device_records_are_rejected() {
    let bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    // Every strict prefix is malformed.
    for len in 0..bytes.len() {
        assert_corrupt(&bytes[..len], "truncated record");
    }
}

#[test]
fn device_records_with_trailing_bytes_are_rejected() {
    let bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    let mut longer = bytes.clone();
    longer.push(0);
    assert_corrupt(&longer, "overlong record");
}

#[test]
fn device_records_with_invalid_magic_are_rejected() {
    let bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    let mut tampered = bytes.clone();
    tampered[0] ^= 0xFF;
    assert_corrupt(&tampered, "invalid magic");
}

#[test]
fn device_records_with_unsupported_versions_are_rejected() {
    let bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    for version in [0_u32, 2, u32::MAX] {
        let mut tampered = bytes.clone();
        tampered[4..8].copy_from_slice(&version.to_le_bytes());
        assert_corrupt(&tampered, "unsupported version");
    }
}

#[test]
fn device_records_with_impossible_header_lengths_are_rejected() {
    let bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    // A declared length below the framing prefix, and one beyond the image.
    for header_len in [0_u32, 8, 4, 4096, u32::MAX] {
        let mut tampered = bytes.clone();
        tampered[8..12].copy_from_slice(&header_len.to_le_bytes());
        assert_corrupt(&tampered, "impossible header length");
    }
}

#[test]
fn device_records_with_invalid_checksums_are_rejected() {
    let bytes = DeviceRecord::new(DeviceId::new(1), 1u64 << 40)
        .expect("record")
        .encode()
        .expect("encode");
    // Flipping any covered byte is detected.
    for offset in 0..36_usize {
        let mut tampered = bytes.clone();
        tampered[offset] ^= 0x01;
        assert_corrupt(&tampered, "covered byte tampered");
    }
    // Flipping the stored checksum itself is detected.
    let mut tampered = bytes.clone();
    tampered[36] ^= 0x01;
    assert_corrupt(&tampered, "checksum tampered");
}
#[test]
fn device_state_records_use_the_same_malformation_rules() {
    let record = DeviceStateRecord::new(Lifecycle::Online);
    let bytes = record.encode().expect("encode");
    assert_eq!(bytes.len(), DEVICE_STATE_HEADER_LEN);
    assert_eq!(&bytes[0..4], b"PLS1");
    // The lifecycle discriminants are shared with the physical header.
    assert_eq!(
        u32::from_le_bytes(bytes[12..16].try_into().expect("state")),
        2
    );
    let stored = u32::from_le_bytes(bytes[24..28].try_into().expect("checksum"));
    assert_eq!(stored, plomid_storage::compute_checksum(&bytes[..24]));

    for len in 0..bytes.len() {
        assert_eq!(
            DeviceStateRecord::decode(&bytes[..len])
                .expect_err("truncated")
                .kind(),
            ErrorKind::Corruption
        );
    }
    let mut bad_magic = bytes.clone();
    bad_magic[0] ^= 0xFF;
    assert!(DeviceStateRecord::decode(&bad_magic).is_err());
    let mut bad_version = bytes.clone();
    bad_version[4..8].copy_from_slice(&7_u32.to_le_bytes());
    assert!(DeviceStateRecord::decode(&bad_version).is_err());
    let mut bad_checksum = bytes.clone();
    bad_checksum[24] ^= 0x01;
    assert!(DeviceStateRecord::decode(&bad_checksum).is_err());
    // An unknown lifecycle discriminant is corruption, never a default state.
    let mut bad_state = bytes.clone();
    bad_state[12..16].copy_from_slice(&99_u32.to_le_bytes());
    let fixed = plomid_storage::compute_checksum(&bad_state[..24]);
    bad_state[24..28].copy_from_slice(&fixed.to_le_bytes());
    assert!(DeviceStateRecord::decode(&bad_state).is_err());
}

#[test]
fn table_and_index_records_reject_the_same_malformations() {
    let table = TableMetadata::new(TableId::new(1), ObjectId::new(1))
        .expect("table")
        .encode()
        .expect("encode");
    let index = IndexMetadata::new(IndexId::new(1), TableId::new(1))
        .expect("index")
        .encode()
        .expect("encode");

    for bytes in [&table, &index] {
        for len in 0..bytes.len() {
            // Every prefix is malformed for the respective decoder.
            assert!(
                TableMetadata::decode(&bytes[..len]).is_err()
                    && IndexMetadata::decode(&bytes[..len]).is_err(),
                "short images must be rejected by both decoders"
            );
        }
        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 0xFF;
        assert!(TableMetadata::decode(&bad_magic).is_err());
        assert!(IndexMetadata::decode(&bad_magic).is_err());
        let mut bad_version = bytes.clone();
        bad_version[4..8].copy_from_slice(&0_u32.to_le_bytes());
        assert!(TableMetadata::decode(&bad_version).is_err());
        assert!(IndexMetadata::decode(&bad_version).is_err());
        let mut bad_checksum = bytes.clone();
        bad_checksum[28] ^= 0x01;
        assert!(TableMetadata::decode(&bad_checksum).is_err());
        assert!(IndexMetadata::decode(&bad_checksum).is_err());
    }
}

#[test]
fn an_incomplete_publication_is_never_authoritative() {
    let root = scratch("staging");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);
    layout.ensure_table(table_id).expect("table");
    let published = layout.read_table_meta(table_id).expect("read");

    // A crash before the atomic rename leaves only the staging artifact. The
    // published record stays authoritative and discovery never sees the
    // partial image.
    let staged = layout.table_meta_path(table_id).with_extension("dat.tmp");
    std::fs::write(&staged, b"partial image").expect("write staged");
    assert_eq!(layout.read_table_meta(table_id).expect("read"), published);
    assert_eq!(
        layout.discover_table_ids().expect("discover"),
        vec![table_id],
        "the staging artifact is not a second table"
    );

    // A staging artifact at the root is never mistaken for `CURRENT`.
    std::fs::write(layout.current_staged_path(), b"partial").expect("write staged");
    assert!(!layout.current_exists());

    cleanup(&root);
}

#[test]
fn a_missing_record_is_reported_as_absence_not_corruption() {
    let root = scratch("absent");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");

    let error = read_device_record(&layout.device_meta_path(DeviceId::new(1))).expect_err("absent");
    assert_eq!(error.kind(), ErrorKind::NotFound);

    cleanup(&root);
}

#[test]
fn metadata_lives_only_under_the_documented_names() {
    let root = scratch("names");
    let layout = DatabaseLayout::new(&root);
    layout.initialize().expect("initialize");
    let table_id = TableId::new(1);
    layout.ensure_table(table_id).expect("table");
    layout
        .ensure_index(table_id, IndexId::new(1))
        .expect("index");
    layout
        .ensure_generation_dir_flat(table_id, GenerationId::new(1))
        .expect("generation");

    // Every logical metadata record is named `META.dat`; the device record has
    // its own name. No `.db` file exists anywhere in a fresh database.
    assert!(layout.table_meta_path(table_id).ends_with(META_FILE_NAME));
    assert!(layout
        .index_meta_path(table_id, IndexId::new(1))
        .ends_with(META_FILE_NAME));
    for entry in walk(&root) {
        assert!(
            !entry.to_string_lossy().ends_with(".db"),
            "no database file may use the legacy .db suffix: {}",
            entry.display()
        );
    }

    cleanup(&root);
}

/// Collects every file path below `dir`, deterministically ordered.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}
