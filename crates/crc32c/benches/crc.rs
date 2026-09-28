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
//! CRC32C benchmark: reference (bitwise) vs software slicing-by-8 vs
//! hardware, across WAL-record and database-page sizes plus the PLOMID
//! 16 KiB page size. Run with `cargo bench -p crc32c`.

use crc32c::{crc_init, reference, software};
use std::time::Instant;

/// Sizes covering small WAL records through large pages/blocks.
const SIZES: [usize; 9] = [
    64,
    256,
    1024,
    4096,
    8192,
    16384, // PLOMID on-disk page size
    65536,
    262_144, // PLOMID block size
    1024 * 1024,
];

/// Target bytes processed per measurement; keeps every row's noise low while
/// bounding total runtime.
const TARGET_BYTES: usize = 64 * 1024 * 1024;

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut data = Vec::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        data.push((state >> 33) as u8);
    }
    data
}

fn throughput_mibs(bytes: usize, secs: f64) -> f64 {
    bytes as f64 / secs / (1024.0 * 1024.0)
}

fn ns_per_op(iters: usize, secs: f64) -> f64 {
    secs / iters as f64 * 1e9
}

fn main() {
    println!("CRC32C benchmark (crc32c crate)");
    let hw_available = crc32c::dispatch_hw().is_some();
    if hw_available {
        println!("hardware: available (dispatch uses CPU CRC instructions)");
    } else {
        println!("hardware: unavailable on this CPU; hardware column omitted");
    }
    println!(
        "{:<11} {:>18} {:>18} {:>18} {:>9}",
        "input", "reference", "software", "hardware", "speedup"
    );
    println!(
        "{:<11} {:>18} {:>18} {:>18} {:>9}",
        "", "MiB/s (ns/op)", "MiB/s (ns/op)", "MiB/s (ns/op)", "ref/sw"
    );

    for &size in &SIZES {
        let data = deterministic_bytes(0xC0FFEE, size);
        let iters = (TARGET_BYTES / size).max(8);
        let label = if size == 16_384 {
            format!("{size} B *")
        } else {
            format!("{size} B")
        };

        let mut acc = 0_u64;
        let start = Instant::now();
        for _ in 0..iters {
            let checksum = std::hint::black_box(reference::compute(std::hint::black_box(&data)));
            acc = acc.wrapping_add(checksum as u64);
        }
        std::hint::black_box(acc);
        let ref_secs = start.elapsed().as_secs_f64();

        let mut acc2 = 0_u64;
        let start = Instant::now();
        for _ in 0..iters {
            let checksum = std::hint::black_box(software::compute(std::hint::black_box(&data)));
            acc2 = acc2.wrapping_add(checksum as u64);
        }
        std::hint::black_box(acc2);
        let sw_secs = start.elapsed().as_secs_f64();

        let mut hw_secs = f64::NAN;
        if let Some(hw) = crc32c::dispatch_hw() {
            let mut acc3 = 0_u64;
            let start = Instant::now();
            for _ in 0..iters {
                let checksum = std::hint::black_box(hw(crc_init(), std::hint::black_box(&data)));
                acc3 = acc3.wrapping_add(checksum as u64);
            }
            std::hint::black_box(acc3);
            hw_secs = start.elapsed().as_secs_f64();
        }

        // Same payload, three implementations: results must agree.
        assert_eq!(acc, acc2, "reference/software mismatch at {size} B");

        let ref_tp = throughput_mibs(size * iters, ref_secs);
        let sw_tp = throughput_mibs(size * iters, sw_secs);
        let speedup = if sw_secs > 0.0 {
            ref_secs / sw_secs
        } else {
            0.0
        };
        let ref_col = format!("{ref_tp:>7.1} ({:>5}ns)", ns_per_op(iters, ref_secs) as u64);
        let sw_col = format!("{sw_tp:>7.1} ({:>5}ns)", ns_per_op(iters, sw_secs) as u64);
        let hw_col = if hw_secs.is_nan() {
            "n/a".to_string()
        } else {
            format!(
                "{:>7.1} ({:>5}ns)",
                throughput_mibs(size * iters, hw_secs),
                ns_per_op(iters, hw_secs) as u64
            )
        };
        println!("{label:<11} {ref_col:>18} {sw_col:>18} {hw_col:>18} {speedup:>8.2}x");
    }
    println!("* = PLOMID on-disk page size (16 KiB)");
}
