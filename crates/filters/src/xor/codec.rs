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
//! The persisted XOR filter image.
//!
//! ```text
//! body  = element_count:u64 ‖ slot_count:u64 ‖ seed:u64 ‖ fingerprints:[u8; slot_count]
//! image = body ‖ footer (magic, version, flags, lengths, CRCs)   ← layout::frame
//! ```
//!
//! Shape first, data second: all three header fields are read and cross-checked
//! before a single fingerprint byte is touched, so a damaged count is reported
//! as corruption instead of driving an allocation. The image is deterministic —
//! the seed is part of the format, not of the machine — so two builds from the
//! same key set in the same order produce identical bytes.
//!
//! A decoded filter is *not* re-verified against a key set: the image carries no
//! keys. What decoding does guarantee is that the slots, seed, and checksum are
//! exactly what an encoder wrote, which is all the no-false-negative property
//! needs from the image itself.

use crate::constants::{
    XOR_FILTER_FORMAT_VERSION, XOR_FILTER_MAGIC, XOR_MAX_ELEMENTS, XOR_MAX_SLOTS,
};
use crate::layout::{self, read_u64};
use crate::xor::error::XorError;
use crate::xor::filter::XorFilter;

/// Bytes of the fixed body header: element count, slot count, and seed.
const HEADER_LEN: usize = 24;

impl XorFilter {
    /// Encodes the filter body (header plus fingerprints, without the footer).
    #[must_use]
    pub fn encode_body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.slots());
        layout::push_u64(&mut out, self.len() as u64);
        layout::push_u64(&mut out, self.slots() as u64);
        layout::push_u64(&mut out, self.seed());
        out.extend_from_slice(self.fingerprints());
        out
    }

    /// Decodes a filter from its body.
    ///
    /// # Errors
    ///
    /// Returns [`XorError::Truncated`] when the header or the fingerprint array
    /// is incomplete, and [`XorError::Inconsistent`] when the slot count exceeds
    /// the format limit, when the element count exceeds the slot count's
    /// element budget, or when the body length disagrees with the header.
    pub fn decode_body(image: &[u8]) -> Result<Self, XorError> {
        let element_count =
            read_u64(image, 0).ok_or(XorError::Truncated("element count"))? as usize;
        let slots = read_u64(image, 8).ok_or(XorError::Truncated("slot count"))? as usize;
        let seed = read_u64(image, 16).ok_or(XorError::Truncated("seed"))?;

        if slots > XOR_MAX_SLOTS {
            return Err(XorError::Inconsistent(
                "slot count exceeds the format limit",
            ));
        }
        if element_count > XOR_MAX_ELEMENTS {
            return Err(XorError::Inconsistent(
                "element count exceeds the format limit",
            ));
        }
        // The empty filter is the exact shape (no elements, no slots); every
        // other filter has more slots than elements, because the load factor is
        // above 1. Anything else is a damaged header.
        if (element_count == 0) != (slots == 0) {
            return Err(XorError::Inconsistent(
                "element and slot counts disagree about emptiness",
            ));
        }
        if element_count > slots {
            return Err(XorError::Inconsistent(
                "element count exceeds the slot count",
            ));
        }

        let body_len = HEADER_LEN + slots;
        if image.len() != body_len {
            return Err(XorError::Inconsistent(
                "body length disagrees with the header",
            ));
        }
        let fingerprints = image[HEADER_LEN..body_len].to_vec();
        Ok(Self::from_parts(seed, element_count as u64, fingerprints))
    }

    /// Encodes the filter into the framed, checksummed image.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        layout::frame(
            XOR_FILTER_MAGIC,
            XOR_FILTER_FORMAT_VERSION,
            &self.encode_body(),
        )
    }

    /// Decodes a filter from a framed image.
    ///
    /// # Errors
    ///
    /// Returns [`XorError::Corrupt`] when the framing rejects the image (magic,
    /// version, flags, lengths, or checksum), and the
    /// [`decode_body`](Self::decode_body) errors when the body itself is
    /// invalid. Framing checks run first, so body decoding only ever sees bytes
    /// whose checksum matched.
    pub fn deserialize(image: &[u8]) -> Result<Self, XorError> {
        let body = layout::unframe(XOR_FILTER_MAGIC, XOR_FILTER_FORMAT_VERSION, image)
            .map_err(XorError::Corrupt)?;
        Self::decode_body(body)
    }
}
