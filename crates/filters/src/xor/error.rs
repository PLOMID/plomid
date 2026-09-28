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
//! XOR filter errors.
//!
//! Construction failure is a *controlled* outcome, not a panic: a build either
//! produces a filter that was verified to contain every inserted element or
//! returns one of the variants below. Decoding failure follows the same rule as
//! the Roaring codec — a filter is never used from an image that failed a
//! check.

use std::fmt;

/// Everything that can go wrong while building or decoding an XOR filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XorError {
    /// The key set is larger than the format's slot budget allows.
    TooManyElements {
        /// Number of elements offered to the builder.
        requested: usize,
        /// Largest element count this format supports.
        limit: usize,
    },
    /// Every construction attempt failed to peel *and verify* the key set.
    ///
    /// The filter does not exist in a degraded form: callers must fall back to
    /// exact evaluation (or rebuild with a different key set).
    BuildFailed {
        /// Number of seeds tried, equal to the configured attempt limit.
        attempts: u32,
    },
    /// The framing layer rejected the image; the description is the framing
    /// error verbatim (magic, version, flags, length, or checksum).
    Corrupt(&'static str),
    /// The image ended before the named field was complete.
    Truncated(&'static str),
    /// A stored count is inconsistent with the rest of the image.
    Inconsistent(&'static str),
}

impl fmt::Display for XorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyElements { requested, limit } => {
                write!(f, "xor filter holds {requested} elements, limit {limit}")
            }
            Self::BuildFailed { attempts } => {
                write!(
                    f,
                    "xor filter construction failed after {attempts} attempts"
                )
            }
            Self::Corrupt(reason) => write!(f, "corrupt xor filter: {reason}"),
            Self::Truncated(field) => write!(f, "truncated xor filter: {field}"),
            Self::Inconsistent(reason) => write!(f, "inconsistent xor filter: {reason}"),
        }
    }
}

impl std::error::Error for XorError {}

#[cfg(test)]
mod tests {
    use super::XorError;

    #[test]
    fn display_states_the_failing_condition() {
        assert_eq!(
            XorError::BuildFailed { attempts: 5 }.to_string(),
            "xor filter construction failed after 5 attempts"
        );
        assert_eq!(
            XorError::TooManyElements {
                requested: 10,
                limit: 4
            }
            .to_string(),
            "xor filter holds 10 elements, limit 4"
        );
    }
}
