use crate::errors::{Error, ErrorKind};
use crate::utils::{le_u16, le_u32, le_u64};
use crate::{
    END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE, Zip64EndOfCentralDirectory,
    Zip64EndOfCentralDirectoryRecord, ZipFileHeaderFixed, ZipSliceArchive,
};
use core::num::NonZeroU64;

const END_OF_CENTRAL_DIR_SIGNATURE: u32 = 0x06054b50;
#[cfg(any(feature = "std", test))]
pub(crate) const END_OF_CENTRAL_DIR_SIGNATURE_BYTES: [u8; 4] =
    END_OF_CENTRAL_DIR_SIGNATURE.to_le_bytes();

#[cfg(feature = "std")]
mod reader;

// https://github.com/zlib-ng/minizip-ng/blob/55db144e03027b43263e5ebcb599bf0878ba58de/mz_zip.c#L78
const END_OF_CENTRAL_DIR_MAX_OFFSET: u64 = 1 << 20;

/// Locates the End of Central Directory (EOCD) record in a ZIP archive.
///
/// The `ZipLocator` is responsible for finding the EOCD record, which is
/// crucial for reading the contents of a ZIP file.
///
/// In the event, that the comment or tailing data contains the EOCD signature,
/// causing the zip locator to fail to parse. One can reparse the data starting
/// from the false EOCD offset using the reported offset
/// [`Error::eocd_offset()`]
#[derive(Debug)]
pub struct ZipLocator {
    max_search_space: u64,
}

impl Default for ZipLocator {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipLocator {
    /// Creates a new `ZipLocator` with a default maximum search space of 1 MiB
    pub fn new() -> Self {
        ZipLocator {
            max_search_space: END_OF_CENTRAL_DIR_MAX_OFFSET,
        }
    }

    /// Sets the maximum number of bytes to search for the EOCD signature.
    ///
    /// The search is performed backwards from the end of the data source.
    ///
    /// ```rust
    /// use rawzip::ZipLocator;
    ///
    /// let locator = ZipLocator::new().max_search_space(1024 * 64); // 64 KiB
    /// ```
    pub fn max_search_space(mut self, max_search_space: u64) -> Self {
        self.max_search_space = max_search_space;
        self
    }

    fn locate_in_byte_slice(&self, data: &[u8]) -> Result<EndOfCentralDirectory, Error> {
        let location = find_end_of_central_dir_signature(data, self.max_search_space as usize)
            .ok_or(ErrorKind::MissingEndOfCentralDirectory)?;

        let mut eocd = self
            .locate_in_byte_slice_impl(data, location)
            .map_err(|e| e.with_eocd_offset(location as u64))?;

        // Transparently verify that the self reported central directory points
        // to a valid entry. If it is not a valid entry, we can attempt to
        // correct offsets when there is undeclared prelude data by testing if
        // the central directory directly precedes the end of central directory
        // marker, which should hold true in the vast majority of cases. If both
        // checks fail, defer returning an error until the user explicitly wants
        // to iterate through the central directory.
        let first_entry = data
            .get(eocd.central_dir_offset as usize..)
            .filter(|d| ZipFileHeaderFixed::parse(d).is_ok());

        match first_entry {
            None if !eocd.is_zip64() => {
                let cd_offset = eocd.eocd_offset.saturating_sub(eocd.central_dir_size);

                let first_entry = data
                    .get(cd_offset as usize..)
                    .filter(|d| ZipFileHeaderFixed::parse(d).is_ok());

                if first_entry.is_some() {
                    eocd.base_offset = cd_offset.saturating_sub(eocd.central_dir_offset);
                    eocd.central_dir_offset = cd_offset;
                }

                Ok(eocd)
            }
            _ => Ok(eocd),
        }
    }

    fn locate_in_byte_slice_impl(
        &self,
        data: &[u8],
        location: usize,
    ) -> Result<EndOfCentralDirectory, Error> {
        let eocd = EndOfCentralDirectoryRecordFixed::parse(&data[location..])?;
        let has_zip64_sentinel = eocd.has_zip64_sentinel();
        let eocd = EndOfCentralDirectoryRecord::from_parts(location as u64, eocd);

        // Validate comment is completely present in the slice
        let comment_start = location + EndOfCentralDirectoryRecordFixed::SIZE;
        let comment_len = eocd.comment_len as usize;
        if comment_start + comment_len > data.len() {
            return Err(Error::from(ErrorKind::Eof));
        }

        if !has_zip64_sentinel {
            return EndOfCentralDirectory::create(eocd);
        }

        // A sentinel EOCD field only hints at zip64. We need to support classic
        // zips with 65535 entries.
        let zip64_locator = location
            .checked_sub(Zip64EndOfCentralDirectoryLocatorRecord::SIZE)
            .and_then(|start| Zip64EndOfCentralDirectoryLocatorRecord::parse(&data[start..]).ok());

        let Some(zip64_locator) = zip64_locator else {
            return EndOfCentralDirectory::create(eocd);
        };

        let zip64_eocd = &data[(zip64_locator.directory_offset as usize).min(data.len())..];
        let zip64_record = Zip64EndOfCentralDirectoryRecord::parse(zip64_eocd)?;

        let zip64 =
            Zip64EndOfCentralDirectory::from_parts(zip64_locator.directory_offset, zip64_record);
        EndOfCentralDirectory::create_zip64(eocd, zip64)
    }

    /// Locates the EOCD record within a byte slice.
    ///
    /// On success, returns a `ZipSliceArchive` which allows reading the archive
    /// directly from the slice. On failure, returns the original slice and an `Error`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use rawzip::ZipLocator;
    /// use std::fs;
    /// use std::io::Read;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut file = fs::File::open("assets/readme.zip")?;
    /// let mut data = Vec::new();
    /// file.read_to_end(&mut data)?;
    ///
    /// let locator = ZipLocator::new();
    /// match locator.locate_in_slice(&data) {
    ///     Ok(archive) => {
    ///         println!("Found EOCD in slice, archive has {} files.", archive.entries_hint());
    ///     }
    ///     Err((_data, e)) => {
    ///         eprintln!("Failed to locate EOCD in slice: {:?}", e);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn locate_in_slice<T: AsRef<[u8]>>(
        &self,
        data: T,
    ) -> Result<ZipSliceArchive<T>, (T, Error)> {
        match self.locate_in_byte_slice(data.as_ref()) {
            Ok(eocd) => Ok(ZipSliceArchive::new(data, eocd)),
            Err(e) => Err((data, e)),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EndOfCentralDirectory {
    eocd_offset: u64,
    zip64_eocd_offset: Option<NonZeroU64>,
    central_dir_size: u64,
    central_dir_offset: u64,
    num_entries: u64,
    comment_len: u16,
    base_offset: u64,
}

impl EndOfCentralDirectory {
    pub(crate) fn create(eocd: EndOfCentralDirectoryRecord) -> Result<Self, Error> {
        let result = EndOfCentralDirectory {
            eocd_offset: eocd.offset,
            zip64_eocd_offset: None,
            central_dir_size: u64::from(eocd.central_dir_size),
            central_dir_offset: u64::from(eocd.central_dir_offset),
            num_entries: u64::from(eocd.num_entries),
            comment_len: eocd.comment_len,
            base_offset: 0,
        };

        result.validate()?;
        Ok(result)
    }

    pub(crate) fn create_zip64(
        eocd: EndOfCentralDirectoryRecord,
        zip64: Zip64EndOfCentralDirectory,
    ) -> Result<Self, Error> {
        let result = EndOfCentralDirectory {
            eocd_offset: eocd.offset,
            zip64_eocd_offset: NonZeroU64::new(zip64.offset),
            central_dir_size: zip64.central_dir_size,
            central_dir_offset: zip64.central_dir_offset,
            num_entries: zip64.num_entries,
            comment_len: eocd.comment_len,
            base_offset: 0,
        };

        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), Error> {
        // It doesn't make sense if the start of the central directory is after
        // the end.
        if self.directory_offset() > self.head_eocd_offset() {
            return Err(Error::from(ErrorKind::InvalidEndOfCentralDirectory));
        }

        Ok(())
    }

    #[inline]
    pub(crate) fn is_zip64(&self) -> bool {
        self.zip64_eocd_offset.is_some()
    }

    pub(crate) fn base_offset(&self) -> u64 {
        self.base_offset
    }

    /// The first end of the central directory signature offsets.
    ///
    /// This is offset where no new central directory records are expected.
    ///
    /// Will be equivalent to [`Self::tail_eocd_offset`] eocd for non-zip64 files
    #[inline]
    pub(crate) fn head_eocd_offset(&self) -> u64 {
        self.zip64_eocd_offset
            .map(core::num::NonZero::get)
            .unwrap_or(self.eocd_offset)
    }

    /// The last end of the central directory signature offsets.
    ///
    /// This will always be the byte offset of 0x06054b50
    #[inline]
    pub(crate) fn tail_eocd_offset(&self) -> u64 {
        self.eocd_offset
    }

    /// offset of the start of the central directory
    #[inline]
    pub(crate) fn directory_offset(&self) -> u64 {
        self.central_dir_offset
    }

    #[inline]
    pub(crate) fn entries(&self) -> u64 {
        self.num_entries
    }

    #[inline]
    pub(crate) fn comment_len(&self) -> usize {
        self.comment_len as usize
    }
}

/// A non-zip64 end of central directory
#[derive(Debug, Clone)]
pub(crate) struct EndOfCentralDirectoryRecord {
    pub(crate) offset: u64,
    pub(crate) central_dir_size: u32,
    pub(crate) central_dir_offset: u32,
    pub(crate) num_entries: u16,
    pub(crate) comment_len: u16,
}

impl EndOfCentralDirectoryRecord {
    #[inline]
    pub fn from_parts(offset: u64, eocd: EndOfCentralDirectoryRecordFixed) -> Self {
        Self {
            offset,
            central_dir_size: eocd.central_dir_size,
            central_dir_offset: eocd.central_dir_offset,
            num_entries: eocd.total_entries,
            comment_len: eocd.comment_len,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EndOfCentralDirectoryRecordFixed {
    pub(crate) signature: u32,
    #[allow(dead_code)]
    pub(crate) disk_number: u16,
    #[allow(dead_code)]
    pub(crate) eocd_disk: u16,
    pub(crate) num_entries: u16,
    pub(crate) total_entries: u16,
    pub(crate) central_dir_size: u32,
    pub(crate) central_dir_offset: u32,
    pub(crate) comment_len: u16,
}

impl EndOfCentralDirectoryRecordFixed {
    pub(crate) const SIZE: usize = 22;
    pub fn parse(data: &[u8]) -> Result<EndOfCentralDirectoryRecordFixed, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let result = EndOfCentralDirectoryRecordFixed {
            signature: le_u32(&data[0..4]),
            disk_number: le_u16(&data[4..6]),
            eocd_disk: le_u16(&data[6..8]),
            num_entries: le_u16(&data[8..10]),
            total_entries: le_u16(&data[10..12]),
            central_dir_size: le_u32(&data[12..16]),
            central_dir_offset: le_u32(&data[16..20]),
            comment_len: le_u16(&data[20..22]),
        };

        if result.signature != END_OF_CENTRAL_DIR_SIGNATURE {
            return Err(Error::from(ErrorKind::InvalidSignature {
                expected: END_OF_CENTRAL_DIR_SIGNATURE,
                actual: result.signature,
            }));
        }

        Ok(result)
    }

    /// If true, a zip64 record *may* be present.
    pub fn has_zip64_sentinel(&self) -> bool {
        self.num_entries == u16::MAX        // 4.4.21
            || self.total_entries == u16::MAX   // 4.4.22
            || self.central_dir_size == u32::MAX // 4.4.23
            || self.central_dir_offset == u32::MAX // 4.4.24
    }
}

///
///
/// 4.3.15
#[derive(Debug)]
#[allow(dead_code)]
struct Zip64EndOfCentralDirectoryLocatorRecord {
    /// zip64 end of central dir locator signature
    pub signature: u32,

    /// number of the disk with the start of the zip64 end of central directory
    pub eocd_disk: u32,

    /// relative offset of the zip64 end of central directory record
    pub directory_offset: u64,

    /// total number of disks
    pub total_disks: u32,
}

impl Zip64EndOfCentralDirectoryLocatorRecord {
    const SIZE: usize = 20;

    pub fn parse(data: &[u8]) -> Result<Zip64EndOfCentralDirectoryLocatorRecord, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let result = Zip64EndOfCentralDirectoryLocatorRecord {
            signature: le_u32(&data[0..4]),
            eocd_disk: le_u32(&data[4..8]),
            directory_offset: le_u64(&data[8..16]),
            total_disks: le_u32(&data[16..20]),
        };

        if result.signature != END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE {
            return Err(Error::from(ErrorKind::InvalidSignature {
                expected: END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE,
                actual: result.signature,
            }));
        }

        Ok(result)
    }
}

pub(crate) fn find_end_of_central_dir_signature(
    data: &[u8],
    max_search_space: usize,
) -> Option<usize> {
    let start_search = data.len().saturating_sub(max_search_space);
    rfind::<END_OF_CENTRAL_DIR_SIGNATURE>(&data[start_search..]).map(|pos| pos + start_search)
}

/// Finds the last occurrence of the 4-byte little-endian `NEEDLE` in `haystack`.
///
/// Some benchmarks:
///
/// - windows().rposition(): 2 GiB/s (complete data independence as it's just loop of u32 loads)
/// - `memmem::rfind`: 8.4 GiB/s (586 MiB/s worst case)
/// - This implementation: 16.0 GiB/s (4.63 GiB/s worst case)
fn rfind<const NEEDLE: u32>(haystack: &[u8]) -> Option<usize> {
    const N: usize = core::mem::size_of::<u32>();
    if haystack.len() < N {
        return None;
    }

    // The fast path scans backwards for the needle's last byte as a candidate
    let search = &haystack[N - 1..];

    // SWAR lanes
    const LANES: usize = core::mem::size_of::<u64>();
    let lo = u64::from_ne_bytes([0x01; LANES]);
    let lo7 = u64::from_ne_bytes([0x7F; LANES]);
    let hi = u64::from_ne_bytes([0x80; LANES]);

    let haszero = |word: u64| word.wrapping_sub(lo) & !word & hi;
    let iszero = |word: u64| !(((word & lo7) + lo7) | word) & hi;
    let bytes = NEEDLE.to_le_bytes();
    let bc = bytes.map(|x| u64::from_ne_bytes([x; LANES]));

    let load = |at: usize| -> u64 {
        let c = &haystack[at..at + LANES];
        u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
    };

    let mut chunks = search.rchunks_exact(LANES);
    let mut start = search.len();
    for c in chunks.by_ref() {
        start -= LANES;
        let word = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        let cand = haszero(word ^ bc[N - 1]);

        // Common case: no candidate last byte, or no aligned first byte
        if cand == 0 || cand & haszero(load(start) ^ bc[0]) == 0 {
            continue;
        }

        // Survivors should be rare but let's check the middle bytes
        if haszero(load(start + 1) ^ bc[1]) & haszero(load(start + 2) ^ bc[2]) == 0 {
            continue;
        }

        // Confirm all lanes at once with the four exact byte planes.
        let matches = iszero(word ^ bc[N - 1])
            & iszero(load(start) ^ bc[0])
            & iszero(load(start + 1) ^ bc[1])
            & iszero(load(start + 2) ^ bc[2]);

        if matches != 0 {
            // highest set bit is the last match
            return Some(start + matches.ilog2() as usize / 8);
        }
    }

    haystack[..chunks.remainder().len() + N - 1]
        .windows(N)
        .rposition(|window| le_u32(window) == NEEDLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;
    use rstest::rstest;

    #[rstest]
    #[case(&[], None)]
    #[case(&[0x50, 0x4b, 0x05, 0x06], Some(0))]
    #[case(&[0x50, 0x4b, 0x05, 0x06, 0x50, 0x4b, 0x05, 0x06], Some(4))]
    #[case(&[0x50, 0x51, 0x4b, 0x05, 0x06, 0xff, 0xff, 0x07, 0x01, 0x50, 0x00], None)]
    fn test_rfind(#[case] input: &[u8], #[case] expected: Option<usize>) {
        assert_eq!(rfind::<END_OF_CENTRAL_DIR_SIGNATURE>(input), expected);
    }

    #[quickcheck]
    fn test_rfind_matches_windows_rposition(data: Vec<u8>) {
        let expected = data
            .windows(END_OF_CENTRAL_DIR_SIGNATURE_BYTES.len())
            .rposition(|window| window == END_OF_CENTRAL_DIR_SIGNATURE_BYTES);

        assert_eq!(rfind::<END_OF_CENTRAL_DIR_SIGNATURE>(&data), expected);
    }
}
