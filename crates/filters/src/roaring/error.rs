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
//! Roaring bitmap errors.

use std::fmt;

/// Everything that can go wrong while decoding, validating, or framing a
/// Roaring bitmap.
///
/// A decoded bitmap is either exactly what the encoder wrote or an error: no
/// variant represents "usable but suspicious", and no error carries a partially
/// built bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoaringError {
    /// The image ended before the named field was complete.
    Truncated(&'static str),
    /// Container keys are not strictly increasing: the repeated key is stored.
    InvalidKeyOrder(u16),
    /// A container was present but held no values.
    EmptyContainer,
    /// The key list and the container list have different lengths.
    MismatchedKeysAndContainers,
    /// A container's values are not strictly increasing.
    UnsortedValues,
    /// A run extends past the end of the container's offset space.
    ValueOutOfRange(u16),
    /// A container carried a type tag this format version does not define.
    UnknownContainerType(u8),
    /// The image had bytes left over after the last container.
    TrailingBytes(usize, usize),
    /// The framing layer rejected the image; the description is the framing
    /// error verbatim (magic, version, flags, length, or checksum).
    Corrupt(&'static str),
}

impl fmt::Display for RoaringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(field) => write!(f, "truncated roaring bitmap: {field}"),
            Self::InvalidKeyOrder(key) => {
                write!(f, "roaring container key out of order: {key}")
            }
            Self::EmptyContainer => write!(f, "roaring bitmap holds an empty container"),
            Self::MismatchedKeysAndContainers => {
                write!(f, "roaring key/container count mismatch")
            }
            Self::UnsortedValues => write!(f, "roaring container values are not sorted"),
            Self::ValueOutOfRange(value) => {
                write!(f, "roaring run ends past the container: {value}")
            }
            Self::UnknownContainerType(tag) => {
                write!(f, "unknown roaring container type: {tag}")
            }
            Self::TrailingBytes(position, total) => {
                write!(
                    f,
                    "trailing bytes after roaring bitmap: {position} of {total}"
                )
            }
            Self::Corrupt(reason) => write!(f, "corrupt roaring bitmap: {reason}"),
        }
    }
}

impl std::error::Error for RoaringError {}

#[cfg(test)]
mod tests {
    use super::RoaringError;

    #[test]
    fn display_names_the_failing_field() {
        assert_eq!(
            RoaringError::Truncated("key").to_string(),
            "truncated roaring bitmap: key"
        );
        assert_eq!(
            RoaringError::Corrupt("filter magic mismatch").to_string(),
            "corrupt roaring bitmap: filter magic mismatch"
        );
    }

    #[test]
    fn errors_are_comparable() {
        assert_eq!(RoaringError::EmptyContainer, RoaringError::EmptyContainer);
        assert_ne!(
            RoaringError::InvalidKeyOrder(1),
            RoaringError::InvalidKeyOrder(2)
        );
    }
}
