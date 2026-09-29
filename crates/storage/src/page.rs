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
//! Fixed-size, checksum-protected pages (16 KiB).

use crate::checksum::{compute_checksum, crc_finalize, crc_init, crc_update};
use crate::physical::{PageHeader, PageType, PAGE_MAGIC, PAGE_SIZE as PHYSICAL_PAGE_SIZE};
use plomid_core::{ErrorKind, GenerationId, Lsn, PageId, PlomidError, Result};

/// On-disk page constants are defined once in `plomid_core::constants` and
/// re-exported here for the page module's callers.
pub use plomid_core::{
    PAGE_DATA_SIZE, PAGE_HEADER_SIZE, PAGE_SIZE_BYTES as PAGE_SIZE, PAGE_TRAILER_SIZE,
};

const DATA_OFFSET: usize = PAGE_HEADER_SIZE;
const TRAILER_OFFSET: usize = PAGE_SIZE - PAGE_TRAILER_SIZE;

/// Compile-time assertion that both page-size definitions agree.
const _: () = {
    assert!(PAGE_SIZE as u64 == PHYSICAL_PAGE_SIZE);
    assert!(PAGE_DATA_SIZE == PAGE_SIZE - PAGE_HEADER_SIZE - PAGE_TRAILER_SIZE);
};

/// An in-memory fixed-size page.
///
/// Layout: 48-byte header, payload, 4-byte trailing CRC32C. The header
/// carries logical identity plus its own header-only checksum (compatible
/// with `PageHeader::decode`); the trailer covers the full page with both
/// checksum fields zeroed so payload corruption is also detected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page {
    bytes: [u8; PAGE_SIZE],
}

impl Page {
    /// Creates a zero-filled page with the supplied ID.
    #[must_use]
    pub fn new(page_id: PageId) -> Self {
        Self::with_type(page_id, PageType::Leaf)
    }

    /// Creates a zero-filled page with an explicit type.
    #[must_use]
    pub fn with_type(page_id: PageId, page_type: PageType) -> Self {
        let header = PageHeader::new(page_id, page_type).with_checksum();
        let mut page = Self {
            bytes: [0; PAGE_SIZE],
        };
        page.bytes[..PAGE_HEADER_SIZE].copy_from_slice(&header.encode());
        page.refresh_trailer();
        page
    }

    /// Decodes and verifies one complete on-disk page.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_borrowed(bytes).map(|view| {
            let mut out = [0_u8; PAGE_SIZE];
            out.copy_from_slice(view.as_bytes());
            Self { bytes: out }
        })
    }

    /// Verifies one complete on-disk page without copying and returns a
    /// borrowed view. The hot read paths use this directly so a validated
    /// 16 KiB page does not pay a second full-page copy after checksum
    /// verification; callers that need ownership copy once from the view.
    pub fn from_borrowed(bytes: &[u8]) -> Result<ValidatedPage<'_>> {
        if bytes.len() != PAGE_SIZE {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "page buffer has invalid length",
            ));
        }
        // Header-only checksum first (magic/version/type validated inside).
        let _ = PageHeader::decode(&bytes[..PAGE_HEADER_SIZE])?;
        let stored =
            u32::from_le_bytes(bytes[TRAILER_OFFSET..PAGE_SIZE].try_into().map_err(|_| {
                PlomidError::new(ErrorKind::Corruption, "invalid page trailer field")
            })?);
        // Whole-page trailer checksum over the in-hand buffer without copying.
        let actual = page_checksum(bytes);
        if actual != stored {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                format!("checksum mismatch (expected {stored:#010x}, got {actual:#010x})"),
            ));
        }
        Ok(ValidatedPage { bytes })
    }

    /// Returns this page's logical ID (never a file offset).
    #[must_use]
    pub fn id(&self) -> PageId {
        self.header().page_id
    }

    /// Returns decoded header fields without verifying checksums.
    #[must_use]
    pub fn header(&self) -> PageHeader {
        parse_header(&self.bytes)
            .unwrap_or_else(|_| PageHeader::new(PageId::new(0), PageType::Free))
    }

    /// Returns the page type.
    #[must_use]
    pub fn page_type(&self) -> PageType {
        self.header().page_type
    }

    /// Returns the generation counter.
    #[must_use]
    pub fn generation(&self) -> GenerationId {
        self.header().generation_id
    }

    /// Returns the last-write LSN.
    #[must_use]
    pub fn lsn(&self) -> Lsn {
        self.header().lsn
    }

    /// Sets the page type, preserving other header fields.
    pub fn set_page_type(&mut self, page_type: PageType) {
        let mut header = self.header();
        header.page_type = page_type;
        self.store_header(header);
    }

    /// Sets the generation counter.
    pub fn set_generation(&mut self, generation: GenerationId) {
        let mut header = self.header();
        header.generation_id = generation;
        self.store_header(header);
    }

    /// Sets the last-write LSN.
    pub fn set_lsn(&mut self, lsn: Lsn) {
        let mut header = self.header();
        header.lsn = lsn;
        self.store_header(header);
    }

    /// Validates header invariants plus header and whole-page checksums.
    pub fn validate(&self) -> Result<()> {
        let header = PageHeader::decode(&self.bytes[..PAGE_HEADER_SIZE])?;
        if header.page_id.is_zero() && header.page_type != PageType::Free {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "non-free page carries zero page ID",
            ));
        }
        if header.payload_length as usize > PAGE_DATA_SIZE {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                "invalid page payload length",
            ));
        }
        self.verify_trailer()?;
        Ok(())
    }

    /// Returns the mutable payload area, excluding header and trailer.
    ///
    /// Mutating the payload invalidates the trailer until re-encoded.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[DATA_OFFSET..TRAILER_OFFSET]
    }

    /// Returns the payload area, excluding header and trailer.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.bytes[DATA_OFFSET..TRAILER_OFFSET]
    }

    /// Encodes the page and refreshes header + trailer checksums.
    ///
    /// Hot write paths should prefer the `&mut` form (a mutable page plus
    /// [`Page::refresh_for_write`]) or encoding into a caller-owned scratch
    /// buffer; this clone exists for callers that only hold `&Page`.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PAGE_SIZE] {
        let mut page = self.clone();
        page.refresh_for_write();
        page.bytes
    }

    /// Encodes the page into `out` without cloning the 16 KiB image.
    ///
    /// Batch writers serialize many pages into one contiguous write buffer;
    /// this refreshes header + trailer checksums in place on `self` (via an
    /// interior copy of the verified header only) and copies the result into
    /// `out`, so a 16-page block write pays sixteen 16 KiB copies into the
    /// staging buffer instead of sixteen clones plus sixteen copies.
    pub fn copy_encoded_into(&mut self, out: &mut [u8; PAGE_SIZE]) {
        out.copy_from_slice(self.refresh_for_write());
    }

    /// Refreshes header + trailer checksums in place and borrows the complete
    /// on-disk image. Persist paths use this instead of [`Page::to_bytes`] so
    /// a durable write does not pay a 16 KiB clone only to recompute the same
    /// checksums and copy the result into a write buffer.
    pub fn refresh_for_write(&mut self) -> &[u8; PAGE_SIZE] {
        let header = self.header().with_checksum();
        self.bytes[..PAGE_HEADER_SIZE].copy_from_slice(&header.encode());
        self.refresh_trailer();
        &self.bytes
    }

    fn store_header(&mut self, mut header: PageHeader) {
        header.checksum = 0;
        let mut hb = header.encode();
        hb[PageHeader::CHECKSUM_OFFSET..PageHeader::CHECKSUM_OFFSET + 4].fill(0);
        let cs = compute_checksum(&hb);
        header.checksum = cs;
        self.bytes[..PAGE_HEADER_SIZE].copy_from_slice(&header.encode());
        self.refresh_trailer();
    }

    fn refresh_trailer(&mut self) {
        let trailer = page_checksum(&self.bytes);
        self.bytes[TRAILER_OFFSET..PAGE_SIZE].copy_from_slice(&trailer.to_le_bytes());
    }

    fn verify_trailer(&self) -> Result<()> {
        let stored = u32::from_le_bytes(
            self.bytes[TRAILER_OFFSET..PAGE_SIZE]
                .try_into()
                .map_err(|_| {
                    PlomidError::new(ErrorKind::Corruption, "invalid page trailer field")
                })?,
        );
        let actual = page_checksum(&self.bytes);
        if actual != stored {
            return Err(PlomidError::new(
                ErrorKind::Corruption,
                format!("checksum mismatch (expected {stored:#010x}, got {actual:#010x})"),
            ));
        }
        Ok(())
    }

    /// Borrows the complete on-disk image (header, payload, trailer).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.bytes
    }

    /// Copies the complete on-disk image into `out` without an intermediate
    /// owned allocation. Callers serializing pages into larger write buffers
    /// use this so `to_bytes` (32 KiB transient) is not constructed only to be
    /// copied and dropped.
    pub fn copy_bytes_into(&self, out: &mut [u8; PAGE_SIZE]) {
        out.copy_from_slice(&self.bytes);
    }
}

/// A verified borrowed view of one complete on-disk page.
///
/// Returned by [`Page::from_borrowed`]. The view borrows the caller's read
/// buffer, so validating a page already resident in a reused batch buffer
/// performs no allocation and no 16 KiB copy; only callers that need an owned
/// [`Page`] perform the single copy via [`ValidatedPage::to_owned_page`].
#[derive(Clone, Copy, Debug)]
pub struct ValidatedPage<'a> {
    bytes: &'a [u8],
}

impl<'a> ValidatedPage<'a> {
    /// Borrows the complete on-disk image.
    #[must_use]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Copies the verified image into an owned [`Page`].
    pub fn to_owned_page(&self) -> Result<Page> {
        Page::from_bytes(self.bytes)
    }

    /// Copies the verified image into `out` without an owned [`Page`].
    pub fn copy_into(&self, out: &mut [u8; PAGE_SIZE]) {
        out.copy_from_slice(self.bytes);
    }

    /// Returns the logical page ID without decoding the full header.
    #[must_use]
    pub fn id(&self) -> PageId {
        PageId::from_le_bytes(self.bytes[12..20].try_into().unwrap_or([0_u8; 8]))
    }
}

/// Whole-page CRC over `bytes` treating the 4-byte header checksum field and
/// the 4-byte trailer as zero, matching the on-disk trailer semantics without
/// copying the page. `bytes` must be a full [`PAGE_SIZE`] buffer.
fn page_checksum(bytes: &[u8]) -> u32 {
    let header_end = PageHeader::CHECKSUM_OFFSET;
    let mut crc = crc_init();
    crc = crc_update(crc, &bytes[..header_end]);
    crc = crc_update(crc, &[0_u8; 4]);
    crc = crc_update(crc, &bytes[header_end + 4..TRAILER_OFFSET]);
    crc = crc_update(crc, &[0_u8; 4]);
    crc_finalize(crc)
}

/// Parses header fields without header-only checksum verification.
///
/// The stored checksum covers the whole page, so it is verified separately.
fn parse_header(bytes: &[u8; PAGE_SIZE]) -> Result<PageHeader> {
    if bytes[..4] != PAGE_MAGIC {
        return Err(PlomidError::new(
            ErrorKind::Corruption,
            "invalid page magic",
        ));
    }
    let version = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid page version field"))?,
    );
    if version != crate::physical::PAGE_FORMAT_VERSION {
        return Err(PlomidError::new(
            ErrorKind::Corruption,
            format!("unsupported page format version {version}"),
        ));
    }
    let page_type = PageType::from_byte(bytes[8])?;
    if bytes[9..12] != [0, 0, 0] {
        return Err(PlomidError::new(
            ErrorKind::Corruption,
            "page header non-zero reserved bytes",
        ));
    }
    let page_id = PageId::from_le_bytes(
        bytes[12..20]
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid page ID field"))?,
    );
    let generation_id = GenerationId::from_le_bytes(
        bytes[20..28]
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid generation field"))?,
    );
    let lsn = Lsn::from_le_bytes(
        bytes[28..36]
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid LSN field"))?,
    );
    let payload_length =
        u32::from_le_bytes(bytes[36..40].try_into().map_err(|_| {
            PlomidError::new(ErrorKind::Corruption, "invalid payload length field")
        })?);
    if payload_length as usize > PAGE_DATA_SIZE {
        return Err(PlomidError::new(
            ErrorKind::Corruption,
            "invalid page payload length",
        ));
    }
    let flags = u32::from_le_bytes(
        bytes[40..44]
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid page flags field"))?,
    );
    let checksum = u32::from_le_bytes(
        bytes[44..48]
            .try_into()
            .map_err(|_| PlomidError::new(ErrorKind::Corruption, "invalid page checksum field"))?,
    );
    Ok(PageHeader {
        magic: PAGE_MAGIC,
        format_version: version,
        page_type,
        page_id,
        generation_id,
        lsn,
        payload_length,
        flags,
        checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_matches_physical_contract() {
        assert_eq!(PAGE_SIZE, 16 * 1024);
        assert_eq!(PAGE_HEADER_SIZE, PageHeader::SIZE);
        assert_eq!(PAGE_TRAILER_SIZE, 4);
        assert_eq!(
            PAGE_DATA_SIZE,
            PAGE_SIZE - PAGE_HEADER_SIZE - PAGE_TRAILER_SIZE
        );
    }

    #[test]
    fn round_trip_preserves_identity_payload_and_generation() {
        let mut page = Page::with_type(PageId::new(7), PageType::Leaf);
        page.set_generation(GenerationId::new(3));
        page.set_lsn(Lsn::new(9));
        page.data_mut()[..5].copy_from_slice(b"hello");
        let bytes = page.to_bytes();
        let decoded = Page::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded.id(), PageId::new(7));
        assert_eq!(decoded.page_type(), PageType::Leaf);
        assert_eq!(decoded.generation(), GenerationId::new(3));
        assert_eq!(decoded.lsn(), Lsn::new(9));
        assert_eq!(&decoded.data()[..5], b"hello");
        decoded.validate().expect("validate");
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut bytes = Page::new(PageId::new(1)).to_bytes();
        bytes[0..4].copy_from_slice(b"XXXX");
        assert!(matches!(Page::from_bytes(&bytes), Err(PlomidError { .. })));
    }

    #[test]
    fn rejects_checksum_mismatch() {
        let mut bytes = Page::new(PageId::new(2)).to_bytes();
        // Flip a payload byte; the trailer checksum no longer matches.
        bytes[100] ^= 0xFF;
        assert!(Page::from_bytes(&bytes).is_err());
    }

    #[test]
    fn rejects_oversized_payload_length() {
        // Craft a valid header with an implausibly large payload length.
        let mut bytes = Page::new(PageId::new(3)).to_bytes();
        let oversized = (PAGE_DATA_SIZE as u32) + 1;
        bytes[36..40].copy_from_slice(&oversized.to_le_bytes());
        assert!(Page::from_bytes(&bytes).is_err());
    }

    #[test]
    fn rejects_unsupported_format_version() {
        let mut bytes = Page::new(PageId::new(4)).to_bytes();
        bytes[4..8].copy_from_slice(&999u32.to_le_bytes());
        assert!(Page::from_bytes(&bytes).is_err());
    }
}
