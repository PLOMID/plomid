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
//! [`XorFilter`]: construction and membership.

use crate::constants::{
    XOR_ALPHA_DEN, XOR_ALPHA_NUM, XOR_FINGERPRINT_MODULUS, XOR_MAX_BUILD_ATTEMPTS,
    XOR_MAX_ELEMENTS, XOR_MAX_SLOTS, XOR_SLOTS_SLACK,
};
use crate::xor::error::XorError;
use crate::xor::hash::{self, KeyHashes};

/// Seed mixed into the first construction attempt; later attempts multiply an
/// increasing attempt counter into it, so a filter that fails to peel moves on
/// to a genuinely different arrangement rather than retrying the same one.
const SEED_BASE: u64 = 0x9E37_79B9_7F4A_7C15;

/// The slot ratio must clear the peelability threshold of a random 3-uniform
/// hypergraph, `m/n > 11/9`.
///
/// This is a compile-time check because the failure it guards against is not a
/// runtime one: below the threshold peeling cannot succeed for *any* seed, so
/// `XorFilter::build` would simply return [`XorError::BuildFailed`] forever and
/// every call site would be broken. A ratio that cannot work should not compile,
/// not fail a test someone may not run. Compared in integers to keep the
/// inequality exact.
const _: () = assert!(
    XOR_ALPHA_NUM * 9 > XOR_ALPHA_DEN * 11,
    "XOR_ALPHA_NUM/XOR_ALPHA_DEN is at or below 11/9, where a random 3-uniform \
     hypergraph never peels and XOR filter construction can never succeed"
);

/// A probabilistic membership filter over byte-string keys.
///
/// ```text
/// XorFilter { seed, element_count, fingerprints: [u8; slots] }
/// contains(key) = fingerprints[a] ^ fingerprints[b] ^ fingerprints[c] == fp(key)
/// ```
///
/// The empty filter has zero slots: it is a valid filter that answers `false`
/// for every key, which is what a caller expects for an empty key set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XorFilter {
    /// Seed the slot indices are derived from.
    seed: u64,
    /// Number of keys the filter was built from.
    element_count: u64,
    /// One fingerprint per slot; never zero.
    fingerprints: Vec<u8>,
}

/// Returns the slot count for `count` elements:
/// `ceil(count * XOR_ALPHA_NUM / XOR_ALPHA_DEN) + XOR_SLOTS_SLACK`, rounded up
/// to a multiple of three.
///
/// Two properties matter here and neither is a free tuning knob:
///
/// * The multiple of three keeps [`hash::derive`]'s three index thirds
///   disjoint, which is what guarantees the three slots of a key are distinct.
/// * The total must stay above the peelability threshold of a random 3-uniform
///   hypergraph (`m/n > 11/9`). Shrinking the ratio below that threshold does
///   not merely make construction less likely to succeed — it makes it
///   impossible, because the 2-core of the slot graph can never empty and every
///   retry seed fails identically. [`XOR_ALPHA_NUM`] and [`XOR_SLOTS_SLACK`]
///   carry the full rationale.
fn slot_count_for(count: usize) -> usize {
    let numerator = (count as u128) * u128::from(XOR_ALPHA_NUM);
    let scaled = numerator.div_ceil(u128::from(XOR_ALPHA_DEN));
    // At most XOR_MAX_SLOTS, so this conversion is exact for allowed inputs.
    let mut slots = (scaled as usize).saturating_add(XOR_SLOTS_SLACK);
    slots = slots.clamp(3, XOR_MAX_SLOTS);
    let remainder = slots % 3;
    if remainder != 0 {
        slots += 3 - remainder;
    }
    slots
}

impl XorFilter {
    /// Builds a filter over `keys`.
    ///
    /// # Errors
    ///
    /// Returns [`XorError::TooManyElements`] when `keys` exceeds the format's
    /// element budget, and [`XorError::BuildFailed`] when every attempt at
    /// peeling and verifying the key set fails. A failed build returns no
    /// filter: callers fall back to exact evaluation.
    pub fn build<'a, I>(keys: I) -> Result<Self, XorError>
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let keys: Vec<&[u8]> = keys.into_iter().collect();
        if keys.len() > XOR_MAX_ELEMENTS {
            return Err(XorError::TooManyElements {
                requested: keys.len(),
                limit: XOR_MAX_ELEMENTS,
            });
        }
        if keys.is_empty() {
            return Ok(Self::empty());
        }
        for attempt in 0..XOR_MAX_BUILD_ATTEMPTS {
            let seed = SEED_BASE.wrapping_mul(u64::from(attempt) + 1);
            if let Some(filter) = Self::try_build(&keys, seed) {
                return Ok(filter);
            }
        }
        Err(XorError::BuildFailed {
            attempts: XOR_MAX_BUILD_ATTEMPTS,
        })
    }

    /// Returns the empty filter: zero slots, `false` for every key.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            seed: 0,
            element_count: 0,
            fingerprints: Vec::new(),
        }
    }

    /// Rebuilds a filter from decoded image parts.
    ///
    /// Only the codec calls this, and only after it has checked the shape: the
    /// slot count inside the format limit, the element count consistent with it,
    /// and the fingerprint array exactly as long as the slot count. The parts
    /// are therefore trusted by construction; no other caller can reach this.
    #[must_use]
    pub(crate) fn from_parts(seed: u64, element_count: u64, fingerprints: Vec<u8>) -> Self {
        Self {
            seed,
            element_count,
            fingerprints,
        }
    }

    /// Returns whether `key` may be in the built set.
    ///
    /// `false` is definitive (the key is absent); `true` means the key is
    /// present or a false positive, so the caller must continue with exact
    /// evaluation.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        let slots = self.fingerprints.len();
        if slots == 0 {
            return false;
        }
        let hashes = hash::derive(hash::hash_key(key), slots, self.seed);
        self.matches(&hashes)
    }

    /// Returns whether the slot fingerprints of `hashes` encode its fingerprint.
    #[must_use]
    fn matches(&self, hashes: &KeyHashes) -> bool {
        let combined =
            self.fingerprints[hashes.a] ^ self.fingerprints[hashes.b] ^ self.fingerprints[hashes.c];
        u64::from(combined) == hashes.fingerprint
    }

    /// Returns the number of keys the filter was built from.
    #[must_use]
    pub fn len(&self) -> usize {
        self.element_count as usize
    }

    /// Returns whether the filter was built from no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.element_count == 0
    }

    /// Returns the number of fingerprint slots.
    #[must_use]
    pub fn slots(&self) -> usize {
        self.fingerprints.len()
    }

    /// Returns the seed the slot indices were derived from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns the fingerprint bytes, one per slot.
    #[must_use]
    pub fn fingerprints(&self) -> &[u8] {
        &self.fingerprints
    }

    /// Returns the false-positive probability of one membership test.
    #[must_use]
    pub fn false_positive_rate(&self) -> f64 {
        1.0 / XOR_FINGERPRINT_MODULUS as f64
    }

    /// One construction attempt with a fixed `seed`.
    ///
    /// Returns `None` unless the whole key set peels *and* the assembled filter
    /// verifies against every key, so a returned filter has no false negatives
    /// by test, not by argument.
    fn try_build(keys: &[&[u8]], seed: u64) -> Option<Self> {
        let slots = slot_count_for(keys.len());
        let mut hashes: Vec<KeyHashes> = keys
            .iter()
            .map(|key| hash::derive(hash::hash_key(key), slots, seed))
            .collect();
        // Repeated keys collapse *before* peeling, because a duplicated key
        // counts twice against each slot it touches and would leave those slots
        // permanently at a count above one. The slot list is reordered by the
        // deduplication, so nothing below may index it by key position.
        deduplicate(&mut hashes);

        let order = peel(&hashes, slots)?;
        let fingerprints = assign(&hashes, &order, slots);
        let filter = Self {
            seed,
            element_count: keys.len() as u64,
            fingerprints,
        };

        // The constructor's contract: every key that went in must be found.
        // `contains` re-derives the key's slots and re-runs the membership
        // equation, so this covers distinct and repeated keys alike without
        // relying on the deduplicated order.
        if keys.iter().all(|key| filter.contains(key)) {
            Some(filter)
        } else {
            None
        }
    }
}

/// Collapses repeated keys in `hashes` in place, leaving one entry per distinct
/// hash.
///
/// A repeated key is the *normal* case for a column filter — a column with a
/// repeated value produces a byte-identical key — and it is what makes the
/// difference between a constructible key set and an unconstructible one. A
/// duplicated key touches each of its three slots twice, so those slots sit at a
/// count of two; [`peel`] only ever peels through a slot held by exactly one
/// key, so a duplicated key can never be peeled and the build fails no matter
/// which seed is tried.
///
/// Two keys are one entry when they derive the same three slots *and* the same
/// fingerprint. Such a pair states the identical membership equation, so no
/// filter can tell them apart and collapsing them discards nothing. Sorting on
/// all four fields puts equal entries adjacent, which is what [`Vec::dedup`]
/// needs; it is the only part of construction that is not linear.
fn deduplicate(hashes: &mut Vec<KeyHashes>) {
    hashes.sort_unstable_by_key(|key| (key.a, key.b, key.c, key.fingerprint));
    hashes.dedup();
}

/// Peels the keys, returning each with the slot it was peeled through.
///
/// A key is peeled once exactly one of its three slots holds no other key: that
/// slot's value is then derivable from the key's fingerprint alone. Peel order
/// is what makes the assembly exact — the slot a key is peeled through is
/// written once, and the other two slots are already final by then. A failure
/// to peel every key is a structural property of this key set and seed, so the
/// caller retries with another seed.
fn peel(hashes: &[KeyHashes], slots: usize) -> Option<Vec<(usize, usize)>> {
    // Per slot: how many not-yet-peeled keys touch it, and their index XOR. A
    // slot with a count of one has its single key recoverable from that XOR.
    let mut counts = vec![0u16; slots];
    let mut occupants = vec![0u32; slots];
    for (index, key) in hashes.iter().enumerate() {
        for slot in [key.a, key.b, key.c] {
            counts[slot] = counts[slot].saturating_add(1);
            occupants[slot] ^= index as u32;
        }
    }

    let mut queue: Vec<usize> = (0..slots).filter(|slot| counts[*slot] == 1).collect();
    let mut peeled = vec![false; hashes.len()];
    let mut order = Vec::with_capacity(hashes.len());
    while let Some(slot) = queue.pop() {
        if counts[slot] != 1 {
            continue;
        }
        let index = occupants[slot] as usize;
        if peeled[index] {
            continue;
        }
        peeled[index] = true;
        order.push((index, slot));
        counts[slot] = 0;
        for other in [hashes[index].a, hashes[index].b, hashes[index].c] {
            counts[other] = counts[other].saturating_sub(1);
            occupants[other] ^= index as u32;
            if counts[other] == 1 {
                queue.push(other);
            }
        }
    }
    if order.len() == hashes.len() {
        Some(order)
    } else {
        None
    }
}

/// Fills the slots in reverse peel order.
///
/// For each key the XOR of its three slots must equal its fingerprint, so the
/// peeled slot is set to `fp ^ (the other two slots)`. Those two are already
/// final (their keys were peeled later, and so assigned earlier here) and the
/// peeled slot is written exactly once, which makes the whole assignment exact.
fn assign(hashes: &[KeyHashes], order: &[(usize, usize)], slots: usize) -> Vec<u8> {
    let mut fingerprints = vec![0u8; slots];
    for (index, target) in order.iter().rev() {
        let key = &hashes[*index];
        let others = [key.a, key.b, key.c]
            .into_iter()
            .filter(|slot| slot != target)
            .collect::<Vec<usize>>();
        let occupied = fingerprints[others[0]] ^ fingerprints[others[1]];
        fingerprints[*target] = occupied ^ (key.fingerprint as u8);
    }
    fingerprints
}

#[cfg(test)]
mod tests {
    use super::{slot_count_for, XorFilter, XOR_ALPHA_DEN, XOR_ALPHA_NUM, XOR_SLOTS_SLACK};
    use crate::xor::hash;
    use crate::xor::XorError;

    /// Builds `count` distinct keys whose byte images do not collide.
    fn keys_for(count: usize, nonce: u64) -> Vec<[u8; 4]> {
        (0..count as u64)
            .map(|index| {
                let mixed = index
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(nonce.wrapping_mul(0x9E37_79B9))
                    ^ nonce.wrapping_mul(0x85EB_CA6B);
                (mixed as u32).to_le_bytes()
            })
            .collect()
    }

    fn build(keys: &[[u8; 4]]) -> XorFilter {
        XorFilter::build(keys.iter().map(|key| &key[..]))
            .expect("construction must succeed for this key set")
    }

    /// The regression guard for the construction failure this module shipped
    /// with: a slot ratio below `11/9` makes peeling impossible, so every seed
    /// fails and `build` can never return a filter. The same inequality is
    /// asserted at compile time above; this test pins the ratio itself so a
    /// silent retune of the constant has to be deliberate.
    #[test]
    fn slot_ratio_is_the_documented_value() {
        // Bound to locals so the comparison is a runtime one, which is what the
        // test is for: the compile-time assertion above proves the inequality,
        // this proves which ratio is in force.
        let (numerator, denominator) = (XOR_ALPHA_NUM, XOR_ALPHA_DEN);
        assert_eq!(numerator, 27, "the slot ratio was retuned");
        assert_eq!(denominator, 20, "the slot ratio was retuned");
        assert!(
            numerator * 9 > denominator * 11,
            "ratio {numerator}/{denominator} is at or below 11/9, \
             where a random 3-uniform hypergraph never peels"
        );
    }

    #[test]
    fn slot_count_is_a_nonempty_multiple_of_three() {
        for count in [0usize, 1, 2, 3, 4, 17, 64, 1000, 999_983] {
            let slots = slot_count_for(count);
            assert_eq!(slots % 3, 0, "count {count} gave {slots} slots");
            assert!(slots >= 3, "count {count} gave {slots} slots");
        }
        // The slack is what keeps small key sets constructible; without it the
        // rounding to a multiple of three leaves them at an unpeelable load.
        assert_eq!(slot_count_for(1), XOR_SLOTS_SLACK + 4);
        assert_eq!(slot_count_for(3), XOR_SLOTS_SLACK + 7);
    }

    #[test]
    fn slot_count_grows_proportionally_for_large_key_sets() {
        // The additive slack must not distort the asymptotic overhead. Checked
        // in integers so the expectation is exact: 1 000 000 * 27/20 is exactly
        // 1 350 000, plus the 32-slot slack, rounded up to the next multiple of
        // three.
        let slots = slot_count_for(1_000_000);
        assert_eq!(slots, 1_350_033);
        // ...and the result still clears the 11/9 threshold with room to spare.
        assert!(slots * 9 > 1_000_000 * 11);
    }

    #[test]
    fn every_key_gets_three_distinct_slots() {
        let slots = slot_count_for(64);
        for key in keys_for(64, 7) {
            let hashes = hash::derive(hash::hash_key(&key), slots, 0);
            assert!(hashes.a < slots && hashes.b < slots && hashes.c < slots);
            assert_ne!(hashes.a, hashes.b);
            assert_ne!(hashes.b, hashes.c);
            assert_ne!(hashes.a, hashes.c);
            assert!(hashes.fingerprint != 0, "fingerprints are never zero");
        }
    }

    /// Small key sets are the case the additive slack exists for: at these sizes
    /// the proportional term alone is a coin flip.
    #[test]
    fn small_key_sets_build_for_every_nonce() {
        for count in [1usize, 2, 3, 5, 7, 16, 64] {
            for nonce in 0..8u64 {
                let keys = keys_for(count, nonce);
                let filter = build(&keys);
                assert_eq!(filter.slots(), slot_count_for(count));
                assert_eq!(filter.len(), count);
                assert!(!filter.is_empty());
            }
        }
    }

    #[test]
    fn large_key_sets_build_and_keep_every_key() {
        for nonce in 0..4u64 {
            let keys = keys_for(10_000, nonce);
            let filter = build(&keys);
            assert_eq!(filter.len(), 10_000);
            assert!(keys.iter().all(|key| filter.contains(key)));
        }
    }

    #[test]
    fn absent_keys_are_rejected_at_the_expected_rate() {
        let filter = build(&keys_for(1000, 0x1234));
        // Probe values far above the key space: the builder packs keys in
        // [0, 1000), so these are all absent.
        let probes = 20_000u32;
        let positives = (u32::MAX - probes..u32::MAX)
            .filter(|value| filter.contains(&value.to_le_bytes()))
            .count();
        // Expected is 1/256 ≈ 0.39%, so ~78 of 20 000. The band absorbs the
        // sampling noise of one seed while staying far below the 1/2 a filter
        // that ignored its fingerprints would reach.
        assert!(
            positives < 200,
            "false-positive rate too high: {positives}/{probes}"
        );
        assert!(positives > 0, "the check must be able to fail at all");
    }

    #[test]
    fn empty_key_set_yields_a_filter_that_contains_nothing() {
        let filter = XorFilter::build(std::iter::empty::<&[u8]>()).expect("an empty build is fine");
        assert!(filter.is_empty());
        assert_eq!(filter.len(), 0);
        assert_eq!(filter.slots(), 0);
        assert!(!filter.contains(b""));
        assert!(!filter.contains(b"anything"));
    }

    #[test]
    fn empty_filter_matches_the_empty_image() {
        let filter = XorFilter::empty();
        assert_eq!(filter, XorFilter::deserialize(&filter.serialize()).unwrap());
    }

    #[test]
    fn filters_round_trip_through_an_image() {
        for count in [1usize, 7, 1000] {
            let keys = keys_for(count, 0x5EED);
            let filter = build(&keys);
            let decoded =
                XorFilter::deserialize(&filter.serialize()).expect("own image must decode");
            assert_eq!(decoded, filter);
            assert!(keys.iter().all(|key| decoded.contains(key)));
        }
    }

    #[test]
    fn element_budget_is_enforced() {
        // The budget check runs before any slot allocation, so a lazily-shaped
        // iterator is enough to drive it.
        let over = crate::constants::XOR_MAX_ELEMENTS + 1;
        let result = XorFilter::build((0..over).map(|_| b"k".as_ref()));
        assert_eq!(
            result,
            Err(XorError::TooManyElements {
                requested: over,
                limit: crate::constants::XOR_MAX_ELEMENTS,
            })
        );
    }

    #[test]
    fn false_positive_rate_is_the_fingerprint_space() {
        let filter = build(&keys_for(16, 3));
        assert!((filter.false_positive_rate() - 1.0 / 256.0).abs() < f64::EPSILON);
    }

    /// Repeated keys are the normal case for a column filter, and they used to
    /// make construction fail on *every* seed: a duplicated key touches each of
    /// its slots twice, so no slot ever holds exactly one key and nothing can be
    /// peeled. This is the regression guard for that failure.
    #[test]
    fn repeated_keys_collapse_before_peeling() {
        let distinct = keys_for(1000, 0xD00D);
        // Every key three times over, as a column with three-way repeats.
        let keys: Vec<[u8; 4]> = distinct
            .iter()
            .chain(&distinct)
            .chain(&distinct)
            .copied()
            .collect();
        let filter = build(&keys);
        assert_eq!(filter.len(), keys.len());
        assert!(keys.iter().all(|key| filter.contains(key)));
    }

    #[test]
    fn a_single_repeated_key_collapses_to_one_entry() {
        let key = [7u8, 0, 0, 0];
        let filter = build(&vec![key; 64]);
        assert_eq!(filter.len(), 64);
        assert!(filter.contains(&key));
    }

    #[test]
    fn repeated_key_filters_round_trip_through_an_image() {
        let distinct = keys_for(300, 0xBEEF);
        let doubled: Vec<[u8; 4]> = distinct.iter().chain(&distinct).copied().collect();
        let filter = build(&doubled);
        let decoded = XorFilter::deserialize(&filter.serialize()).expect("own image must decode");
        assert_eq!(decoded, filter);
        assert_eq!(decoded.len(), doubled.len());
        assert!(doubled.iter().all(|key| decoded.contains(key)));
    }

    /// Locks in the fix at the scale it was found at. The benchmark's 100 000
    /// random keys contained two repeats and construction failed there; this
    /// repeats that shape with a far higher repeat count so the guard does not
    /// depend on a birthday collision happening to occur.
    #[test]
    fn large_key_set_with_many_repeats_builds() {
        let mut keys = keys_for(90_000, 0xF00D);
        let head: Vec<[u8; 4]> = keys.iter().take(10_000).copied().collect();
        keys.extend(head);
        assert_eq!(keys.len(), 100_000);
        let filter = build(&keys);
        assert_eq!(filter.len(), 100_000);
        assert!(keys.iter().all(|key| filter.contains(key)));
    }
}
