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
//! Compression support for columnar chunks.
//!
//! This module is the byte-oriented codec layer only. Value encodings,
//! selection, and the self-describing chunk frame live in [`crate::encoding`],
//! which drives this facade. Both layers share the same codec tags, so a chunk
//! header, a frame, and this module can never disagree about what a payload is.

use crate::chunk::ChunkCompression;
#[cfg(feature = "zstd")]
use crate::layout::corruption;
use plomid_core::{ErrorKind, PlomidError, Result};

/// Default compression to use for columnar data.
pub fn default_compression() -> ChunkCompression {
    ChunkCompression::None
}

/// Compresses data using the specified compression method.
pub fn compress(data: &[u8], compression: ChunkCompression) -> Result<Vec<u8>> {
    match compression {
        ChunkCompression::None => Ok(data.to_vec()),
        ChunkCompression::Zstd => {
            #[cfg(feature = "zstd")]
            {
                Ok(zstd::encode_all(data, 3)?)
            }
            #[cfg(not(feature = "zstd"))]
            {
                Err(PlomidError::new(
                    ErrorKind::Unsupported,
                    "zstd not available",
                ))
            }
        }
    }
}

/// Decompresses data using the specified compression method.
///
/// `expected_size` is the length the caller's framing declares. It is passed to
/// the codec as a hard output bound, so a payload that claims to expand beyond
/// it is rejected by the codec instead of being materialized. The declared
/// length must therefore be validated by the caller before it reaches here; the
/// columnar read path checks it against the format's per-chunk limit.
pub fn decompress(
    data: &[u8],
    expected_size: u64,
    compression: ChunkCompression,
) -> Result<Vec<u8>> {
    match compression {
        ChunkCompression::None => Ok(data.to_vec()),
        ChunkCompression::Zstd => {
            #[cfg(feature = "zstd")]
            {
                let capacity = usize::try_from(expected_size)
                    .map_err(|_| corruption("declared chunk size does not fit in memory"))?;
                // `bulk::decompress` writes at most `capacity` bytes and fails
                // rather than growing, so the declared length bounds the
                // allocation even for a hostile frame.
                Ok(zstd::bulk::decompress(data, capacity)?)
            }
            #[cfg(not(feature = "zstd"))]
            {
                let _ = expected_size;
                Err(PlomidError::new(
                    ErrorKind::Unsupported,
                    "zstd not available",
                ))
            }
        }
    }
}

/// Returns true if compression is available for the given method.
pub fn is_compression_available(compression: ChunkCompression) -> bool {
    match compression {
        ChunkCompression::None => true,
        #[cfg(feature = "zstd")]
        ChunkCompression::Zstd => true,
        #[cfg(not(feature = "zstd"))]
        ChunkCompression::Zstd => false,
    }
}

/// Estimates compressed size for planning chunk layout.
pub fn estimate_compressed_size(uncompressed_size: usize) -> usize {
    (uncompressed_size as f64 * 0.7) as usize
}
