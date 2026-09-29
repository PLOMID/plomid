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
//! Shared naming primitives of the database layout.
//!
//! Names that describe *where* durable state lives belong to the layout, so
//! they live next to the layout modules that use them rather than in the global
//! constant list. Only the naming rules that several layout modules share are
//! defined here.
//!
//! Every logical identity is rendered as a fixed-width zero-padded decimal
//! number, so ordering by name is ordering by identity and a directory name can
//! never be confused with a prefix of another one.

/// Width of every rendered layout identity (`T-…`, `GEN-…`, `I-…`, `D-…`).
pub const LAYOUT_ID_DIGITS: usize = 20;

/// Suffix of a staging artifact written before an atomic rename.
pub const STAGING_SUFFIX: &str = ".tmp";

/// Name of a metadata record inside a table, generation, or index directory.
pub const META_FILE_NAME: &str = "META.dat";

/// Renders a layout identity with the fixed layout width.
///
/// The identity is zero-padded to [`LAYOUT_ID_DIGITS`], so ordering by name is
/// ordering by identity and a name can never be a prefix of another one.
#[must_use]
pub fn render_layout_id(value: u64) -> String {
    format!("{value:0LAYOUT_ID_DIGITS$}")
}

/// Parses the identity of a layout directory name such as `T-0000000000000001`.
///
/// Returns `None` for anything that is not exactly a fixed-width non-zero
/// identifier with the expected prefix, so unrelated files (and staging
/// artifacts) are never mistaken for layout directories.
#[must_use]
pub(crate) fn parse_layout_id(name: &str, prefix: &str) -> Option<u64> {
    let number = name.strip_prefix(prefix)?;
    if number.len() != LAYOUT_ID_DIGITS || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = number.parse::<u64>().ok()?;
    if value == 0 {
        return None;
    }
    Some(value)
}

/// Renders a layout directory name with a prefix and the fixed layout width.
#[must_use]
pub(crate) fn render_prefixed_id(prefix: &str, value: u64) -> String {
    format!("{prefix}{}", render_layout_id(value))
}

#[cfg(test)]
mod tests {
    use super::{parse_layout_id, render_layout_id, LAYOUT_ID_DIGITS};

    #[test]
    fn rendering_is_fixed_width_and_round_trips() {
        let rendered = render_layout_id(1);
        assert_eq!(rendered.len(), LAYOUT_ID_DIGITS);
        assert_eq!(rendered, "00000000000000000001");
        assert_eq!(parse_layout_id("T-00000000000000000001", "T-"), Some(1));
        assert_eq!(
            parse_layout_id(&format!("GEN-{}", render_layout_id(u64::MAX)), "GEN-"),
            Some(u64::MAX)
        );
    }

    #[test]
    fn unrelated_names_are_rejected() {
        assert_eq!(parse_layout_id("T-1", "T-"), None);
        assert_eq!(parse_layout_id("T-00000000000000000000", "T-"), None);
        assert_eq!(parse_layout_id("I-00000000000000000001", "T-"), None);
        assert_eq!(parse_layout_id("T-0000000000000000000a", "T-"), None);
        assert_eq!(parse_layout_id("META.dat", "T-"), None);
    }
}
