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
//! Node size constants and growth/shrink thresholds for the ART node families.
//!
//! These mirror the standard adaptive radix tree transition points:
//!   Node4  → Node16  → Node48  → Node256  (growth)
//!   Node256 → Node48 → Node16  → Node4    (shrink)
//!
//! Thresholds are chosen so a node grows one level after inserting the
//! (capacity + 1)-th child, and shrinks one level after decreasing to the
//! shrink threshold of the current node type.

/// Maximum children before a node must grow to the next type.
pub const ART_NODE4_CAP: usize = 4;
pub const ART_NODE16_CAP: usize = 16;
pub const ART_NODE48_CAP: usize = 48;
pub const ART_NODE256_CAP: usize = 256;

/// Child count at which a node of the given type should shrink to the
/// previous type. These are deliberately lower than the growth threshold
/// to avoid flapping.
pub const ART_NODE256_SHRINK: usize = 48; // shrink when <= 48 children
pub const ART_NODE48_SHRINK: usize = 16; // shrink when <= 16 children
pub const ART_NODE16_SHRINK: usize = 4; // shrink when <= 4 children

/// Maximum children for each node type, indexed by node type enum.
pub const NODE_CAPACITY: [usize; 4] = [
    ART_NODE4_CAP,
    ART_NODE16_CAP,
    ART_NODE48_CAP,
    ART_NODE256_CAP,
];

/// Shrink threshold for each node type, indexed by node type enum.
pub const NODE_SHRINK: [usize; 4] = [
    ART_NODE4_CAP, // Node4 never shrinks
    ART_NODE16_SHRINK,
    ART_NODE48_SHRINK,
    ART_NODE256_SHRINK,
];
