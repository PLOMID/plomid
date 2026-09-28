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
//! Transaction identity + lifecycle context carried through operations.
use plomid_core::{ErrorKind, PlomidError, Result, TxnId};

use super::Snapshot;

/// Lifecycle state of one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    /// Accepting operations.
    Active,
    /// Durably committed and visible per snapshot rules.
    Committed,
    /// Aborted; its versions never become visible to others.
    Aborted,
}

/// Transaction context carried through real storage operations.
#[derive(Debug, Clone)]
pub struct TransactionContext {
    /// Identity of the owning transaction.
    pub txn_id: TxnId,
    /// Consistent snapshot taken when the transaction began.
    pub snapshot: Snapshot,
    /// Current lifecycle state.
    pub state: TxnState,
}

impl TransactionContext {
    /// Creates an active transaction context for `txn_id` with `snapshot`.
    #[must_use]
    pub fn new(txn_id: TxnId, snapshot: Snapshot) -> Self {
        Self {
            txn_id,
            snapshot,
            state: TxnState::Active,
        }
    }

    /// Marks the context committed; rejects double commit and commit-after-abort.
    pub fn mark_committed(&mut self) -> Result<()> {
        match self.state {
            TxnState::Active => {
                self.state = TxnState::Committed;
                Ok(())
            }
            TxnState::Committed => Err(PlomidError::new(
                ErrorKind::Transaction,
                "transaction is already committed",
            )),
            TxnState::Aborted => Err(PlomidError::new(
                ErrorKind::Transaction,
                "aborted transaction cannot commit",
            )),
        }
    }

    /// Marks the context aborted; rejects double abort and abort-after-commit.
    pub fn mark_aborted(&mut self) -> Result<()> {
        match self.state {
            TxnState::Active => {
                self.state = TxnState::Aborted;
                Ok(())
            }
            TxnState::Aborted => Err(PlomidError::new(
                ErrorKind::Transaction,
                "transaction is already aborted",
            )),
            TxnState::Committed => Err(PlomidError::new(
                ErrorKind::Transaction,
                "committed transaction cannot abort",
            )),
        }
    }

    /// Returns true while the context accepts operations.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == TxnState::Active
    }
}
