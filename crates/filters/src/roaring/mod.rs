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
//! Roaring-style compressed bitmaps over a 32-bit value space.
//!
//! A Roaring bitmap splits the 32-bit value space into `2^16` buckets, one per
//! 16-bit high key. Each non-empty bucket holds the values
//! `key << 16 | offset` and is stored in the most compact of three container
//! representations, chosen deterministically:
//!
//! ```text
//! bitmap = sorted keys[2^16 buckets] ‖ one container per key
//!                                   │
//!   cardinality ≤ 4096 ─────────────┼──▶ Array  { values:   [u16]      }  2 B/value
//!   runs·4 B < 8 KiB   ─────────────┼──▶ Run    { (start,len): [u16;2] }  4 B/run
//!   otherwise          ─────────────┴──▶ Bitmap { words: [u64; 1024] }     8 KiB
//! ```
//!
//! The encoding is canonical: the same value set always produces the same
//! container family and the same bytes. Decoding accepts any well-formed image
//! (including containers that are not the most compact choice, as produced by
//! other Roaring implementations) and validates every invariant before the
//! bitmap becomes usable.
//!
//! # Module map
//!
//! * [`error`] — [`RoaringError`]: every way decoding, validation, or framing
//!   can fail.
//! * [`container`] — [`Container`] plus the [`ContainerIter`] over the buckets
//!   of a bitmap.
//! * [`containers`] — the per-representation algorithms: [`containers::array`],
//!   [`containers::bitmap`], and [`containers::run`] codecs and value
//!   algorithms, with [`containers::set_ops`] for container-level set
//!   arithmetic.
//! * [`offsets`] — the sorted-offset merge primitives that container set
//!   arithmetic is defined in terms of.
//! * [`bitmap`] — [`RoaringBitmap`]: membership, mutation, and construction.
//! * [`ranges`] — value-range tests, insertion, and removal.
//! * [`set_ops`] — union, intersection, difference, symmetric difference, and
//!   the subset/superset/overlap relations ([`RelationalOp`]).
//! * [`aggregate`] — minimum, maximum, and cardinality of a value range.
//! * [`iter`] — [`Iter`] over values and [`ContainerIter`] over buckets.
//! * [`validate`] — the invariant checker behind [`RoaringBitmap::validate`].
//! * [`codec`] — the persisted image: body encoding plus the framed
//!   serialization API ([`RoaringBitmap::serialize`] and friends).

pub(crate) mod aggregate;
pub(crate) mod bitmap;
pub(crate) mod codec;
pub(crate) mod container;
pub(crate) mod containers;
pub(crate) mod error;
pub(crate) mod iter;
pub(crate) mod offsets;
pub(crate) mod ranges;
pub(crate) mod set_ops;
pub(crate) mod validate;

pub use bitmap::RoaringBitmap;
pub use container::{Container, RunWord};
pub use error::RoaringError;
pub use iter::{ContainerIter, Iter};
pub use set_ops::RelationalOp;
