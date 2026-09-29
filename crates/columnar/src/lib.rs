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
//! Immutable columnar segments for analytical workloads.
//!
//! Convert the Hot Row Store's mutable rows into an immutable, column-oriented
//! representation suitable for analytical scans. The segment is materialized from
//! a consistent MVCC snapshot, compressed, verified, and published through the
//! existing generation machinery.
//!
//! # Architecture
//!
//! ```text
//! HOT ROW STORE
//!       │
//!       ▼
//!     FLUSH
//!       │
//!       ▼
//! IMMUTABLE COLUMNAR SEGMENT
//!       │
//!       ├── column metadata
//!       ├── null bitmap
//!       ├── values
//!       ├── chunks
//!       └── statistics
//!       │
//!       ▼
//!    COMPRESS
//!       │
//!       ▼
//!     VERIFY
//!       │
//!       ▼
//!      SYNC
//!       │
//!       ▼
//!    PUBLISH
//! ```
//!
//! # Pruning (Zone Maps + BRIN)
//!
//! Segments carry analytical pruning metadata — per-column zone maps (min,
//! max, NULL state, row count) grouped into BRIN row ranges — in a CRC32C
//! protected trailer appended to the segment image. A predicate is evaluated
//! against that metadata to produce the candidate row ranges a scan must
//! touch; metadata that is missing, uncertain, unsupported, or malformed never
//! removes a row from the scan. See [`pruning`] for the proof rules.
//!
//! # Persisted format
//!
//! Every persisted columnar structure is an explicit little-endian binary format
//! with magic, format version, explicit lengths, counts, reserved fields, and
//! CRC32C integrity checksums at the segment boundary.

#![forbid(unsafe_code)]

pub mod chunk;
pub mod column;
pub mod compaction;
pub mod compression;
pub mod encoding;
pub mod flush;
pub mod format;
mod layout;
pub mod materialization;
pub mod pruning;
pub mod read;
pub mod segment;
pub mod statistics;
pub mod store;
pub mod vector;

pub use chunk::*;
pub use column::*;
pub use compaction::*;
pub use compression::*;
pub use encoding::*;
pub use flush::*;
pub use format::*;
pub use materialization::*;
pub use pruning::*;
pub use read::*;
pub use segment::*;
pub use statistics::*;
pub use store::*;
