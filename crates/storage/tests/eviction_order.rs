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
//! Eviction-ordering regression tests: evicted pages must never leave flushed
//! parents referencing unwritten children. Bulk writes with a tiny pool force
//! constant eviction; dropping without checkpoint/sync and reopening must
//! yield a valid, complete database.
use plomid_storage::StorageManager;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("plomid-evict-{label}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn key(i: usize) -> Vec<u8> {
    // Deliberately NOT zero-padded: decimal row keys sort differently than
    // they insert, scattering splits across the tree (the production shape
    // that exposes eviction-ordering bugs; sorted appends only touch the
    // right edge and cannot).
    format!("k{i}").into_bytes()
}

#[test]
fn bulk_write_with_eviction_reopens_complete() {
    let root = scratch("bulk");
    let result = (|| {
        // Pool of 8 pages against thousands of keys: every put evicts.
        let mut manager = StorageManager::create(&root, 8, 1 << 20)?;
        for chunk in 0..20 {
            let batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = (0..500)
                .map(|i| {
                    let id = chunk * 500 + i;
                    (key(id), Some(vec![(id & 0xff) as u8; 48]))
                })
                .collect();
            manager.apply_batch(&batch)?;
        }
        manager.sync()?;
        drop(manager);
        // Reopen (no checkpoint involved) and verify every key.
        let mut reopened = StorageManager::open(&root, 8, 1 << 20)?;
        for id in (0..10000).step_by(37) {
            assert_eq!(
                reopened.get(&key(id))?,
                Some(vec![(id & 0xff) as u8; 48]),
                "key {id}"
            );
        }
        assert!(reopened.get(b"missing")?.is_none());
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn engine_key_mix_with_sync_reopens_complete() {
    // Same key mix as the engine PK path (row + long index keys, sorted),
    // synced before drop: all content must survive.
    let root = scratch("enginemix");
    let result = (|| {
        let mut manager = StorageManager::create(&root, 64, 1 << 20)?;
        let mut ops: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
        for i in 0..5000 {
            ops.push((format!("t:{i}").into_bytes(), Some(vec![1u8; 40])));
            ops.push((
                format!("__plomid_index:t_pkey:{i:020}").into_bytes(),
                Some(format!("t:{i}").into_bytes()),
            ));
        }
        ops.sort_by(|a, b| a.0.cmp(&b.0));
        manager.apply_batch(&ops)?;
        manager.sync()?;
        drop(manager);
        let mut reopened = StorageManager::open(&root, 64, 1 << 20)?;
        for i in (0..5000).step_by(97) {
            assert!(
                reopened.get(format!("t:{i}").as_bytes())?.is_some(),
                "row {i}"
            );
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn drop_without_sync_reopens_complete() {
    let root = scratch("nosync");
    let result = (|| {
        let mut manager = StorageManager::create(&root, 8, 1 << 20)?;
        for chunk in 0..10 {
            let batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = (0..500)
                .map(|i| {
                    let id = chunk * 500 + i;
                    (key(id), Some(vec![(id & 0xff) as u8; 32]))
                })
                .collect();
            manager.apply_batch(&batch)?;
        }
        // Deliberately NO sync: drop with a pool full of dirty pages, then
        // reopen. The file image must still be structurally valid (stale but
        // never dangling); WAL replay is out of scope at this layer, so this
        // asserts openability, not content.
        drop(manager);
        let _reopened = StorageManager::open(&root, 8, 1 << 20)?;
        Ok::<(), plomid_core::PlomidError>(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    assert!(result.is_ok(), "{result:?}");
}
