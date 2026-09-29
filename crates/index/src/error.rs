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
//! Error kinds for the ART index crate.

use plomid_core::ErrorKind;

/// ART-specific error classification.
///
/// These are kept separate from the engine's durable error kinds because
/// the ART core is deliberately independent: a key-not-found in an
/// in-memory ART is not the same contract as a missing page on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtError {
    /// The requested key was not present.
    NotFound,
    /// An insert would violate a key uniqueness constraint.
    DuplicateKey,
    /// An operation was invalid for the current index kind.
    InvalidOperation,
    /// The structure failed an internal invariant check.
    Corrupt,
    /// The requested operation is not supported by this index.
    Unsupported,
}

impl std::fmt::Display for ArtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "ART key not found"),
            Self::DuplicateKey => {
                write!(f, "duplicate key value violates unique index")
            }
            Self::InvalidOperation => write!(f, "invalid ART operation"),
            Self::Corrupt => write!(f, "ART structure corrupted"),
            Self::Unsupported => write!(f, "unsupported ART operation"),
        }
    }
}

impl std::error::Error for ArtError {}

impl From<ArtError> for plomid_core::PlomidError {
    fn from(err: ArtError) -> Self {
        match err {
            ArtError::NotFound => Self::new(ErrorKind::NotFound, err.to_string()),
            ArtError::DuplicateKey => Self::new(ErrorKind::Conflict, err.to_string()),
            ArtError::InvalidOperation => Self::new(ErrorKind::InvalidArgument, err.to_string()),
            ArtError::Corrupt => Self::new(ErrorKind::Corruption, err.to_string()),
            ArtError::Unsupported => Self::new(ErrorKind::Unsupported, err.to_string()),
        }
    }
}
