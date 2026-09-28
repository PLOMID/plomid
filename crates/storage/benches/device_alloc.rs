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
//! PLOMID device benchmarks: extent allocation, release, reuse, and
//! fragmented allocation with coalescing.
//!
//! Run with: `cargo bench -p plomid-storage --bench device_alloc`.

use plomid_core::{DeviceId, Result};
use plomid_storage::device::{capacity_for_extents, StorageDevice};
use plomid_storage::physical::EXTENT_SIZE;
use std::path::PathBuf;
use std::time::Instant;

fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "{name}-b-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    (dir.clone(), dir.join("device.dat"))
}

fn report(name: &str, iters: u64, secs: f64) {
    let per_op = if secs > 0.0 { iters as f64 / secs } else { 0.0 };
    println!("  {name:.<40} {iters:>8} ops in {secs:8.3}s = {per_op:10.0} ops/s");
}

fn main() -> Result<()> {
    println!("PLOMID device benchmarks");
    // Small device: metadata is pre-sized, so even a 64-extent file stays
    // a sparse container without multi-gigabyte I/O.
    let capacity = capacity_for_extents(64).expect("capacity");

    {
        let (dir, path) = scratch("device-alloc-64");
        let device = StorageDevice::create(&path, DeviceId::new(1), capacity).expect("create");
        let start = Instant::now();
        let iters = 32_u64;
        for _ in 0..iters {
            let extent = device.allocate(EXTENT_SIZE).expect("alloc");
            device.release(extent.start).expect("release");
        }
        report("extent_alloc_64mib", iters, start.elapsed().as_secs_f64());
        drop(device);
        std::fs::remove_dir_all(&dir).ok();
    }

    {
        let (dir, path) = scratch("device-alloc-256");
        let device = StorageDevice::create(&path, DeviceId::new(2), capacity).expect("create");
        let start = Instant::now();
        let iters = 16_u64;
        for _ in 0..iters {
            let extent = device.allocate(4 * EXTENT_SIZE).expect("alloc");
            device.release(extent.start).expect("release");
        }
        report("extent_alloc_256mib", iters, start.elapsed().as_secs_f64());
        drop(device);
        std::fs::remove_dir_all(&dir).ok();
    }

    {
        let (dir, path) = scratch("device-alloc-512");
        let device = StorageDevice::create(&path, DeviceId::new(3), capacity).expect("create");
        let start = Instant::now();
        let iters = 8_u64;
        for _ in 0..iters {
            let extent = device.allocate(8 * EXTENT_SIZE).expect("alloc");
            device.release(extent.start).expect("release");
        }
        report("extent_alloc_512mib", iters, start.elapsed().as_secs_f64());
        drop(device);
        std::fs::remove_dir_all(&dir).ok();
    }

    {
        let (dir, path) = scratch("device-alloc-1g");
        let device = StorageDevice::create(&path, DeviceId::new(4), capacity).expect("create");
        let start = Instant::now();
        let iters = 4_u64;
        for _ in 0..iters {
            let extent = device.allocate(16 * EXTENT_SIZE).expect("alloc");
            device.release(extent.start).expect("release");
        }
        report("extent_alloc_1gib", iters, start.elapsed().as_secs_f64());
        drop(device);
        std::fs::remove_dir_all(&dir).ok();
    }

    {
        let (dir, path) = scratch("device-reuse");
        let device = StorageDevice::create(&path, DeviceId::new(5), capacity).expect("create");
        let seed = device.allocate(EXTENT_SIZE).expect("seed");
        device.release(seed.start).expect("free");
        let start = Instant::now();
        let iters = 64_u64;
        for _ in 0..iters {
            let extent = device.allocate(EXTENT_SIZE).expect("alloc");
            device.release(extent.start).expect("release");
        }
        report("extent_release_reuse", iters, start.elapsed().as_secs_f64());
        drop(device);
        std::fs::remove_dir_all(&dir).ok();
    }

    {
        let (dir, path) = scratch("device-frag");
        let device = StorageDevice::create(&path, DeviceId::new(6), capacity).expect("create");
        let mut held = Vec::new();
        for _ in 0..16 {
            held.push(device.allocate(EXTENT_SIZE).expect("fill").start);
        }
        // Free alternating extents to fragment the map.
        for chunk in held.chunks(2) {
            device.release(chunk[0]).expect("frag");
        }
        let start = Instant::now();
        let iters = 16_u64;
        for _ in 0..iters {
            let extent = device.allocate(EXTENT_SIZE).expect("frag alloc");
            device.release(extent.start).expect("frag release");
        }
        report("fragmented_alloc", iters, start.elapsed().as_secs_f64());
        // Release the survivors and coalesce back into one range.
        for start in held.into_iter().skip(1).step_by(2) {
            device.release(start).expect("coalesce");
        }
        let start = Instant::now();
        let extent = device.allocate(64 * EXTENT_SIZE).expect("coalesced");
        report("coalesced_alloc_4gib", 1, start.elapsed().as_secs_f64());
        device.release(extent.start).expect("free");
        drop(device);
        std::fs::remove_dir_all(&dir).ok();
    }

    Ok(())
}
