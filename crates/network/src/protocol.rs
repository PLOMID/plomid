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
//! PostgreSQL frontend/backend wire protocol message definitions for PLOMID.
//!
//! This module re-exports the protocol message types and the wire codec.
//! The message definitions live in `message.rs` and the encoder/decoder
//! live in `codec.rs`.

pub mod codec;
pub mod message;

pub use codec::{MessageDecoder, MessageEncoder, MAX_FRONTEND_MESSAGE_SIZE};
pub use message::{
    AuthMechanism, AuthenticationRequest, BackendKeyData, CommandCompleteMessage, DataRowMessage,
    ErrorResponse, FrontendTag, MessageTag, ParameterStatus, QueryMessage, RowFieldDescription,
    TransactionStatus,
};
