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
//! Explicit lifecycle state machine stages.

use std::fmt;

/// Explicit lifecycle state machine of a [`super::LifecycleManager`].
///
/// Illegal transitions return errors; they never silently succeed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LifecycleState {
    /// No storage is open. The initial and final state.
    Closed,
    /// A mount is in progress. Mutations are rejected.
    Mounting,
    /// The storage is mounted and may serve operations.
    Ready,
    /// A demount is in progress. Mutations are rejected.
    Demounting,
    /// The last mount or demount failed; owned resources were released.
    Failed,
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Stages of the mount sequence, in execution order.
///
/// `Lifecycle` is not a mount step: it labels errors caused by an illegal
/// lifecycle transition (for example a second mount of a mounted manager).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum MountStage {
    /// Illegal lifecycle transition (mount while mounted, and so on).
    Lifecycle,
    /// Open the storage device (segments and WAL).
    Device,
    /// Validate the physical storage format and layout.
    FormatValidation,
    /// Validate durable metadata (WAL boundary, publication pointer).
    MetadataValidation,
    /// Locate and validate the checkpoint state.
    Checkpoint,
    /// Recover the WAL into the storage image.
    Recovery,
    /// Load the authoritative catalog.
    Catalog,
    /// Establish the active generation.
    Generation,
}

/// Stages of the demount sequence, in execution order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DemountStage {
    /// Illegal lifecycle transition (demount while not mounted).
    Lifecycle,
    /// Stop mutations: the mutation gate begins rejecting operations.
    StopMutations,
    /// Flush required dirty state through existing mechanisms.
    Flush,
    /// Checkpoint if required.
    Checkpoint,
    /// Final durability synchronization.
    Sync,
    /// Close owned resources.
    Close,
}
