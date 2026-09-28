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
//! Stage-attributed mount and demount errors.

use crate::stages::{DemountStage, MountStage};
use plomid_core::{ErrorKind, PlomidError};
use std::fmt;

/// A mount failure attributed to the lifecycle stage that caused it.
#[derive(Debug)]
pub struct MountError {
    stage: MountStage,
    source: PlomidError,
}

impl MountError {
    pub(crate) fn new(stage: MountStage, source: PlomidError) -> Self {
        Self { stage, source }
    }

    /// Returns the mount stage that failed.
    #[must_use]
    pub fn stage(&self) -> MountStage {
        self.stage
    }

    /// Returns the stable error category of the underlying failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.source.kind()
    }

    /// Consumes the wrapper and returns the underlying [`PlomidError`].
    #[must_use]
    pub fn into_inner(self) -> PlomidError {
        self.source
    }
}

impl fmt::Display for MountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "mount failed at stage {:?}: {}",
            self.stage, self.source
        )
    }
}

impl std::error::Error for MountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<MountError> for PlomidError {
    fn from(error: MountError) -> Self {
        let MountError { stage, source } = error;
        let kind = source.kind();
        PlomidError::with_source(kind, format!("mount failed at stage {stage:?}"), source)
    }
}

/// A demount failure attributed to the lifecycle stage that caused it.
#[derive(Debug)]
pub struct DemountError {
    stage: DemountStage,
    source: PlomidError,
}

impl DemountError {
    pub(crate) fn new(stage: DemountStage, source: PlomidError) -> Self {
        Self { stage, source }
    }

    /// Returns the demount stage that failed.
    #[must_use]
    pub fn stage(&self) -> DemountStage {
        self.stage
    }

    /// Returns the stable error category of the underlying failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.source.kind()
    }

    /// Consumes the wrapper and returns the underlying [`PlomidError`].
    #[must_use]
    pub fn into_inner(self) -> PlomidError {
        self.source
    }
}

impl fmt::Display for DemountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "demount failed at stage {:?}: {}",
            self.stage, self.source
        )
    }
}

impl std::error::Error for DemountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<DemountError> for PlomidError {
    fn from(error: DemountError) -> Self {
        let DemountError { stage, source } = error;
        let kind = source.kind();
        PlomidError::with_source(kind, format!("demount failed at stage {stage:?}"), source)
    }
}
