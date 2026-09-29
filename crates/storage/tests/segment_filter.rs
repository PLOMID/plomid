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
//! Sealed-segment negative-filter tests: new-key writes must skip sealed
//! segments they cannot match, without changing exact lookup semantics.
use plomid_storage::StorageManager;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "plomid-segfilter-{label}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch");
    path
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Counts persisted `.xor` sidecars under any packs directory.
fn sidecars(root: &Path) -> usize {
    let mut n = 0;
    fn walk(dir: &Path, n: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, n);
            } else if p.extension().is_some_and(|ext| ext == "xor") {
                *n += 1;
            }
        }
    }
    walk(root, &mut n);
    n
}

fn key(i: usize) -> Vec<u8> {
    format!("key-{i:06}").into_bytes()
}

#[test]
fn new_keys_skip_sealed_probes_without_losing_writes() {
    let root = scratch("skips");
    let result = (|| {
        // 1-byte segments rotate on (almost) every insert.
        let mut manager = StorageManager::create(&root, 32, 1)?;
        for i in 0..100 {
            manager.insert(&key(i), format!("v{i}").as_bytes())?;
        }
        assert!(manager.segment_count() > 10, "needs many sealed segments");
        let (probes0, skips0) = manager.filter_stats();
        let _ = (probes0, skips0);
        // 100 brand-new keys: none can match a sealed segment.
        let batch: Vec<(Vec<u8>, Option<Vec<u8>>)> = (100..200)
            .map(|i| (key(i), Some(format!("v{i}").into_bytes())))
            .collect();
        manager.apply_batch(&batch)?;
        let (probes, skips) = manager.filter_stats();
        let new_skips = skips - skips0;
        let new_probes = probes - probes0;
        // Sealed segments hold ~100 keys each... every new key must skip the
        // vast majority of sealed probes (XOR false-positive rate « 1%).
        assert!(
            new_skips > new_probes * 10,
            "filter must skip most sealed probes: probes={new_probes} skips={new_skips}"
        );
        // All 200 keys read back exactly.
        for i in 0..200 {
            assert_eq!(
                manager.get(&key(i))?,
                Some(format!("v{i}").into_bytes()),
                "key {i}"
            );
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn sealed_replace_and_delete_stay_exact() {
    let root = scratch("exact");
    let result = (|| {
        let mut manager = StorageManager::create(&root, 32, 1)?;
        for i in 0..50 {
            manager.insert(&key(i), b"v1")?;
        }
        // Replace a sealed key: must update in place, single visible copy.
        manager.apply_batch(&[(key(7), Some(b"v2".to_vec()))])?;
        assert_eq!(manager.get(&key(7))?, Some(b"v2".to_vec()));
        // Delete a sealed key: the (now stale) filter still lists it, but the
        // exact path decides and the key must read absent.
        manager.apply_batch(&[(key(7), None)])?;
        assert_eq!(manager.get(&key(7))?, None);
        // Neighbors untouched.
        assert_eq!(manager.get(&key(6))?, Some(b"v1".to_vec()));
        assert_eq!(manager.get(&key(8))?, Some(b"v1".to_vec()));
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn absent_keys_return_none_across_sealed_segments() {
    let root = scratch("absent");
    let result = (|| {
        let mut manager = StorageManager::create(&root, 32, 1)?;
        for i in 0..200 {
            manager.insert(&key(i), b"v")?;
        }
        let (_, skips0) = manager.filter_stats();
        for i in 1000..1300 {
            assert_eq!(manager.get(&key(i))?, None);
        }
        let (_, skips1) = manager.filter_stats();
        // 300 absent keys × ~200 sealed segments ≈ 60k skippable probes.
        assert!(
            skips1 - skips0 > 30_000,
            "absent reads must skip sealed descents: {}",
            skips1 - skips0
        );
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn filters_persist_across_restart() {
    let root = scratch("persist");
    let result = (|| {
        {
            let mut manager = StorageManager::create(&root, 32, 1)?;
            for i in 0..100 {
                manager.insert(&key(i), b"v")?;
            }
            manager.sync()?;
        }
        assert!(sidecars(&root) > 5, "seal must persist sidecars");
        {
            let mut manager = StorageManager::open(&root, 32, 1)?;
            for i in 0..100 {
                assert_eq!(manager.get(&key(i))?, Some(b"v".to_vec()));
            }
            let (_, skips0) = manager.filter_stats();
            // New keys immediately skip without any rebuild scan being
            // required for correctness (sidecars loaded at open).
            let batch: Vec<(Vec<u8>, Option<Vec<u8>>)> =
                (500..600).map(|i| (key(i), Some(b"n".to_vec()))).collect();
            manager.apply_batch(&batch)?;
            let (_, skips1) = manager.filter_stats();
            assert!(skips1 - skips0 > 1_000, "reopened filters must skip");
            for i in 500..600 {
                assert_eq!(manager.get(&key(i))?, Some(b"n".to_vec()));
            }
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn corrupt_sidecar_falls_back_safely() {
    let root = scratch("corrupt");
    let result = (|| {
        {
            let mut manager = StorageManager::create(&root, 32, 1)?;
            for i in 0..50 {
                manager.insert(&key(i), b"v")?;
            }
            manager.sync()?;
        }
        // Corrupt every sidecar image.
        fn walk(dir: &Path) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p);
                } else if p.extension().is_some_and(|ext| ext == "xor") {
                    let _ = std::fs::write(&p, b"garbage-bytes");
                }
            }
        }
        walk(&root);
        {
            let mut manager = StorageManager::open(&root, 32, 1)?;
            // All reads exact despite corrupt filters (lazy rebuild path).
            for i in 0..50 {
                assert_eq!(manager.get(&key(i))?, Some(b"v".to_vec()));
            }
            // Writes still land correctly.
            manager.apply_batch(&[(key(9999), Some(b"n".to_vec()))])?;
            assert_eq!(manager.get(&key(9999))?, Some(b"n".to_vec()));
        }
        Ok::<(), plomid_core::PlomidError>(())
    })();
    cleanup(&root);
    assert!(result.is_ok(), "{result:?}");
}
