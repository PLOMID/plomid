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
//! Type-name helpers shared by the catalog and the modality crates.
//!
//! `custom_type_oid` is a deterministic FNV-1a hash of a user-defined type
//! name: it needs no catalog access, which is what lets value coercion tag a
//! composite/enum/domain value without reaching into the catalog.

pub fn custom_type_oid(name: &str) -> u32 {
    let bare = bare_type_name(name);
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bare.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    100_000 + hash % 100_000_000
}

/// The unqualified, lowercased portion of a (possibly schema-qualified) name.
pub fn bare_type_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}
