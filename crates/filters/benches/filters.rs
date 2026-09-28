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
//! PLOMID filter benchmarks: Roaring bitmap construction, membership, set
//! algebra, and persistence, plus XOR filter build and probes.
//!
//! Run with: `cargo bench -p plomid-filters --bench filters`.
//!
//! The harness is deliberately dependency-free (no criterion): the report needs
//! operations/sec *and* p50/p95/p99 latencies, which are measured directly with
//! `Instant` over per-operation samples. This matches the plain-harness style
//! used by `crates/index/benches/art.rs` and
//! `crates/columnar/benches/flush_pipeline.rs`.
//!
//! Datasets come from a deterministic splitmix64 PRNG, so a run is reproducible
//! from its seed alone. Every number printed was produced by running this
//! benchmark, not estimated.

use plomid_filters::{RoaringBitmap, XorFilter};
use std::time::Instant;

/// Deterministic splitmix64 PRNG.
///
/// Written out here on purpose: a filter benchmark should not force a
/// random-number dependency into the crate graph.
struct Rng(u64);

impl Rng {
    /// Creates a generator with the given seed.
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Returns the next pseudo-random `u64`.
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Returns a pseudo-random `u32` over the whole 32-bit space.
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Returns a pseudo-random index in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Latency of one operation in nanoseconds.
type Nanos = u64;

/// Returns the requested percentile from an unsorted latency sample.
///
/// Nearest-rank method: the first sample at or above the requested share of the
/// sorted sample.
fn percentile(samples: &mut [Nanos], pct: f64) -> Nanos {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let rank = ((samples.len() as f64) * pct).ceil() as usize;
    let index = rank.saturating_sub(1).min(samples.len() - 1);
    samples[index]
}

/// Prints one measured operation.
fn report(label: &str, ops: usize, elapsed_s: f64, samples: &mut [Nanos]) {
    let per_sec = if elapsed_s > 0.0 {
        ops as f64 / elapsed_s
    } else {
        0.0
    };
    let mean_us = if samples.is_empty() {
        0.0
    } else {
        samples.iter().sum::<Nanos>() as f64 / samples.len() as f64 / 1_000.0
    };
    println!(
        "  {label:<34} {ops:>8} ops {per_sec:>12.0} ops/s  mean {mean_us:>7.3} us  \
         p50 {:>7.3} us  p95 {:>7.3} us  p99 {:>7.3} us",
        percentile(samples, 0.50) as f64 / 1_000.0,
        percentile(samples, 0.95) as f64 / 1_000.0,
        percentile(samples, 0.99) as f64 / 1_000.0,
    );
}

/// The value distributions a column filter is expected to see.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Random positions across the 32-bit space: every bucket is sparse.
    Random,
    /// Ascending positions: the best case for run containers.
    Sequential,
    /// Clustered positions in long runs separated by gaps.
    Clustered,
    /// Dense positions inside a narrow window: pushes buckets to bitmaps.
    Dense,
}

impl Shape {
    /// Stable label used in the report.
    const fn name(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Sequential => "sequential",
            Self::Clustered => "clustered",
            Self::Dense => "dense",
        }
    }

    /// Builds `count` values for this shape.
    fn values(self, count: usize, seed: u64) -> Vec<u32> {
        let mut rng = Rng::new(seed);
        match self {
            Self::Random => (0..count).map(|_| rng.next_u32()).collect(),
            Self::Sequential => (0..count).map(|value| value as u32).collect(),
            Self::Clustered => {
                // Runs of 512 consecutive values, then a 4096-value gap.
                (0..count)
                    .map(|value| {
                        let run = (value / 512) as u32;
                        (run * 4608) + (value % 512) as u32
                    })
                    .collect()
            }
            Self::Dense => (0..count).map(|value| (value % 65_536) as u32).collect(),
        }
    }
}

/// Measures insertion of every value, one at a time.
fn bench_insert(shape: Shape, count: usize) {
    let values = shape.values(count, 0x5EED_1234);
    let mut bitmap = RoaringBitmap::new();
    let mut samples = Vec::with_capacity(count);
    let start = Instant::now();
    for value in &values {
        let op = Instant::now();
        bitmap.insert(*value);
        samples.push(op.elapsed().as_nanos() as Nanos);
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("insert", count, elapsed, &mut samples);
    println!(
        "  {:<34} cardinality {} buckets {}",
        "structure",
        bitmap.cardinality(),
        bitmap.container_count()
    );
}

/// Measures point membership: hits and misses.
fn bench_contains(shape: Shape, count: usize) {
    let values = shape.values(count, 0xA11CE);
    let mut bitmap = RoaringBitmap::new();
    for value in &values {
        bitmap.insert(*value);
    }
    let mut rng = Rng::new(0xBEEF);
    let probes = count.min(100_000);

    let mut hit_samples = Vec::with_capacity(probes);
    let start = Instant::now();
    for _ in 0..probes {
        let value = values[rng.below(values.len())];
        let op = Instant::now();
        let found = bitmap.contains(value);
        hit_samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(found, "an inserted value must be found");
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("contains (hit)", probes, elapsed, &mut hit_samples);

    // Misses probe values the shape never produced: high half of the space.
    let mut miss_samples = Vec::with_capacity(probes);
    let start = Instant::now();
    for _ in 0..probes {
        let value = values[rng.below(values.len())] ^ 0xFFFF_0000;
        let op = Instant::now();
        let found = bitmap.contains(value);
        miss_samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(!found, "a value outside the shape must be absent");
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("contains (miss)", probes, elapsed, &mut miss_samples);
}

/// Measures cardinality of growing ranges over one bitmap.
fn bench_range(shape: Shape, count: usize) {
    let values = shape.values(count, 0xF00D);
    let mut bitmap = RoaringBitmap::new();
    for value in &values {
        bitmap.insert(*value);
    }
    let mut rng = Rng::new(0xC0FFEE);
    let probes = 10_000usize;
    let span = values.last().copied().unwrap_or(0).max(1);
    let mut samples = Vec::with_capacity(probes);
    let start = Instant::now();
    for _ in 0..probes {
        let from = rng.next_u32() % span;
        let to = from.saturating_add(span / 8);
        let op = Instant::now();
        let in_range = bitmap.cardinality_in_range(from, to);
        samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(in_range <= bitmap.cardinality());
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("cardinality_in_range", probes, elapsed, &mut samples);
}

/// Measures union and intersection between two same-shaped bitmaps.
fn bench_set_ops(shape: Shape, count: usize, seed: u64) {
    let left_values = shape.values(count, seed);
    let right_values = shape.values(count, seed + 1);
    let mut left = RoaringBitmap::new();
    let mut right = RoaringBitmap::new();
    for value in &left_values {
        left.insert(*value);
    }
    for value in &right_values {
        right.insert(*value);
    }

    let rounds = 200usize;
    let mut union_samples = Vec::with_capacity(rounds);
    let start = Instant::now();
    for _ in 0..rounds {
        let op = Instant::now();
        let merged = left.union(&right);
        union_samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(merged.is_superset(&left) && merged.is_superset(&right));
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("union", rounds, elapsed, &mut union_samples);

    let mut intersect_samples = Vec::with_capacity(rounds);
    let start = Instant::now();
    for _ in 0..rounds {
        let op = Instant::now();
        let overlap = left.intersection(&right);
        intersect_samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(left.is_superset(&overlap) && right.is_superset(&overlap));
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("intersection", rounds, elapsed, &mut intersect_samples);
}

/// Measures serialization and deserialization of the whole bitmap.
fn bench_serialize(shape: Shape, count: usize) {
    let values = shape.values(count, 0xDEAD_BEEF);
    let mut bitmap = RoaringBitmap::new();
    for value in &values {
        bitmap.insert(*value);
    }
    let rounds = 200usize;

    let mut encode_samples = Vec::with_capacity(rounds);
    let start = Instant::now();
    for _ in 0..rounds {
        let op = Instant::now();
        let image = bitmap.serialize();
        encode_samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(!image.is_empty());
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("serialize", rounds, elapsed, &mut encode_samples);

    let image = bitmap.serialize();
    let mut decode_samples = Vec::with_capacity(rounds);
    let start = Instant::now();
    for _ in 0..rounds {
        let op = Instant::now();
        let decoded = RoaringBitmap::deserialize(&image).expect("own image must decode");
        decode_samples.push(op.elapsed().as_nanos() as Nanos);
        assert_eq!(decoded, bitmap);
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("deserialize", rounds, elapsed, &mut decode_samples);
    println!(
        "  {:<34} {} bytes for {} values ({:.1} bits/value)",
        "image",
        image.len(),
        bitmap.cardinality(),
        image.len() as f64 * 8.0 / f64::from(bitmap.cardinality().max(1))
    );
}

/// Measures XOR filter construction and membership probes.
fn bench_xor(count: usize) {
    let mut rng = Rng::new(0x1234_5678);
    let values: Vec<u32> = (0..count).map(|_| rng.next_u32()).collect();

    let start = Instant::now();
    // Build keys as 4-byte little-endian slices: XorFilter works over byte
    // strings, so we encode each `u32` value as `[u8; 4]` in LE order.
    //
    // From ~77k values up, a random u32 dataset is expected to contain repeats
    // (birthday bound), and repeats are what used to make construction fail
    // here; the filter collapses them before peeling.
    let keys: Vec<[u8; 4]> = values.iter().map(|v| v.to_le_bytes()).collect();
    let filter = XorFilter::build(keys.iter().map(|key| &key[..]))
        .unwrap_or_else(|error| panic!("construction failed for {count} keys: {error}"));
    let build = start.elapsed().as_secs_f64();
    // size_bytes() does not exist on XorFilter; use the serialized image length
    // instead. The image includes the 40-byte footer, which is negligible at
    // these sizes.
    let image_len = filter.serialize().len();
    println!(
        "  {:<34} {count:>8} values {build:>10.4} s  {} bits/value",
        "build",
        image_len as u64 * 8 / count.max(1) as u64
    );

    let probes = count.min(100_000);
    let mut hit_samples = Vec::with_capacity(probes);
    let start = Instant::now();
    for _ in 0..probes {
        let value = values[rng.below(values.len())];
        let key = value.to_le_bytes();
        let op = Instant::now();
        let found = filter.contains(&key);
        hit_samples.push(op.elapsed().as_nanos() as Nanos);
        assert!(found, "no false negatives are allowed");
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("xor contains (hit)", probes, elapsed, &mut hit_samples);

    // Misses come from a disjoint value space: the lowest 2^20 values that the
    // random probes do not cover are rare, so use the low range and count how
    // many probe as present (the measured false-positive rate).
    let mut miss_samples = Vec::with_capacity(probes);
    let mut positives = 0usize;
    let start = Instant::now();
    for index in 0..probes {
        let value = u32::try_from(index).expect("probe index fits a u32");
        let key = value.to_le_bytes();
        let op = Instant::now();
        let found = filter.contains(&key);
        miss_samples.push(op.elapsed().as_nanos() as Nanos);
        if found {
            positives += 1;
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    report("xor contains (miss)", probes, elapsed, &mut miss_samples);
    println!(
        "  {:<34} {positives} of {probes} absent probes reported present ({:.4}%)",
        "false-positive rate",
        positives as f64 * 100.0 / probes as f64
    );
}

/// Runs one shape and size through every bitmap measurement.
fn bench_shape(shape: Shape, count: usize) {
    println!();
    println!("{} — {count} values", shape.name());
    bench_insert(shape, count);
    bench_contains(shape, count);
    bench_range(shape, count);
    bench_set_ops(shape, count, 0x5EED);
    bench_serialize(shape, count);
}

fn main() {
    println!("PLOMID filter benchmarks");
    for shape in [
        Shape::Random,
        Shape::Sequential,
        Shape::Clustered,
        Shape::Dense,
    ] {
        for count in [1_000usize, 10_000, 100_000] {
            bench_shape(shape, count);
        }
    }

    println!();
    println!("XOR filter");
    for count in [1_000usize, 10_000, 100_000] {
        bench_xor(count);
    }

    println!();
    println!("Benchmarks complete.");
}
