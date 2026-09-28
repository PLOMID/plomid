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
//! PLOMID storage benchmarks: random page reads, sequential block reads,
//! sequential pack writes, and batched page reads.
//!
//! Benchmark-quality rules: every case warms up, black-boxes results so the
//! optimizer cannot eliminate the work, and reports ops/s, bytes/s, and
//! latency/op. Setup (file creation, seeding, fsync) happens before timing.
//!
//! Run with: `cargo bench -p plomid-storage --bench storage_io`.

use plomid_core::{BlockId, PageId, Result};
use plomid_storage::physical::PageType;
use plomid_storage::{BlockManager, PackManager, Page, PageManager, PAGE_SIZE};
use std::path::PathBuf;
use std::time::Instant;

const PAGE_COUNT: usize = 4096;
const BLOCK_COUNT: usize = 256;
const PAGE_BYTES: u64 = PAGE_SIZE as u64;
const BLOCK_BYTES: u64 = 256 * 1024;

fn scratch(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "{name}-b-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    (dir.clone(), dir.join("pages.plpm"), dir.join("blocks.plbm"))
}

fn gib_per_sec(bytes: u64, secs: f64) -> f64 {
    bytes as f64 / secs / (1024.0 * 1024.0 * 1024.0)
}

/// Deterministic xorshift: the old harness stepped `(acc + id) % N`, which is
/// a strided walk, not a random one. This keeps the access pattern uniform
/// without allocating or calling into an RNG on the hot path.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self, bound: usize) -> usize {
        let mut x = self.0 | 1;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x as usize) % bound
    }
}

fn report(name: &str, iters: u64, bytes: u64, secs: f64) {
    let per_sec = if secs > 0.0 { iters as f64 / secs } else { 0.0 };
    let gib = gib_per_sec(bytes, secs);
    let mib = bytes as f64 / secs / (1024.0 * 1024.0);
    let ns = if iters > 0 {
        secs / iters as f64 * 1e9
    } else {
        0.0
    };
    println!(
        "  {name:.<32} {iters:>8} ops in {secs:8.3}s = {per_sec:10.0} ops/s ({mib:8.1} MiB/s, {gib:5.2} GiB/s, {ns:9.0} ns/op)"
    );
}

fn seed_page_files() -> (PathBuf, PathBuf) {
    let (_, pages, blocks) = scratch("storage_io");
    let mut pm = PageManager::create(&pages).expect("pm create");
    for i in 0..PAGE_COUNT {
        let mut page = pm.allocate_page(PageType::Leaf).expect("alloc");
        page.data_mut()[..8].copy_from_slice(&(i as u64).to_le_bytes());
        for (j, b) in page.data_mut()[8..64].iter_mut().enumerate() {
            *b = (i.wrapping_add(j * 31) & 0xff) as u8;
        }
        pm.write_page(&page).expect("write");
    }
    pm.flush().expect("pm flush");

    let mut bm = BlockManager::create(&blocks).expect("bm create");
    for b in 0..BLOCK_COUNT {
        let id = bm.allocate_block().expect("bm alloc");
        let mut pages_in = Vec::with_capacity(16);
        for p in 0..16 {
            let mut page = Page::with_type(PageId::new((b * 16 + p + 1) as u64), PageType::Leaf);
            page.data_mut()[..8].copy_from_slice(&((b * 16 + p) as u64).to_le_bytes());
            pages_in.push(page);
        }
        bm.write_block(id, &pages_in).expect("bm write");
    }
    bm.flush().expect("bm flush");
    drop(bm);
    drop(pm);
    (pages, blocks)
}

fn main() -> Result<()> {
    let (pages_path, blocks_path) = seed_page_files();

    println!("PLOMID storage benchmarks");
    println!(
        "pages={PAGE_COUNT} ({} MiB) blocks={BLOCK_COUNT} page={PAGE_BYTES}B block={BLOCK_BYTES}B",
        PAGE_COUNT as u64 * PAGE_BYTES / (1024 * 1024)
    );

    // 1. Random page reads via PageManager::read_page (uniform xorshift).
    {
        let mut pm = PageManager::open(&pages_path).expect("pm open");
        let ids: Vec<PageId> = (0..PAGE_COUNT)
            .map(|i| PageId::new((i as u64) + 1))
            .collect();
        for id in ids.iter().take(64) {
            let page = pm.read_page(*id).expect("warmup");
            std::hint::black_box(page.id());
        }
        let mut rng = XorShift(0x9E3779B97F4A7C15);
        let mut acc = 0_u64;
        let iters = 20_000_u64;
        let start = Instant::now();
        for _ in 0..iters {
            let id = ids[rng.next(PAGE_COUNT)];
            let page = pm.read_page(id).expect("read");
            let tag = u64::from_le_bytes(page.data()[..8].try_into().expect("tag"));
            acc = acc.wrapping_add(tag ^ page.id().get());
        }
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(acc);
        report("random_page_read", iters, iters * PAGE_BYTES, secs);
    }

    // 2. Sequential block reads via BlockManager::read_blocks.
    {
        let mut bm = BlockManager::open(&blocks_path).expect("bm open");
        let ids: Vec<BlockId> = (0..BLOCK_COUNT)
            .map(|i| BlockId::new((i as u64) + 1))
            .collect();
        let warm = bm.read_blocks(&ids).expect("warmup");
        std::hint::black_box(warm.len());
        let iters = 20_u64;
        let mut acc = 0_u64;
        let start = Instant::now();
        for _ in 0..iters {
            let blocks = bm.read_blocks(&ids).expect("read blocks");
            for (i, pages) in blocks.iter().enumerate() {
                acc = acc.wrapping_add(pages.len() as u64 + i as u64);
                std::hint::black_box(pages[0].id());
            }
        }
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(acc);
        report(
            "sequential_block_read",
            iters * BLOCK_COUNT as u64,
            iters * BLOCK_COUNT as u64 * BLOCK_BYTES,
            secs,
        );
    }

    // 3. Sequential pack writes via PackManager::write_blocks.
    {
        let (dir, path) = {
            let d = std::env::temp_dir().join(format!(
                "storage_io-pack-write-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("time")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).expect("dir");
            (d.clone(), d.join("pack.plpk"))
        };
        let mut pk = PackManager::create(&path, 64).expect("pk create");
        let mut block_ids = Vec::new();
        for _ in 0..16 {
            block_ids.push(pk.allocate_block().expect("alloc"));
        }
        let mut payload: Vec<(BlockId, Vec<Page>)> = Vec::new();
        for (b, id) in block_ids.iter().enumerate() {
            let mut pages_in = Vec::with_capacity(16);
            for p in 0..16 {
                let mut page =
                    Page::with_type(PageId::new((b * 16 + p + 1) as u64), PageType::Leaf);
                page.data_mut()[..8].copy_from_slice(&((b * 16 + p) as u64).to_le_bytes());
                // Pack writes pre-validate each page; mutating the payload via
                // `data_mut` stales the trailer, so refresh it here (the same
                // refresh `to_bytes` performs inside `write_blocks`).
                page.refresh_for_write();
                pages_in.push(page);
            }
            payload.push((*id, pages_in));
        }
        pk.write_blocks(&payload).expect("warmup");
        let start = Instant::now();
        let iters = 20_u64;
        for _ in 0..iters {
            pk.write_blocks(&payload).expect("write blocks");
        }
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(&payload);
        report(
            "sequential_pack_write",
            iters * 16,
            iters * 16 * BLOCK_BYTES,
            secs,
        );
        drop(pk);
        std::fs::remove_dir_all(&dir).ok();
    }

    // 4. Batched page reads via PageManager::read_pages (single merged run).
    {
        let mut pm = PageManager::open(&pages_path).expect("pm open");
        let ids: Vec<PageId> = (0..PAGE_COUNT)
            .map(|i| PageId::new((i as u64) + 1))
            .collect();
        let warm = pm.read_pages(&ids[..64]).expect("warmup");
        std::hint::black_box(warm.len());
        let mut acc = 0_u64;
        let iters = 10_u64;
        let start = Instant::now();
        for _ in 0..iters {
            let pages = pm.read_pages(&ids).expect("read pages");
            for page in &pages {
                acc = acc.wrapping_add(page.id().get());
            }
            std::hint::black_box(pages.len());
        }
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(acc);
        report(
            "batched_page_read",
            iters * PAGE_COUNT as u64,
            iters * PAGE_COUNT as u64 * PAGE_BYTES,
            secs,
        );
    }

    std::fs::remove_dir_all(&pages_path.parent().expect("parent")).ok();
    std::fs::remove_dir_all(&blocks_path.parent().expect("parent")).ok();
    Ok(())
}
