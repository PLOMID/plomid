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
//! CRC32C correctness suite.
//!
//! Every optimized path is validated against the bitwise `reference`
//! implementation, never against itself. Randomized inputs use a fixed-seed
//! LCG so failures are reproducible. Tests never assume the running CPU has
//! CRC instructions; hardware paths are compared only when available.

use crate::{compute, crc_finalize, crc_init, crc_update, hardware_accelerated, verify, Mismatch};
use crate::{reference, software};

/// Deterministic pseudo-random byte generator (no external dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }
}

/// Every boundary size from the contract plus the PLOMID page size.
const BOUNDARY_SIZES: [usize; 22] = [
    0, 1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256, 4095, 4096, 8191, 8192, 16383,
    16384,
];

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Lcg(seed);
    (0..len).map(|_| rng.next() as u8).collect()
}

#[test]
fn canonical_known_answer() {
    assert_eq!(compute(b"123456789"), 0xE306_9283);
    assert_eq!(software::compute(b"123456789"), 0xE306_9283);
    assert_eq!(reference::compute(b"123456789"), 0xE306_9283);
}

#[test]
fn empty_input() {
    // CRC32C of the empty string.
    assert_eq!(compute(&[]), 0x0000_0000);
    assert_eq!(software::compute(&[]), 0x0000_0000);
    assert_eq!(reference::compute(&[]), 0x0000_0000);
}

#[test]
fn boundary_sizes_match_reference() {
    for (index, &len) in BOUNDARY_SIZES.iter().enumerate() {
        let data = deterministic_bytes(0x5EED_0001 + index as u64, len);
        let expected = reference::compute(&data);
        assert_eq!(software::compute(&data), expected, "software len={len}");
        assert_eq!(compute(&data), expected, "dispatched len={len}");
        if hardware_accelerated() {
            let hw = crate::dispatch_hw().expect("hardware available");
            assert_eq!(hw(crc_init(), &data), reference::update(crc_init(), &data));
        }
    }
}

#[test]
fn streaming_matches_one_shot_at_arbitrary_boundaries() {
    let data = deterministic_bytes(0xBEEF, 4096);
    let one_shot = compute(&data);
    // Chunking plans: 1 + N-1, 7 + 13 + remainder, byte-at-a-time, header +
    // payload style splits, and uneven fragments.
    let plans: [Vec<usize>; 4] = [
        vec![1, data.len() - 1],
        vec![7, 13, data.len() - 20],
        vec![1; data.len()],
        vec![48, 160, 1024, 3, data.len() - 1235],
    ];
    for (index, plan) in plans.iter().enumerate() {
        let mut crc = crc_init();
        let mut consumed = 0_usize;
        for &chunk_len in plan.iter() {
            let end = (consumed + chunk_len).min(data.len());
            crc = crc_update(crc, &data[consumed..end]);
            consumed = end;
        }
        if consumed < data.len() {
            crc = crc_update(crc, &data[consumed..]);
        }
        assert_eq!(crc_finalize(crc), one_shot, "plan {index}");
    }
}

#[test]
fn randomized_equivalence() {
    let mut rng = Lcg(0xCAFE_BABE);
    for case in 0..64_u32 {
        let len = (rng.next() % 70_000) as usize;
        let data = deterministic_bytes(rng.next(), len);
        let expected = reference::compute(&data);
        assert_eq!(software::compute(&data), expected, "software case {case}");
        assert_eq!(compute(&data), expected, "dispatched case {case}");
        if hardware_accelerated() {
            let hw = crate::dispatch_hw().expect("hardware available");
            assert_eq!(hw(crc_init(), &data), reference::update(crc_init(), &data));
        }
    }
}

#[test]
fn streaming_state_is_portable_between_implementations() {
    let data = deterministic_bytes(0x7777, 5000);
    let half = data.len() / 2;
    let mut crc = crc_init();
    crc = software::update(crc, &data[..half]);
    if hardware_accelerated() {
        let hw = crate::dispatch_hw().expect("hardware available");
        crc = hw(crc, &data[half..]);
        // Mixed-implementation streaming must equal the pure reference run.
        assert_eq!(crc_finalize(crc), reference::compute(&data));
    }
}

#[test]
fn bit_corruption_is_detected() {
    let data = deterministic_bytes(0xD00D, 3000);
    let expected = compute(&data);

    // Corruptions: first byte, middle byte, final byte, first bit, last bit,
    // and multiple bits at once.
    let mut corruptions = Vec::new();
    let mut first = data.clone();
    first[0] ^= 0xFF;
    corruptions.push(first);
    let mut middle = data.clone();
    middle[data.len() / 2] ^= 0x10;
    corruptions.push(middle);
    let mut last = data.clone();
    last[data.len() - 1] ^= 0x01;
    corruptions.push(last);
    let mut first_bit = data.clone();
    first_bit[0] ^= 0x01;
    corruptions.push(first_bit);
    let mut last_bit = data.clone();
    last_bit[data.len() - 1] ^= 0x80;
    corruptions.push(last_bit);
    let mut multi = data.clone();
    multi[7] ^= 0x0F;
    multi[1500] ^= 0xF0;
    multi[data.len() - 2] ^= 0x03;
    corruptions.push(multi);

    for corruption in corruptions {
        let mismatch = verify(&corruption, expected).expect_err("corruption must be detected");
        assert_ne!(mismatch.expected, mismatch.actual);
        let actual = compute(&corruption);
        assert_eq!(mismatch, Mismatch { expected, actual });
    }

    // Unmodified data verifies successfully.
    assert!(verify(&data, expected).is_ok());
}

#[test]
fn fallback_works_without_hardware() {
    // Whenever hardware is unavailable, the dispatched implementation is the
    // software one and still matches the reference.
    let data = deterministic_bytes(0x4242, 10_000);
    assert_eq!(compute(&data), reference::compute(&data));
    if !hardware_accelerated() {
        assert_eq!(
            software::update(crc_init(), &data),
            reference::update(crc_init(), &data)
        );
    }
}
