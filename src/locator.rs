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
        let max_search_space = self.max_search_space.min(data.len() as u64) as usize;
        let (before, record) = split_at_end_of_central_dir_signature(data, max_search_space)
            .ok_or(ErrorKind::MissingEndOfCentralDirectory)?;

        let location = before.len();
        let mut eocd = self
            .locate_in_byte_slice_impl(data, before, record)
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

    /// Parses the end of central directory record that starts `record`, where
    /// `before` is the data before it
    fn locate_in_byte_slice_impl(
        &self,
        data: &[u8],
        before: &[u8],
        record: &[u8],
    ) -> Result<EndOfCentralDirectory, Error> {
        let location = before.len();
        let eocd = EndOfCentralDirectoryRecordFixed::parse(record)?;

        // Validate comment is completely present in the slice
        let required = EndOfCentralDirectoryRecordFixed::SIZE + usize::from(eocd.comment_len);
        if record.len() < required {
            return Err(Error::from(ErrorKind::Eof));
        }

        // Always probe for a zip64 EOCD locator, which can be present without sentinels
        let zip64_locator = before
            .last_chunk::<{ Zip64EndOfCentralDirectoryLocatorRecord::SIZE }>()
            .and_then(|locator| Zip64EndOfCentralDirectoryLocatorRecord::parse(locator).ok());

        let Some(zip64_locator) = zip64_locator else {
            return EndOfCentralDirectory::create(EndOfCentralDirectoryRecord::from_parts(
                location as u64,
                &eocd,
            ));
        };

        let zip64_eocd = &data[zip64_locator.directory_offset.min(data.len() as u64) as usize..];
        let zip64_record = match parse_zip64_candidate(
            zip64_eocd,
            &eocd,
            &zip64_locator,
            location as u64 - Zip64EndOfCentralDirectoryLocatorRecord::SIZE as u64,
        ) {
            Some(record) => record,
            None => {
                return EndOfCentralDirectory::create(EndOfCentralDirectoryRecord::from_parts(
                    location as u64,
                    &eocd,
                ));
            }
        };

        let zip64 =
            Zip64EndOfCentralDirectory::from_parts(zip64_locator.directory_offset, zip64_record);
        let eocd = EndOfCentralDirectoryRecord::from_parts(location as u64, &eocd);
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
    pub fn from_parts(offset: u64, eocd: &EndOfCentralDirectoryRecordFixed) -> Self {
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
    #[allow(dead_code)]
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

fn parse_zip64_candidate(
    data: &[u8],
    classic: &EndOfCentralDirectoryRecordFixed,
    locator: &Zip64EndOfCentralDirectoryLocatorRecord,
    locator_offset: u64,
) -> Option<Zip64EndOfCentralDirectoryRecord> {
    const ZIP64_SIZE_PREFIX: u64 = 12;
    const ZIP64_MIN_SIZE: u64 = Zip64EndOfCentralDirectoryRecord::SIZE as u64 - ZIP64_SIZE_PREFIX;

    let zip64 = Zip64EndOfCentralDirectoryRecord::parse(data).ok()?;

    if zip64.size < ZIP64_MIN_SIZE {
        return None;
    }

    let record_end = locator
        .directory_offset
        .checked_add(ZIP64_SIZE_PREFIX)?
        .checked_add(zip64.size)?;

    let consistent = record_end == locator_offset
        && (classic.num_entries == u16::MAX || u64::from(classic.num_entries) == zip64.num_entries)
        && (classic.total_entries == u16::MAX
            || u64::from(classic.total_entries) == zip64.total_entries)
        && (classic.central_dir_size == u32::MAX
            || u64::from(classic.central_dir_size) == zip64.central_dir_size)
        && (classic.central_dir_offset == u32::MAX
            || u64::from(classic.central_dir_offset) == zip64.central_dir_offset);

    consistent.then_some(zip64)
}

/// Splits `data` at the last end of central directory signature that starts
/// within its last `max_search_space` bytes
pub(crate) fn split_at_end_of_central_dir_signature(
    data: &[u8],
    max_search_space: usize,
) -> Option<(&[u8], &[u8])> {
    let (_, window) = data.split_at(data.len().saturating_sub(max_search_space));
    let pos = rfind::<END_OF_CENTRAL_DIR_SIGNATURE>(window)?;

    // Measuring the split from the end of `data` keeps it, and every slice of
    // the record after it, in bounds by construction. `pos` is within
    // `window`, so this never saturates.
    Some(data.split_at(data.len().saturating_sub(window.len() - pos)))
}

/// Finds the last end of central directory signature in `haystack`, for the
/// reader locator.
///
/// This search is not inlined. When it was, the generic reader locator became
/// larger, and the optimizer then inlined less of the reader's central
/// directory iteration. The slice locator inlines [`rfind`], as a call costs
/// more than the search of a short slice.
#[cfg(feature = "std")]
#[inline(never)]
pub(crate) fn rfind_end_of_central_dir_signature(haystack: &[u8]) -> Option<usize> {
    rfind::<END_OF_CENTRAL_DIR_SIGNATURE>(haystack)
}

/// Finds the last occurrence of the 4-byte little-endian `NEEDLE` in `haystack`.
///
/// Some benchmarks:
///
/// - windows().rposition(): 2 GiB/s (complete data independence as it's just loop of u32 loads)
/// - `memmem::rfind`: 8.4 GiB/s (586 MiB/s worst case)
/// - This implementation: 16.0 GiB/s (4.63 GiB/s worst case)
///
/// Each load is from an array that is split from the end of the haystack, at
/// an offset within the array, so the search has no panic paths.
#[inline(always)]
fn rfind<const NEEDLE: u32>(haystack: &[u8]) -> Option<usize> {
    const N: usize = core::mem::size_of::<u32>();

    // SWAR lanes
    const LANES: usize = core::mem::size_of::<u64>();
    const LO: u64 = u64::from_ne_bytes([0x01; LANES]);
    const LO7: u64 = u64::from_ne_bytes([0x7F; LANES]);
    const HI: u64 = u64::from_ne_bytes([0x80; LANES]);

    #[inline(always)]
    fn haszero(word: u64) -> u64 {
        word.wrapping_sub(LO) & !word & HI
    }

    #[inline(always)]
    fn iszero(word: u64) -> u64 {
        !(((word & LO7) + LO7) | word) & HI
    }

    // Loads the word at `at` as one load. Byte-wise loads let the optimizer
    // share the bytes that overlapping words have in common, and it then
    // assembles each word a byte at a time.
    #[inline(always)]
    fn word_at<const L: usize>(tail: &[u8; L], at: usize) -> u64 {
        tail.get(at..)
            .and_then(<[u8]>::first_chunk::<LANES>)
            .map_or(0, |word| u64::from_le_bytes(*word))
    }

    // Returns the end of the last needle that ends in the word at `at`. The
    // needle's other bytes are loaded from the word before, so `at` is at
    // least a word into `tail`, and each load is at a fixed offset from it.
    #[inline(always)]
    fn check<const L: usize>(tail: &[u8; L], at: usize, bc: &[u64; N]) -> Option<usize> {
        let cur = word_at(tail, at);
        let cand = haszero(cur ^ bc[N - 1]);

        // Common case: no candidate last byte, or no aligned first byte
        let w0 = word_at(tail, at - (N - 1));
        if cand == 0 || cand & haszero(w0 ^ bc[0]) == 0 {
            return None;
        }

        // Survivors should be rare but let's check the middle bytes
        let w1 = word_at(tail, at - (N - 2));
        let w2 = word_at(tail, at - (N - 3));
        if haszero(w1 ^ bc[1]) & haszero(w2 ^ bc[2]) == 0 {
            return None;
        }

        // Confirm all lanes at once with the four exact byte planes.
        let matches =
            iszero(cur ^ bc[N - 1]) & iszero(w0 ^ bc[0]) & iszero(w1 ^ bc[1]) & iszero(w2 ^ bc[2]);

        // highest set bit is the last match
        (matches != 0).then(|| at + matches.ilog2() as usize / 8 + 1)
    }

    // The fast path scans backwards a block at a time. `tail` is a block and
    // the word before it, split from the end of the data left to search.
    const BLOCK: usize = 8 * LANES;
    const TAIL: usize = BLOCK + LANES;
    const PAIR: usize = 2 * LANES;

    // Returns the end of the last needle that ends in the block
    #[inline(always)]
    fn scan(tail: &[u8; TAIL], bc: &[u64; N]) -> Option<usize> {
        (0..BLOCK / LANES)
            .rev()
            .find_map(|i| check(tail, LANES + i * LANES, bc))
    }

    // Returns if a needle can end in the block, from the needle's last byte,
    // then from its last two bytes, which are rarer together
    #[inline(always)]
    fn candidates(tail: &[u8; TAIL], bc: &[u64; N]) -> bool {
        let mut last = 0;
        for at in (LANES..TAIL).step_by(LANES) {
            last |= haszero(word_at(tail, at) ^ bc[N - 1]);
        }
        if last == 0 {
            return false;
        }

        let mut pair = 0;
        for at in (LANES..TAIL).step_by(LANES) {
            pair |=
                haszero(word_at(tail, at) ^ bc[N - 1]) & haszero(word_at(tail, at - 1) ^ bc[N - 2]);
        }
        pair != 0
    }

    // Searches a word at a time, then the last bytes one at a time
    #[inline(always)]
    fn words(mut head: &[u8], bc: &[u64; N], needle: u32) -> Option<usize> {
        while let Some((_, tail)) = head.split_last_chunk::<PAIR>() {
            if let Some(end) = check(tail, LANES, bc) {
                return Some(head.len() + end - PAIR - N);
            }
            (head, _) = head.split_at(head.len().saturating_sub(LANES));
        }

        head.windows(N).rposition(|window| le_u32(window) == needle)
    }

    // Filters the blocks, then searches what is left. This loop is not inlined,
    // as it takes more instructions in a larger function. It runs only when a
    // block is left, so the call is a small part of its time.
    #[inline(never)]
    fn blocks(mut head: &[u8], bc: &[u64; N], needle: u32) -> Option<usize> {
        while let Some((_, tail)) = head.split_last_chunk::<TAIL>() {
            if candidates(tail, bc) {
                if let Some(end) = scan(tail, bc) {
                    return Some(head.len() + end - TAIL - N);
                }
            }
            (head, _) = head.split_at(head.len().saturating_sub(BLOCK));
        }

        words(head, bc, needle)
    }

    if haystack.len() < N {
        return None;
    }

    let bytes = NEEDLE.to_le_bytes();
    let bc = bytes.map(|x| u64::from_ne_bytes([x; LANES]));

    // A match is most often in the last block, as an end of central directory
    // record with no or a short comment, so it is checked before blocks are
    // filtered
    let mut head = haystack;
    if let Some((_, tail)) = head.split_last_chunk::<TAIL>() {
        if let Some(end) = scan(tail, &bc) {
            // The match ends at the same distance from the end of `head` as
            // from the end of `tail`
            return Some(head.len() + end - TAIL - N);
        }
        (head, _) = head.split_at(head.len().saturating_sub(BLOCK));
        if head.len() >= TAIL {
            return blocks(head, &bc, NEEDLE);
        }
    }

    words(head, &bc, NEEDLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::{Arbitrary, Gen};
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

    /// A haystack long enough to reach the block filter. The default `Vec`
    /// is shorter than 100 bytes, so it cannot.
    #[derive(Clone, Debug)]
    struct Haystack(Vec<u8>);

    impl Arbitrary for Haystack {
        fn arbitrary(g: &mut Gen) -> Self {
            let len = usize::arbitrary(g) % 1024;

            // Random bytes seldom hold the signature, so a dense haystack
            // makes each byte one of the signature's bytes or a zero, which
            // gives matches and near misses at all offsets
            let dense = bool::arbitrary(g);
            let data = (0..len)
                .map(|_| {
                    let b = u8::arbitrary(g);
                    match dense {
                        true => END_OF_CENTRAL_DIR_SIGNATURE_BYTES
                            .get(usize::from(b % 5))
                            .copied()
                            .unwrap_or(0),
                        false => b,
                    }
                })
                .collect();
            Haystack(data)
        }

        fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
            Box::new(self.0.shrink().map(Haystack))
        }
    }

    #[quickcheck]
    fn test_rfind_matches_windows_rposition(data: Haystack) {
        assert_rfind_matches_windows_rposition(&data.0, "random haystack");
    }

    // The longest haystack in the exhaustive tests. It spans the last block,
    // two filtered blocks, and each length of the word search after them.
    const EXHAUSTIVE_LEN: usize = 5 * 64;

    fn assert_rfind_matches_windows_rposition(data: &[u8], context: &str) {
        let expected = data
            .windows(END_OF_CENTRAL_DIR_SIGNATURE_BYTES.len())
            .rposition(|window| window == END_OF_CENTRAL_DIR_SIGNATURE_BYTES);

        assert_eq!(
            rfind::<END_OF_CENTRAL_DIR_SIGNATURE>(data),
            expected,
            "{context}"
        );
    }

    fn filled(fill: &[u8], len: usize) -> Vec<u8> {
        fill.iter().copied().cycle().take(len).collect()
    }

    // Each fill passes one more of the search's filters before it rejects the
    // fill
    #[rstest]
    #[case::zeros(&[0x00])]
    #[case::last_byte(&[0x06])]
    #[case::last_two_bytes(&[0x05, 0x06])]
    #[case::first_and_last_bytes(&[0x50, 0x00, 0x00, 0x06])]
    #[case::all_but_one_byte(&[0x50, 0x00, 0x05, 0x06])]
    fn test_rfind_signature_at_each_offset(#[case] fill: &[u8]) {
        let sig = END_OF_CENTRAL_DIR_SIGNATURE_BYTES;
        for len in 0..=EXHAUSTIVE_LEN {
            let mut data = filled(fill, len);
            assert_rfind_matches_windows_rposition(&data, &format!("len {len}"));

            for pos in 0..len.saturating_sub(sig.len() - 1) {
                data[pos..pos + sig.len()].copy_from_slice(&sig);
                assert_rfind_matches_windows_rposition(&data, &format!("len {len}, pos {pos}"));
                data.copy_from_slice(&filled(fill, len));
            }
        }
    }

    // The search must return the last of two matches at each pair of offsets
    #[rstest]
    #[case::zeros(&[0x00])]
    #[case::all_but_one_byte(&[0x50, 0x00, 0x05, 0x06])]
    fn test_rfind_last_of_two_signatures(#[case] fill: &[u8]) {
        let sig = END_OF_CENTRAL_DIR_SIGNATURE_BYTES;
        let empty = filled(fill, EXHAUSTIVE_LEN);
        let mut data = empty.clone();
        for first in 0..=EXHAUSTIVE_LEN - 2 * sig.len() {
            for last in first + sig.len()..=EXHAUSTIVE_LEN - sig.len() {
                data[first..first + sig.len()].copy_from_slice(&sig);
                data[last..last + sig.len()].copy_from_slice(&sig);
                assert_rfind_matches_windows_rposition(
                    &data,
                    &format!("first {first}, last {last}"),
                );
                data.copy_from_slice(&empty);
            }
        }
    }
}
