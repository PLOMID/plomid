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
//! XOR filters: probabilistic membership for cheap candidate rejection.
//!
//! An XOR filter stores one fingerprint per slot in an array of `m ≈ 1.35n`
//! slots, laid out so that each key's three assigned slots XOR to exactly that
//! key's fingerprint:
//!
//! ```text
//! slot array      [ f0 | f1 | f2 | … | f(m-1) ]          fingerprint per slot
//! key ──hash──▶   a, b, c   (one per third of the array)
//! contains(key) ⇔ f[a] ^ f[b] ^ f[c] == fingerprint(key)
//! ```
//!
//! Guarantees, in the order they matter for query correctness:
//!
//! * **no false negatives** — every inserted key satisfies the equation, and
//!   the constructor re-checks exactly that against every key before returning
//!   the filter;
//! * **bounded false positives** — an absent key satisfies the equation with
//!   probability `1/2^XOR_FINGERPRINT_BITS` (≈ 0.39%);
//! * **controlled failure** — construction either succeeds or returns
//!   [`XorError::BuildFailed`]; a filter is never returned in a degraded form.
//!
//! A `true` answer therefore only ever means "may be present": callers must
//! fall through to exact evaluation. A `false` answer may skip evaluation
//! entirely, which is the whole point of the structure.
//!
//! # Module map
//!
//! * [`error`] — [`XorError`]: build and decode failures.
//! * [`hash`] — the deterministic FNV-1a / splitmix64 key hashing, so a
//!   persisted filter is reproducible across builds and platforms.
//! * [`filter`] — [`XorFilter`]: construction, membership, and accessors.
//! * [`codec`] — the framed, checksummed image ([`XorFilter::serialize`] and
//!   [`XorFilter::deserialize`]).
//!
//! The image format is shared with the Roaring codec: a body followed by the
//! crate's 40-byte footer ([`layout`](crate::layout)).

pub(crate) mod codec;
pub(crate) mod error;
pub(crate) mod filter;
pub(crate) mod hash;

pub use error::XorError;
pub use filter::XorFilter;
