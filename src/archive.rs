#[cfg(feature = "std")]
use crate::Crc32;
use crate::errors::{Error, ErrorKind};
use crate::extra_fields::{ExtraFieldId, ExtraFields};
use crate::headers::EntryFlags;
use crate::mode::{
    CreatorSystem, EntryMode, VersionMadeBy, msdos_mode_to_file_mode, unix_mode_to_file_mode,
};
use crate::path::{RawPath, ZipFilePath};
#[cfg(feature = "std")]
use crate::reader_at::{ReaderAt, ReaderAtExt};
use crate::time::{DosDateTime, ZipDateTimeKind, extract_best_timestamp};
use crate::utils::{le_u16, le_u32, le_u64};
use crate::{EndOfCentralDirectory, EndOfCentralDirectoryRecordFixed, ZipLocator};
#[cfg(feature = "std")]
use std::io::Write;

#[cfg(feature = "std")]
mod reader;
#[cfg(feature = "std")]
pub use reader::{ZipEntries, ZipEntry, ZipReader, ZipSliceVerifier, ZipVerifier};

pub(crate) const END_OF_CENTRAL_DIR_SIGNATURE64: u32 = 0x06064b50;
pub(crate) const END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE: u32 = 0x07064b50;
pub(crate) const CENTRAL_HEADER_SIGNATURE: u32 = 0x02014b50;

/// The recommended buffer size to use when reading from a zip file.
///
/// A buffer of this size should hold an entire central directory record as the
/// spec recommends (4.4.10):
///
/// > the combined length of any directory and these three fields SHOULD NOT
/// > generally exceed 65,535 bytes.
///
/// However, a pathological central directory record can still exceed this size
/// causing [`ErrorKind::BufferTooSmall`] to be returned when parsing entries.
/// [`MAX_CENTRAL_DIRECTORY_RECORD_SIZE`] can be used to avoid this error.
pub const RECOMMENDED_BUFFER_SIZE: usize = 1 << 16;

/// The maximum size in bytes of a single central directory file header record.
///
/// - Fixed-size: 46 bytes
/// - file name, comment, and extra data can each be up to 65,535 bytes
///
/// A buffer of this size guarantees [`ErrorKind::BufferTooSmall`] can never
/// occur. At roughly 192 KiB, it is still small enough to be reasonable for
/// most use cases.
pub const MAX_CENTRAL_DIRECTORY_RECORD_SIZE: usize =
    ZipFileHeaderFixed::SIZE + 3 * u16::MAX as usize;

/// Represents a Zip archive that operates on an in-memory data.
///
/// A [`ZipSliceArchive`] is more efficient and easier to use than a [`ZipArchive`],
/// as there is no buffer management and memory copying involved.
///
/// # Examples
///
/// ```rust
/// use rawzip::{ZipArchive, ZipSliceArchive, Error};
///
/// fn process_zip_slice(data: &[u8]) -> Result<(), Error> {
///     let archive = ZipArchive::from_slice(data)?;
///     println!("Found {} entries.", archive.entries_hint());
///     for entry_result in archive.entries() {
///         let entry = entry_result?;
///         println!("File: {:?}", entry.file_path().as_ref());
///     }
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ZipSliceArchive<T> {
    data: T,
    eocd: EndOfCentralDirectory,
}

impl<T: AsRef<[u8]>> ZipSliceArchive<T> {
    pub(crate) fn new(data: T, eocd: EndOfCentralDirectory) -> Self {
        ZipSliceArchive { data, eocd }
    }

    /// Returns an iterator over the entries in the central directory of the archive.
    pub fn entries(&self) -> ZipSliceEntries<'_> {
        let data = self.data.as_ref();
        let directory_start = self.eocd.directory_offset();
        let entry_data = &data[(directory_start as usize)..self.eocd.head_eocd_offset() as usize];
        ZipSliceEntries {
            entry_data,
            base_offset: self.eocd.base_offset(),
            current_offset: directory_start,
        }
    }

    /// Returns a reference to the underlying data.
    pub fn get_ref(&self) -> &T {
        &self.data
    }

    /// Consumes this archive and returns the underlying data.
    pub fn into_inner(self) -> T {
        self.data
    }

    /// Returns a hint for the total number of entries in the archive.
    ///
    /// This value is read from the End of Central Directory record.
    pub fn entries_hint(&self) -> u64 {
        self.eocd.entries()
    }

    /// Returns the offset of the End of Central Directory (EOCD) signature.
    ///
    /// This is the byte position where the EOCD signature (`0x06054b50`) was
    /// found. It is useful for recovery scenarios when dealing with false EOCD
    /// signatures or when restarting archive searches from a known position.
    pub fn eocd_offset(&self) -> u64 {
        self.eocd.tail_eocd_offset()
    }

    /// The declared offset of the start of the central directory.
    ///
    /// To verify the validity of this offset, start iterating through the
    /// central directory via [`ZipSliceArchive::entries`]. Ensure no errors are
    /// returned on the first entry.
    ///
    /// This value is useful when calculating the amount of prelude data in the
    /// input, as it serves as the upper bound until each file's
    /// [`ZipFileHeaderRecord::local_header_offset`] can be examined.
    pub fn directory_offset(&self) -> u64 {
        self.eocd.directory_offset()
    }

    /// Returns the offset where the ZIP archive ends.
    ///
    /// This returns the position immediately after the last byte of the ZIP
    /// archive, including the End of Central Directory record and any comment.
    /// This is useful for extracting trailing data.
    ///
    /// The calculation does not rely on self-reported sizes from the archive.
    pub fn end_offset(&self) -> u64 {
        self.eocd.tail_eocd_offset()
            + EndOfCentralDirectoryRecordFixed::SIZE as u64
            + self.comment().as_bytes().len() as u64
    }

    /// The comment of the zip file.
    pub fn comment(&self) -> ZipStr<'_> {
        let data = self.data.as_ref();
        let comment_start =
            self.eocd.tail_eocd_offset() as usize + EndOfCentralDirectoryRecordFixed::SIZE;
        let comment_len = self.eocd.comment_len();
        ZipStr::new(&data[comment_start..comment_start + comment_len])
    }

    /// Converts the [`ZipSliceArchive`] into a general [`ZipArchive`] by
    /// wrapping the data in a [`std::io::Cursor`].
    ///
    /// This is useful for unifying code that might handle both slice-based and
    /// reader-based archives. The data is wrapped in a [`std::io::Cursor`] to
    /// provide the [`ReaderAt`] implementation needed for [`ZipArchive`].
    ///
    /// This is only needed for `AsRef<[u8]>` types that do not themselves
    /// implement [`ReaderAt`], such as a memory-mapped file (`memmap2::Mmap`).
    /// Otherwise prefer the zero-cost [`ZipSliceArchive::into_reader`].
    #[cfg(feature = "std")]
    pub fn into_cursor_archive(self) -> ZipArchive<std::io::Cursor<T>> {
        ZipArchive {
            reader: std::io::Cursor::new(self.data),
            eocd: self.eocd,
        }
    }

    /// Seeks to the given file entry in the zip archive.
    ///
    /// The slice API eagerly validates that the entire compressed data is
    /// present before returning a [`ZipSliceEntry`].
    pub fn get_entry(&self, entry: ZipArchiveEntryWayfinder) -> Result<ZipSliceEntry<'_>, Error> {
        let data = self.data.as_ref();
        let header = &data[(entry.local_header_offset as usize).min(data.len())..];
        let file_header = ZipLocalFileHeaderFixed::parse(header)?;
        let variable_length = file_header.variable_length();

        let header_size = (ZipLocalFileHeaderFixed::SIZE + variable_length) as u32;
        let (total_size, o1) =
            (u64::from(header_size)).overflowing_add(entry.compressed_size_hint());

        if o1 || (header.len() as u64) < total_size {
            return Err(Error::from(ErrorKind::Eof));
        }

        let (entire_entry, descriptor) = header.split_at(total_size as usize);

        Ok(ZipSliceEntry {
            data: entire_entry,
            verifier: ZipVerification {
                crc: entry.crc,
                uncompressed_size: entry.uncompressed_size_hint(),
            },
            local_header_offset: entry.local_header_offset,
            data_start_offset: header_size,
            has_data_descriptor: entry.has_data_descriptor,
            data_descriptor_uses_zip64_sizes: entry.data_descriptor_uses_zip64_sizes,
            descriptor,
        })
    }
}

/// Represents a single entry (file or directory) within a `ZipSliceArchive`.
///
/// It provides access to the raw compressed data of the entry.
#[derive(Debug, Clone)]
pub struct ZipSliceEntry<'a> {
    // From local header offset to end of compressed data
    data: &'a [u8],
    verifier: ZipVerification,
    local_header_offset: u64,
    // self.data[self.data_start_offset] is the start of compressed data
    data_start_offset: u32,
    has_data_descriptor: bool,
    data_descriptor_uses_zip64_sizes: bool,
    descriptor: &'a [u8],
}

impl<'a> ZipSliceEntry<'a> {
    /// Returns the raw, compressed data of the entry as a byte slice.
    pub fn data(&self) -> &'a [u8] {
        &self.data[self.data_start_offset as usize..]
    }

    /// Returns the expected CRC and uncompressed size of the inflated data.
    ///
    /// Rawzip considers the central directory to be authoritative, so this
    /// returns the CRC and uncompressed size from the central directory. If you
    /// want to check the data descriptor instead, use
    /// [`ZipSliceEntry::data_descriptor`] to read it.
    pub fn claim_verifier(&self) -> ZipVerification {
        self.verifier
    }

    /// Reads the trailing [`ZipDataDescriptor`] for this entry, if present.
    pub fn data_descriptor(&self) -> Result<Option<ZipDataDescriptor>, Error> {
        if !self.has_data_descriptor {
            return Ok(None);
        }

        let descriptor =
            DataDescriptor::parse(self.descriptor, self.data_descriptor_uses_zip64_sizes)?;
        Ok(Some(ZipDataDescriptor {
            crc: descriptor.crc,
            compressed_size: descriptor.compressed_size,
            uncompressed_size: descriptor.uncompressed_size,
        }))
    }

    /// Returns a reader that wraps a decompressor and verify the size and CRC
    /// of the decompressed data once finished.
    #[cfg(feature = "std")]
    pub fn verifying_reader<D>(&self, reader: D) -> ZipSliceVerifier<D>
    where
        D: std::io::Read,
    {
        ZipSliceVerifier(ZipVerifier {
            reader,
            verifier: self.verifier,
            crc: Crc32::new(),
            size: 0,
        })
    }

    /// Returns the byte range of the compressed data within the archive.
    ///
    /// This range is calculated from the local file header and identifies the
    /// compressed data within the original archive bytes.
    ///
    /// # Security Usage
    ///
    /// This method is useful for detecting overlapping entries, which are often
    /// used in zip bombs. By comparing the ranges returned by this method
    /// across multiple entries, you can identify when entries share compressed
    /// data:
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, Error};
    /// # fn example(data: &[u8]) -> Result<(), Error> {
    /// let archive = ZipArchive::from_slice(data)?;
    /// let mut ranges = Vec::new();
    ///
    /// for entry_result in archive.entries() {
    ///     let entry = entry_result?;
    ///     let wayfinder = entry.wayfinder();
    ///     if let Ok(zip_entry) = archive.get_entry(wayfinder) {
    ///         ranges.push(zip_entry.compressed_data_range());
    ///     }
    /// }
    ///
    /// // Check for overlapping ranges
    /// ranges.sort_by_key(|&(start, _)| start);
    /// for window in ranges.windows(2) {
    ///     let (_, end1) = window[0];
    ///     let (start2, _) = window[1];
    ///     if end1 > start2 {
    ///         panic!("Warning: Overlapping entries detected!");
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn compressed_data_range(&self) -> (u64, u64) {
        let compressed_data_start = self.local_header_offset + self.data_start_offset as u64;
        let compressed_data_end =
            compressed_data_start + (self.data.len() - self.data_start_offset as usize) as u64;
        (compressed_data_start, compressed_data_end)
    }

    /// Returns the local file header information.
    ///
    /// This is the single entry point for the entry's file path, extra fields,
    /// and flags from the local header.
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, Error};
    /// # fn example(data: &[u8]) -> Result<(), Error> {
    /// let archive = ZipArchive::from_slice(data)?;
    /// let entry_header = archive.entries().next_entry()?.unwrap();
    /// let entry = archive.get_entry(entry_header.wayfinder())?;
    /// let header = entry.local_header();
    /// let _path = header.file_path();
    /// let _flags = header.flags();
    /// for (_id, _field) in header.extra_fields() {}
    /// # Ok(())
    /// # }
    /// ```
    pub fn local_header(&self) -> ZipLocalFileHeader<'a> {
        let header =
            ZipLocalFileHeaderFixed::parse(self.data).expect("header has already been parsed");
        let file_name_len = header.file_name_len as usize;
        let extra_field_len = header.extra_field_len as usize;
        let filename_start = ZipLocalFileHeaderFixed::SIZE;
        let filename_end = filename_start + file_name_len;
        let extra_field_end = filename_end + extra_field_len;
        let (compressed_size, uncompressed_size) =
            local_header_size_hints(&header, &self.data[filename_end..extra_field_end]);
        ZipLocalFileHeader {
            fixed: header,
            compressed_size,
            uncompressed_size,
            file_path: ZipFilePath::from_bytes(&self.data[filename_start..filename_end]),
            extra_field: &self.data[filename_end..extra_field_end],
        }
    }
}

/// An iterator over the central directory file header records.
///
/// Created from [`ZipSliceArchive::entries`].
#[derive(Debug, Clone)]
pub struct ZipSliceEntries<'data> {
    entry_data: &'data [u8],
    base_offset: u64,
    current_offset: u64,
}

impl<'data> ZipSliceEntries<'data> {
    /// Yield the next zip file entry in the central directory if there is any
    #[inline]
    pub fn next_entry(&mut self) -> Result<Option<ZipFileHeaderRecord<'data>>, Error> {
        if self.entry_data.is_empty() {
            return Ok(None);
        }

        let file_header = ZipFileHeaderFixed::parse(self.entry_data)?;
        let Some((file_name, extra_field, file_comment, entry_data)) =
            file_header.parse_variable_length(&self.entry_data[ZipFileHeaderFixed::SIZE..])
        else {
            return Err(Error::from(ErrorKind::Eof));
        };

        let mut entry = ZipFileHeaderRecord::from_parts(
            file_header,
            file_name,
            extra_field,
            file_comment,
            self.current_offset,
        );
        entry.local_header_offset += self.base_offset;
        self.current_offset += (self.entry_data.len() - entry_data.len()) as u64;
        self.entry_data = entry_data;
        Ok(Some(entry))
    }
}

impl<'data> Iterator for ZipSliceEntries<'data> {
    type Item = Result<ZipFileHeaderRecord<'data>, Error>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_entry().transpose()
    }
}

/// The main entrypoint for reading a Zip archive.
///
/// It can be created from a slice with [`ZipArchive::from_slice`].
#[cfg_attr(
    feature = "std",
    doc = "With the `std` feature, it can also be created from a file or any `Read + Seek` source."
)]
///
/// For more complex use cases, use the [`ZipLocator`] to locate an archive.
#[cfg_attr(
    feature = "std",
    doc = r#"
# Examples

Creating from a file:

```rust
# fn example_from_file(file: std::fs::File) -> Result<(), rawzip::Error> {
#     use rawzip::{ZipArchive, RECOMMENDED_BUFFER_SIZE};
let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
let archive = ZipArchive::from_file(file, &mut buffer)?;
#     Ok(())
# }
```
"#
)]
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "std"), allow(dead_code))]
pub struct ZipArchive<R> {
    reader: R,
    eocd: EndOfCentralDirectory,
}

impl ZipArchive<()> {
    /// Creates a [`ZipLocator`] configured with a maximum search space for the
    /// End of Central Directory Record (EOCD).
    pub fn with_max_search_space(max_search_space: u64) -> ZipLocator {
        ZipLocator::new().max_search_space(max_search_space)
    }

    /// Parses an archive from in-memory data.
    pub fn from_slice<T: AsRef<[u8]>>(data: T) -> Result<ZipSliceArchive<T>, Error> {
        ZipLocator::new().locate_in_slice(data).map_err(|(_, e)| e)
    }
}

/// Holds the expected CRC32 checksum and uncompressed size for a Zip entry.
///
/// This struct is used to verify the integrity of decompressed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipVerification {
    /// The expected CRC32 checksum of the uncompressed data.
    pub crc: u32,

    /// The expected uncompressed size of the entry.
    pub uncompressed_size: u64,
}

impl ZipVerification {
    /// Validates the size and CRC of the entry.
    ///
    /// Returns an error if either the size or the CRC does not match.
    pub fn valid(&self, rhs: ZipVerification) -> Result<(), Error> {
        if self.uncompressed_size != rhs.uncompressed_size {
            return Err(Error::from(ErrorKind::InvalidSize {
                expected: self.uncompressed_size,
                actual: rhs.uncompressed_size,
            }));
        }

        if self.crc != rhs.crc {
            return Err(Error::from(ErrorKind::InvalidChecksum {
                expected: self.crc,
                actual: rhs.crc,
            }));
        }

        Ok(())
    }
}

/// Local file header information from a ZIP archive entry.
///
/// This struct provides access to data stored in the local file header of a ZIP entry,
/// which may differ from the information in the central directory. The local header
/// contains the filename and extra fields as they appear at the start of each entry's
/// data within the ZIP file.
///
/// Most ZIP tools use the central directory as authoritative, but access to local
/// header data is useful for validation, security analysis, and forensic purposes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZipLocalFileHeader<'a> {
    fixed: ZipLocalFileHeaderFixed,
    compressed_size: u64,
    uncompressed_size: u64,
    file_path: ZipFilePath<RawPath<'a>>,
    extra_field: &'a [u8],
}

impl<'a> ZipLocalFileHeader<'a> {
    /// Returns the general purpose bit flags from the local file header.
    ///
    /// These may differ from the central directory record's flags. See
    /// [`EntryFlags`] for the individual flag accessors.
    #[inline]
    pub fn flags(&self) -> EntryFlags {
        self.fixed.flags
    }

    /// Returns the file path from the local file header.
    ///
    /// This may differ from the central directory file path.
    #[inline]
    pub fn file_path(&self) -> ZipFilePath<RawPath<'a>> {
        self.file_path
    }

    /// Returns the raw MS-DOS modification timestamp from the local file header.
    ///
    /// This may differ from the central directory record's
    /// [`last_modified_dos`](ZipFileHeaderRecord::last_modified_dos).
    #[inline]
    pub fn last_modified_dos(&self) -> DosDateTime {
        DosDateTime::new(self.fixed.last_mod_time, self.fixed.last_mod_date)
    }

    /// Returns the compression method declared in the local file header.
    #[inline]
    pub fn compression_method(&self) -> CompressionMethod {
        self.fixed.compression_method
    }

    /// Returns the last modification date and time declared in the local file header.
    ///
    /// Extended timestamps in the local header's extra fields are preferred
    /// when present.
    #[inline]
    pub fn last_modified(&self) -> ZipDateTimeKind {
        extract_best_timestamp(
            self.extra_fields(),
            self.fixed.last_mod_time,
            self.fixed.last_mod_date,
        )
    }

    /// The CRC32 checksum declared in the local file header.
    ///
    /// **WARNING**: this value is zero when written by a streaming writer,
    /// and may otherwise differ from the central directory record's value.
    #[inline]
    pub fn crc32(&self) -> u32 {
        self.fixed.crc32
    }

    /// The purported number of bytes of the compressed data from the local file header.
    ///
    /// **WARNING**: this value is zero when written by a streaming writer,
    /// and may otherwise differ from the central directory record's value.
    #[inline]
    pub fn compressed_size_hint(&self) -> u64 {
        self.compressed_size
    }

    /// The purported number of bytes of the uncompressed data from the local file header.
    ///
    /// **WARNING**: this value is zero when written by a streaming writer,
    /// and may otherwise differ from the central directory record's value.
    #[inline]
    pub fn uncompressed_size_hint(&self) -> u64 {
        self.uncompressed_size
    }

    /// Returns an iterator over the extra fields from the local file header.
    ///
    /// Extra fields in the local header may differ from those in the central directory.
    /// The local header may contain additional or different metadata compared to the
    /// central directory entry.
    #[inline]
    pub fn extra_fields(&self) -> ExtraFields<'a> {
        ExtraFields::new(self.extra_field)
    }
}

fn local_header_size_hints(header: &ZipLocalFileHeaderFixed, extra_field: &[u8]) -> (u64, u64) {
    let mut compressed_size = u64::from(header.compressed_size);
    let mut uncompressed_size = u64::from(header.uncompressed_size);

    if header.compressed_size == u32::MAX || header.uncompressed_size == u32::MAX {
        for (field_id, field_data) in ExtraFields::new(extra_field) {
            if field_id != ExtraFieldId::ZIP64 {
                continue;
            }

            let mut field = field_data;
            if header.uncompressed_size == u32::MAX {
                if let Some(v) = field.get(..8).map(le_u64) {
                    uncompressed_size = v;
                    field = &field[8..];
                }
            }

            if header.compressed_size == u32::MAX {
                if let Some(v) = field.get(..8).map(le_u64) {
                    compressed_size = v;
                }
            }

            break;
        }
    }

    (compressed_size, uncompressed_size)
}

/// The data descriptor trailing an entry's compressed data.
///
/// Obtain one via [`ZipSliceEntry::data_descriptor`].
///
/// The descriptor's size fields are 4 or 8 bytes wide depending on whether the
/// entry's central directory record signalled zip64 sizes (APPNOTE 4.3.9.2).
///
/// Decoding assumes the optional `0x08074b50` signature is present iff the
/// first four bytes equal it. For the rare producer that omits the signature on
/// a descriptor whose CRC equals `0x08074b50`, the fields are decoded shifted
/// by four bytes, causing a false rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipDataDescriptor {
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
}

impl ZipDataDescriptor {
    /// The CRC32 checksum of the uncompressed data as recorded in the data
    /// descriptor.
    #[inline]
    pub fn crc32(&self) -> u32 {
        self.crc
    }

    /// The compressed size of the entry as recorded in the data descriptor.
    #[inline]
    pub fn compressed_size(&self) -> u64 {
        self.compressed_size
    }

    /// The uncompressed size of the entry as recorded in the data descriptor.
    #[inline]
    pub fn uncompressed_size(&self) -> u64 {
        self.uncompressed_size
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DataDescriptor {
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
}

impl DataDescriptor {
    /// The maximum on-disk size of a data descriptor: optional 4-byte
    /// signature + 4-byte crc + two 8-byte zip64 sizes.
    #[cfg(feature = "std")]
    const MAX_SIZE: usize = 24;
    pub const SIGNATURE: u32 = 0x08074b50;

    /// Parses a data descriptor from `data`.
    fn parse(data: &[u8], uses_zip64_sizes: bool) -> Result<DataDescriptor, Error> {
        let eof = || Error::from(ErrorKind::Eof);

        // Skip the optional 0x08074b50 signature (APPNOTE 4.3.9.3).
        let body = match data.split_first_chunk::<4>() {
            Some((sig, rest)) if u32::from_le_bytes(*sig) == Self::SIGNATURE => rest,
            _ => data,
        };

        let (crc, sizes) = body.split_first_chunk::<4>().ok_or_else(eof)?;
        let crc = u32::from_le_bytes(*crc);

        // The compressed/uncompressed sizes are 8 bytes each in zip64 mode,
        // otherwise 4 bytes each (APPNOTE 4.3.9.2).
        let (compressed_size, uncompressed_size) = if uses_zip64_sizes {
            let (compressed, rest) = sizes.split_first_chunk::<8>().ok_or_else(eof)?;
            let (uncompressed, _) = rest.split_first_chunk::<8>().ok_or_else(eof)?;
            (
                u64::from_le_bytes(*compressed),
                u64::from_le_bytes(*uncompressed),
            )
        } else {
            let (compressed, rest) = sizes.split_first_chunk::<4>().ok_or_else(eof)?;
            let (uncompressed, _) = rest.split_first_chunk::<4>().ok_or_else(eof)?;
            (
                u64::from(u32::from_le_bytes(*compressed)),
                u64::from(u32::from_le_bytes(*uncompressed)),
            )
        };

        Ok(DataDescriptor {
            crc,
            compressed_size,
            uncompressed_size,
        })
    }

    #[cfg(feature = "std")]
    fn read_at<R>(reader: R, offset: u64, uses_zip64_sizes: bool) -> Result<DataDescriptor, Error>
    where
        R: ReaderAt,
    {
        // It's safe to over-read here due to size of cd + eocd is already greater
        let mut buffer = [0u8; Self::MAX_SIZE];
        let read = reader.try_read_at_least_at(&mut buffer, Self::MAX_SIZE, offset)?;
        Self::parse(&buffer[..read], uses_zip64_sizes)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Zip64EndOfCentralDirectory {
    pub offset: u64,
    pub central_dir_offset: u64,
    pub central_dir_size: u64,
    pub num_entries: u64,
}

impl Zip64EndOfCentralDirectory {
    #[inline]
    pub fn from_parts(offset: u64, record: Zip64EndOfCentralDirectoryRecord) -> Self {
        Self {
            offset,
            central_dir_offset: record.central_dir_offset,
            central_dir_size: record.central_dir_size,
            num_entries: record.num_entries,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Zip64EndOfCentralDirectoryRecord {
    /// zip64 end of central dir signature
    pub signature: u32,

    /// size of zip64 end of central directory record
    #[allow(dead_code)]
    pub size: u64,

    /// version made by
    #[allow(dead_code)]
    pub version_made_by: VersionMadeBy,

    /// version needed to extract
    #[allow(dead_code)]
    pub version_needed: u16,

    /// number of this disk
    #[allow(dead_code)]
    pub disk_number: u32,

    /// number of the disk with the start of the central directory
    #[allow(dead_code)]
    pub cd_disk: u32,

    /// total number of entries in the central directory on this disk
    pub num_entries: u64,

    /// total number of entries in the central directory
    #[allow(dead_code)]
    pub total_entries: u64,

    /// size of the central directory
    pub central_dir_size: u64,

    /// offset of start of central directory with respect to the starting disk number
    pub central_dir_offset: u64,
    // zip64 extensible data sector
    // pub extensible_data: Vec<u8>,
}

impl Zip64EndOfCentralDirectoryRecord {
    pub(crate) const SIZE: usize = 56;

    #[inline]
    pub fn parse(data: &[u8]) -> Result<Zip64EndOfCentralDirectoryRecord, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let result = Zip64EndOfCentralDirectoryRecord {
            signature: le_u32(&data[0..4]),
            size: le_u64(&data[4..12]),
            version_made_by: VersionMadeBy::from_raw(le_u16(&data[12..14])),
            version_needed: le_u16(&data[14..16]),
            disk_number: le_u32(&data[16..20]),
            cd_disk: le_u32(&data[20..24]),
            num_entries: le_u64(&data[24..32]),
            total_entries: le_u64(&data[32..40]),
            central_dir_size: le_u64(&data[40..48]),
            central_dir_offset: le_u64(&data[48..56]),
        };

        if result.signature != END_OF_CENTRAL_DIR_SIGNATURE64 {
            return Err(Error::from(ErrorKind::InvalidSignature {
                expected: END_OF_CENTRAL_DIR_SIGNATURE64,
                actual: result.signature,
            }));
        }

        Ok(result)
    }
}

/// The compression method used on an individual Zip archive entry.
///
/// Contains associated consts for compression methods mentioned in the spec (§
/// 4.4.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompressionMethod(u16);

impl CompressionMethod {
    /// No compression.
    pub const STORE: Self = Self(0);
    pub const SHRUNK: Self = Self(1);
    pub const REDUCE1: Self = Self(2);
    pub const REDUCE2: Self = Self(3);
    pub const REDUCE3: Self = Self(4);
    pub const REDUCE4: Self = Self(5);
    pub const IMPLODED: Self = Self(6);
    pub const TOKENIZING: Self = Self(7);
    /// Deflate.
    pub const DEFLATE: Self = Self(8);
    pub const DEFLATE64: Self = Self(9);
    /// PKWARE DCL Imploding. Historically also labeled "old IBM TERSE" in some
    /// APPNOTE revisions; method 18 ([`CompressionMethod::TERSE`]) is the
    /// current ("new") IBM TERSE.
    pub const DCL_IMPLODE: Self = Self(10);
    pub const BZIP2: Self = Self(12);
    pub const LZMA: Self = Self(14);
    /// IBM CMPSC compression.
    pub const CMPSC: Self = Self(16);
    /// IBM TERSE (new).
    pub const TERSE: Self = Self(18);
    /// IBM LZ77 z Architecture (PFS).
    pub const LZ77: Self = Self(19);
    /// Deprecated zstd id; use [`CompressionMethod::ZSTD`] (93) for zstd.
    pub const ZSTD_DEPRECATED: Self = Self(20);
    /// Zstandard.
    pub const ZSTD: Self = Self(93);
    pub const MP3: Self = Self(94);
    pub const XZ: Self = Self(95);
    pub const JPEG: Self = Self(96);
    pub const WAVPACK: Self = Self(97);
    pub const PPMD: Self = Self(98);
    /// AES encryption.
    pub const AES: Self = Self(99);

    /// Wrap a raw compression-method id.
    #[inline]
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    /// Returns the raw value of the compression method.
    #[inline]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Returns the method name (eg" `"DEFLATE"`) when known
    #[inline]
    pub const fn name(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("STORE"),
            1 => Some("SHRUNK"),
            2 => Some("REDUCE1"),
            3 => Some("REDUCE2"),
            4 => Some("REDUCE3"),
            5 => Some("REDUCE4"),
            6 => Some("IMPLODED"),
            7 => Some("TOKENIZING"),
            8 => Some("DEFLATE"),
            9 => Some("DEFLATE64"),
            10 => Some("DCL_IMPLODE"),
            12 => Some("BZIP2"),
            14 => Some("LZMA"),
            16 => Some("CMPSC"),
            18 => Some("TERSE"),
            19 => Some("LZ77"),
            20 => Some("ZSTD_DEPRECATED"),
            93 => Some("ZSTD"),
            94 => Some("MP3"),
            95 => Some("XZ"),
            96 => Some("JPEG"),
            97 => Some("WAVPACK"),
            98 => Some("PPMD"),
            99 => Some("AES"),
            _ => None,
        }
    }
}

impl core::fmt::Debug for CompressionMethod {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.name() {
            Some(name) => write!(f, "CompressionMethod::{name}"),
            None => write!(f, "CompressionMethod({})", self.0),
        }
    }
}

impl core::fmt::Display for CompressionMethod {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.0, self.name().unwrap_or("UNKNOWN"))
    }
}

impl From<u16> for CompressionMethod {
    fn from(id: u16) -> Self {
        Self(id)
    }
}

// Backwards-compat aliases for the old enum variant names.
#[allow(non_upper_case_globals)]
impl CompressionMethod {
    #[deprecated(note = "use CompressionMethod::STORE")]
    pub const Store: Self = Self::STORE;
    #[deprecated(note = "use CompressionMethod::SHRUNK")]
    pub const Shrunk: Self = Self::SHRUNK;
    #[deprecated(note = "use CompressionMethod::REDUCE1")]
    pub const Reduce1: Self = Self::REDUCE1;
    #[deprecated(note = "use CompressionMethod::REDUCE2")]
    pub const Reduce2: Self = Self::REDUCE2;
    #[deprecated(note = "use CompressionMethod::REDUCE3")]
    pub const Reduce3: Self = Self::REDUCE3;
    #[deprecated(note = "use CompressionMethod::REDUCE4")]
    pub const Reduce4: Self = Self::REDUCE4;
    #[deprecated(note = "use CompressionMethod::IMPLODED")]
    pub const Imploded: Self = Self::IMPLODED;
    #[deprecated(note = "use CompressionMethod::TOKENIZING")]
    pub const Tokenizing: Self = Self::TOKENIZING;
    #[deprecated(note = "use CompressionMethod::DEFLATE")]
    pub const Deflate: Self = Self::DEFLATE;
    #[deprecated(note = "use CompressionMethod::DEFLATE64")]
    pub const Deflate64: Self = Self::DEFLATE64;
    #[deprecated(note = "use CompressionMethod::DCL_IMPLODE")]
    pub const DclImplode: Self = Self::DCL_IMPLODE;
    #[deprecated(note = "use CompressionMethod::BZIP2")]
    pub const Bzip2: Self = Self::BZIP2;
    #[deprecated(note = "use CompressionMethod::LZMA")]
    pub const Lzma: Self = Self::LZMA;
    #[deprecated(note = "use CompressionMethod::CMPSC")]
    pub const Cmpsc: Self = Self::CMPSC;
    #[deprecated(note = "use CompressionMethod::TERSE")]
    pub const Terse: Self = Self::TERSE;
    #[deprecated(note = "use CompressionMethod::LZ77")]
    pub const Lz77: Self = Self::LZ77;
    #[deprecated(note = "use CompressionMethod::ZSTD_DEPRECATED")]
    pub const ZstdDeprecated: Self = Self::ZSTD_DEPRECATED;
    #[deprecated(note = "use CompressionMethod::ZSTD")]
    pub const Zstd: Self = Self::ZSTD;
    #[deprecated(note = "use CompressionMethod::MP3")]
    pub const Mp3: Self = Self::MP3;
    #[deprecated(note = "use CompressionMethod::XZ")]
    pub const Xz: Self = Self::XZ;
    #[deprecated(note = "use CompressionMethod::JPEG")]
    pub const Jpeg: Self = Self::JPEG;
    #[deprecated(note = "use CompressionMethod::WAVPACK")]
    pub const WavPack: Self = Self::WAVPACK;
    #[deprecated(note = "use CompressionMethod::PPMD")]
    pub const Ppmd: Self = Self::PPMD;
    #[deprecated(note = "use CompressionMethod::AES")]
    pub const Aes: Self = Self::AES;
}

/// A borrowed data from a Zip archive, typically for comments or non-path text.
///
/// Zip archives may contain text that is not strictly UTF-8. This type
/// represents such text as a byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ZipStr<'a>(&'a [u8]);

impl<'a> ZipStr<'a> {
    /// Creates a new `ZipStr` from a byte slice.
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self(data)
    }

    /// Returns the underlying byte slice.
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Converts the borrowed `ZipStr` into an owned `ZipString` by cloning the
    /// data.
    #[cfg(feature = "alloc")]
    #[inline]
    pub fn into_owned(&self) -> ZipString {
        ZipString::new(self.0.to_vec())
    }
}

/// An owned string (`Vec<u8>`) from a Zip archive, typically for comments or non-path text.
///
/// Similar to `ZipStr`, but owns its data.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ZipString(alloc::vec::Vec<u8>);

#[cfg(feature = "alloc")]
impl ZipString {
    /// Creates a new `ZipString` from a vector of bytes.
    #[inline]
    pub fn new(data: alloc::vec::Vec<u8>) -> Self {
        Self(data)
    }

    /// Returns a borrowed `ZipStr` view of this `ZipString`.
    #[inline]
    pub fn as_str(&self) -> ZipStr<'_> {
        ZipStr::new(self.0.as_slice())
    }
}

/// Represents a record from the Zip archive's central directory for a single
/// file
///
/// This contains metadata about the file. If interested in navigating to the
/// file contents, use `[ZipFileHeaderRecord::wayfinder]`.
///
/// Reference 4.3.12 in the zip specification
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ZipFileHeaderRecord<'a> {
    signature: u32,
    version_made_by: u16,
    version_needed: u16,
    flags: EntryFlags,
    compression_method: CompressionMethod,
    last_mod_time: u16,
    last_mod_date: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    file_name_len: u16,
    extra_field_len: u16,
    file_comment_len: u16,
    disk_number_start: u32,
    internal_file_attrs: u16,
    external_file_attrs: u32,
    local_header_offset: u64,
    central_directory_offset: u64,
    file_name: ZipFilePath<RawPath<'a>>,
    extra_field: &'a [u8],
    file_comment: ZipStr<'a>,
    is_zip64: bool,
    data_descriptor_uses_zip64_sizes: bool,
}

impl<'a> ZipFileHeaderRecord<'a> {
    #[inline]
    fn from_parts(
        header: ZipFileHeaderFixed,
        file_name: &'a [u8],
        extra_field: &'a [u8],
        file_comment: &'a [u8],
        central_directory_offset: u64,
    ) -> Self {
        let zip64_sizes =
            header.uncompressed_size == u32::MAX || header.compressed_size == u32::MAX;

        // Resolve descriptor size width from the raw inline size sentinels to
        // match libziparchive's approach:
        //
        // https://android.googlesource.com/platform/system/libziparchive/+/refs/tags/android-17.0.0_r1/zip_archive.cc#764
        let data_descriptor_uses_zip64_sizes = zip64_sizes;

        let mut result = Self {
            signature: header.signature,
            version_made_by: header.version_made_by,
            version_needed: header.version_needed,
            flags: header.flags,
            compression_method: header.compression_method,
            last_mod_time: header.last_mod_time,
            last_mod_date: header.last_mod_date,
            crc32: header.crc32,
            compressed_size: u64::from(header.compressed_size),
            uncompressed_size: u64::from(header.uncompressed_size),
            file_name_len: header.file_name_len,
            extra_field_len: header.extra_field_len,
            file_comment_len: header.file_comment_len,
            disk_number_start: u32::from(header.disk_number_start),
            internal_file_attrs: header.internal_file_attrs,
            external_file_attrs: header.external_file_attrs,
            local_header_offset: u64::from(header.local_header_offset),
            central_directory_offset,
            file_name: ZipFilePath::from_bytes(file_name),
            extra_field,
            file_comment: ZipStr::new(file_comment),
            is_zip64: false,
            data_descriptor_uses_zip64_sizes,
        };

        if !zip64_sizes
            && result.local_header_offset != u64::from(u32::MAX)
            && result.disk_number_start != u32::from(u16::MAX)
        {
            return result;
        }

        let extra_fields = ExtraFields::new(extra_field);
        for (field_id, field_data) in extra_fields {
            if field_id != ExtraFieldId::ZIP64 {
                continue;
            }

            let mut field = field_data;

            result.is_zip64 = true;

            if header.uncompressed_size == u32::MAX {
                let Some(uncompressed_size) = field.get(..8).map(le_u64) else {
                    break;
                };
                result.uncompressed_size = uncompressed_size;
                field = &field[8..];
            }

            if header.compressed_size == u32::MAX {
                let Some(compressed_size) = field.get(..8).map(le_u64) else {
                    break;
                };
                result.compressed_size = compressed_size;
                field = &field[8..];
            }

            if header.local_header_offset == u32::MAX {
                let Some(local_header_offset) = field.get(..8).map(le_u64) else {
                    break;
                };
                result.local_header_offset = local_header_offset;
                field = &field[8..];
            }

            if header.disk_number_start == u16::MAX {
                let Some(disk_number_start) = field.get(..4).map(le_u32) else {
                    break;
                };
                result.disk_number_start = disk_number_start;
            }

            break;
        }

        result
    }

    /// Describes if the file is a directory.
    ///
    /// See [`ZipFilePath::is_dir`] for more information.
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.file_name.is_dir()
    }

    /// Returns the general purpose bit flags for this entry.
    ///
    /// See [`EntryFlags`] for the individual flag accessors.
    #[inline]
    pub fn flags(&self) -> EntryFlags {
        self.flags
    }

    /// Describes where the file's data is located within the archive.
    #[inline]
    pub fn wayfinder(&self) -> ZipArchiveEntryWayfinder {
        ZipArchiveEntryWayfinder {
            uncompressed_size: self.uncompressed_size,
            compressed_size: self.compressed_size,
            local_header_offset: self.local_header_offset,
            has_data_descriptor: self.flags().has_data_descriptor(),
            crc: self.crc32,
            data_descriptor_uses_zip64_sizes: self.data_descriptor_uses_zip64_sizes,
        }
    }

    /// The purported number of bytes of the uncompressed data.
    ///
    /// **WARNING**: this number has not yet been validated, so don't trust it
    /// to make allocation decisions.
    #[inline]
    pub fn uncompressed_size_hint(&self) -> u64 {
        self.uncompressed_size
    }

    /// The purported number of bytes of the compressed data.
    ///
    /// **WARNING**: this number has not yet been validated, so don't trust it
    /// to make allocation decisions.
    #[inline]
    pub fn compressed_size_hint(&self) -> u64 {
        self.compressed_size
    }

    /// The declared offset to the local file header within the Zip archive.
    ///
    /// To verify the validity of this offset, call
    /// [`ZipSliceArchive::get_entry`].
    ///
    /// The minimum of all local header offsets (or `directory_offset()` when a
    /// zip is empty), will be the length of prelude data in a zip archive (data
    /// that is unrelated to the zip archive).
    ///
    #[inline]
    pub fn local_header_offset(&self) -> u64 {
        self.local_header_offset
    }

    /// The compression method used to compress the data
    #[inline]
    pub fn compression_method(&self) -> CompressionMethod {
        self.compression_method
    }

    /// Returns the file path in its raw form.
    ///
    /// # Safety
    ///
    /// The raw path may contain unsafe components like:
    /// - Absolute paths (`/etc/passwd`)
    /// - Directory traversal (`../../../etc/passwd`)
    /// - Invalid UTF-8 sequences
    ///
    /// # Example
    /// ```rust
    /// # use rawzip::ZipArchive;
    /// # fn example() -> Result<(), rawzip::Error> {
    /// # let data = include_bytes!("../assets/test.zip");
    /// # let archive = ZipArchive::from_slice(data)?;
    /// # let mut entries = archive.entries();
    /// # let entry = entries.next_entry()?.unwrap();
    /// // Get raw path (potentially unsafe)
    /// let raw_path = entry.file_path();
    /// println!("Raw path bytes: {:?}", raw_path.as_ref());
    /// # Ok(())
    /// # }
    /// # example()?;
    /// # Ok::<(), rawzip::Error>(())
    /// ```
    #[cfg_attr(
        feature = "alloc",
        doc = r#"
With the `alloc` feature, use [`ZipFilePath::try_normalize`] to create a safe path:

```rust
# use rawzip::ZipArchive;
# fn example() -> Result<(), Box<dyn std::error::Error>> {
# let data = include_bytes!("../assets/test.zip");
# let archive = ZipArchive::from_slice(data)?;
# let mut entries = archive.entries();
# let entry = entries.next_entry()?.unwrap();
let safe_path = entry.file_path().try_normalize()?;
println!("Safe path: {}", safe_path.as_ref());
# Ok(())
# }
# example()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```
"#
    )]
    #[inline]
    pub fn file_path(&self) -> ZipFilePath<RawPath<'a>> {
        self.file_name
    }

    /// Returns the last modification date and time.
    ///
    /// This method parses the extra field data to locate more accurate timestamps.
    #[inline]
    pub fn last_modified(&self) -> ZipDateTimeKind {
        extract_best_timestamp(self.extra_fields(), self.last_mod_time, self.last_mod_date)
    }

    /// Returns the raw MS-DOS modification timestamp stored in the central
    /// directory record.
    ///
    /// Unlike [`last_modified`](Self::last_modified), this is the raw value from
    /// the header, and ignores any higher-resolution Extended Timestamp
    /// extra field.
    #[inline]
    pub fn last_modified_dos(&self) -> DosDateTime {
        DosDateTime::new(self.last_mod_time, self.last_mod_date)
    }

    /// Returns the file mode information extracted from the external file attributes.
    ///
    /// Is a convenience method over interpreting
    /// [`version_made_by`](Self::version_made_by) and
    /// [`external_attributes`](Self::external_attributes).
    #[inline]
    pub fn mode(&self) -> EntryMode {
        let mut mode = match self.version_made_by().creator_system() {
            CreatorSystem::UNIX | CreatorSystem::MACOS => {
                unix_mode_to_file_mode(self.external_file_attrs >> 16)
            }
            CreatorSystem::FAT | CreatorSystem::NTFS | CreatorSystem::MVS | CreatorSystem::VFAT => {
                msdos_mode_to_file_mode(self.external_file_attrs)
            }
            // default to basic permissions
            _ => 0o644,
        };

        // Check if it's a directory by filename ending with '/'
        if self.is_dir() {
            mode |= 0o040000; // S_IFDIR
        }

        EntryMode::new(mode)
    }

    /// Returns the "version made by" field stored in the central directory record.
    #[inline]
    pub fn version_made_by(&self) -> VersionMadeBy {
        VersionMadeBy::from_raw(self.version_made_by)
    }

    /// Returns the raw external file attributes stored in the central directory.
    ///
    /// Consider if [`mode`](Self::mode) is a better alternative than accessing
    /// the raw bytes.
    #[inline]
    pub fn external_attributes(&self) -> u32 {
        self.external_file_attrs
    }

    /// The declared CRC32 checksum of the uncompressed data.
    ///
    /// To verify the validity of this value for slice-backed entries, compare
    /// it with the [`ZipSliceEntry::claim_verifier`] result while decompressing
    /// data from [`ZipSliceEntry::data`].
    #[inline]
    pub fn crc32(&self) -> u32 {
        self.crc32
    }

    /// Returns the offset from the start of reader where this central directory
    /// record was parsed from.
    #[inline]
    pub fn central_directory_offset(&self) -> u64 {
        self.central_directory_offset
    }

    /// Returns an iterator over the extra fields in this file header record.
    ///
    /// Extra fields contain additional metadata about files in ZIP archives,
    /// such as timestamps, alignment information, and platform-specific data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, extra_fields::ExtraFieldId};
    /// # fn example(data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    /// let archive = ZipArchive::from_slice(data)?;
    /// for entry_result in archive.entries() {
    ///     let entry = entry_result?;
    ///     let mut extra_fields = entry.extra_fields();
    ///     for (field_id, field_data) in extra_fields.by_ref() {
    ///         match field_id {
    ///             ExtraFieldId::JAVA_JAR => {
    ///                 println!("Handle jar CAFE field with {} bytes", field_data.len());
    ///             }
    ///             _ => {
    ///                 println!("Found extra field ID: 0x{:04x}", field_id.as_u16());
    ///             }
    ///         }
    ///     }
    ///
    ///     // If desired, check for truncated data
    ///     if !extra_fields.remaining_bytes().is_empty() {
    ///         println!("Warning: Some extra field data was truncated");
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Raw access to the entire extra field data is available when
    /// `remaining_bytes` is called prior to any iteration.
    #[inline]
    pub fn extra_fields(&self) -> ExtraFields<'_> {
        ExtraFields::new(self.extra_field)
    }

    /// Returns the file entry's comment.
    #[inline]
    pub fn comment(&self) -> ZipStr<'_> {
        self.file_comment
    }
}

/// Contains directions to where the Zip entry's data is located within the Zip archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipArchiveEntryWayfinder {
    uncompressed_size: u64,
    compressed_size: u64,
    local_header_offset: u64,
    crc: u32,
    has_data_descriptor: bool,
    data_descriptor_uses_zip64_sizes: bool,
}

impl ZipArchiveEntryWayfinder {
    /// Equivalent to [`ZipFileHeaderRecord::compressed_size_hint`]
    ///
    /// This is a convenience method to avoid having to deal with lifetime
    /// issues on a `ZipFileHeaderRecord`
    #[inline]
    pub fn uncompressed_size_hint(&self) -> u64 {
        self.uncompressed_size
    }

    /// Equivalent to [`ZipFileHeaderRecord::compressed_size_hint`]
    ///
    /// This is a convenience method to avoid having to deal with lifetime
    /// issues on a `ZipFileHeaderRecord`
    #[inline]
    pub fn compressed_size_hint(&self) -> u64 {
        self.compressed_size
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(not(feature = "std"), allow(dead_code))]
pub(crate) struct ZipLocalFileHeaderFixed {
    pub(crate) signature: u32,
    pub(crate) version_needed: u16,
    pub(crate) flags: EntryFlags,
    pub(crate) compression_method: CompressionMethod,
    pub(crate) last_mod_time: u16,
    pub(crate) last_mod_date: u16,
    pub(crate) crc32: u32,
    pub(crate) compressed_size: u32,
    pub(crate) uncompressed_size: u32,
    pub(crate) file_name_len: u16,
    pub(crate) extra_field_len: u16,
}

impl ZipLocalFileHeaderFixed {
    const SIZE: usize = 30;
    pub const SIGNATURE: u32 = 0x04034b50;

    pub fn parse(data: &[u8]) -> Result<ZipLocalFileHeaderFixed, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let result = ZipLocalFileHeaderFixed {
            signature: le_u32(&data[0..4]),
            version_needed: le_u16(&data[4..6]),
            flags: EntryFlags::new(le_u16(&data[6..8])),
            compression_method: CompressionMethod::new(le_u16(&data[8..10])),
            last_mod_time: le_u16(&data[10..12]),
            last_mod_date: le_u16(&data[12..14]),
            crc32: le_u32(&data[14..18]),
            compressed_size: le_u32(&data[18..22]),
            uncompressed_size: le_u32(&data[22..26]),
            file_name_len: le_u16(&data[26..28]),
            extra_field_len: le_u16(&data[28..30]),
        };

        if result.signature != Self::SIGNATURE {
            return Err(Error::from(ErrorKind::InvalidSignature {
                expected: Self::SIGNATURE,
                actual: result.signature,
            }));
        }

        Ok(result)
    }

    pub fn variable_length(&self) -> usize {
        self.file_name_len as usize + self.extra_field_len as usize
    }

    #[cfg(feature = "std")]
    pub fn write<W>(&self, mut writer: W) -> Result<(), Error>
    where
        W: Write,
    {
        // Batch writes with a fixed size buffer. Improved throughput 25%
        let mut buffer = [0u8; 30];
        buffer[..4].copy_from_slice(&self.signature.to_le_bytes());
        buffer[4..6].copy_from_slice(&self.version_needed.to_le_bytes());
        buffer[6..8].copy_from_slice(&self.flags.bits().to_le_bytes());
        buffer[8..10].copy_from_slice(&self.compression_method.0.to_le_bytes());
        buffer[10..12].copy_from_slice(&self.last_mod_time.to_le_bytes());
        buffer[12..14].copy_from_slice(&self.last_mod_date.to_le_bytes());
        buffer[14..18].copy_from_slice(&self.crc32.to_le_bytes());
        buffer[18..22].copy_from_slice(&self.compressed_size.to_le_bytes());
        buffer[22..26].copy_from_slice(&self.uncompressed_size.to_le_bytes());
        buffer[26..28].copy_from_slice(&self.file_name_len.to_le_bytes());
        buffer[28..30].copy_from_slice(&self.extra_field_len.to_le_bytes());
        writer.write_all(&buffer)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ZipFileHeaderFixed {
    pub signature: u32,
    pub version_made_by: u16,
    pub version_needed: u16,
    pub flags: EntryFlags,
    pub compression_method: CompressionMethod,
    pub last_mod_time: u16,
    pub last_mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub file_name_len: u16,
    pub extra_field_len: u16,
    pub file_comment_len: u16,
    pub disk_number_start: u16,
    pub internal_file_attrs: u16,
    pub external_file_attrs: u32,
    pub local_header_offset: u32,
}

impl ZipFileHeaderFixed {
    #[cfg(feature = "std")]
    pub fn variable_length(&self) -> usize {
        self.file_name_len as usize + self.extra_field_len as usize + self.file_comment_len as usize
    }
}

type VariableFields<'a> = (
    &'a [u8], // file_name
    &'a [u8], // extra_field
    &'a [u8], // file_comment
    &'a [u8], // rest of the data
);

impl ZipFileHeaderFixed {
    pub(crate) const SIZE: usize = 46;

    #[inline]
    pub fn parse(data: &[u8]) -> Result<ZipFileHeaderFixed, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let result = ZipFileHeaderFixed {
            signature: le_u32(&data[0..4]),
            version_made_by: le_u16(&data[4..6]),
            version_needed: le_u16(&data[6..8]),
            flags: EntryFlags::new(le_u16(&data[8..10])),
            compression_method: CompressionMethod::new(le_u16(&data[10..12])),
            last_mod_time: le_u16(&data[12..14]),
            last_mod_date: le_u16(&data[14..16]),
            crc32: le_u32(&data[16..20]),
            compressed_size: le_u32(&data[20..24]),
            uncompressed_size: le_u32(&data[24..28]),
            file_name_len: le_u16(&data[28..30]),
            extra_field_len: le_u16(&data[30..32]),
            file_comment_len: le_u16(&data[32..34]),
            disk_number_start: le_u16(&data[34..36]),
            internal_file_attrs: le_u16(&data[36..38]),
            external_file_attrs: le_u32(&data[38..42]),
            local_header_offset: le_u32(&data[42..46]),
        };

        if result.signature != CENTRAL_HEADER_SIGNATURE {
            return Err(Error::from(ErrorKind::InvalidSignature {
                expected: CENTRAL_HEADER_SIGNATURE,
                actual: result.signature,
            }));
        }

        Ok(result)
    }

    #[inline]
    fn parse_variable_length<'a>(&self, data: &'a [u8]) -> Option<VariableFields<'a>> {
        if data.len() < self.file_name_len as usize {
            return None;
        }
        let (file_name, rest) = data.split_at(self.file_name_len as usize);

        if rest.len() < self.extra_field_len as usize {
            return None;
        }
        let (extra_field, rest) = rest.split_at(self.extra_field_len as usize);

        if rest.len() < self.file_comment_len as usize {
            return None;
        }
        let (file_comment, rest) = rest.split_at(self.file_comment_len as usize);

        Some((file_name, extra_field, file_comment, rest))
    }

    #[cfg(feature = "std")]
    pub fn write<W>(&self, mut writer: W) -> Result<(), Error>
    where
        W: Write,
    {
        // Batch writes with a fixed size buffer. Improved throughput 25%
        let mut buffer = [0u8; Self::SIZE];
        buffer[0..4].copy_from_slice(&self.signature.to_le_bytes());
        buffer[4..6].copy_from_slice(&self.version_made_by.to_le_bytes());
        buffer[6..8].copy_from_slice(&self.version_needed.to_le_bytes());
        buffer[8..10].copy_from_slice(&self.flags.bits().to_le_bytes());
        buffer[10..12].copy_from_slice(&self.compression_method.0.to_le_bytes());
        buffer[12..14].copy_from_slice(&self.last_mod_time.to_le_bytes());
        buffer[14..16].copy_from_slice(&self.last_mod_date.to_le_bytes());
        buffer[16..20].copy_from_slice(&self.crc32.to_le_bytes());
        buffer[20..24].copy_from_slice(&self.compressed_size.to_le_bytes());
        buffer[24..28].copy_from_slice(&self.uncompressed_size.to_le_bytes());
        buffer[28..30].copy_from_slice(&self.file_name_len.to_le_bytes());
        buffer[30..32].copy_from_slice(&self.extra_field_len.to_le_bytes());
        buffer[32..34].copy_from_slice(&self.file_comment_len.to_le_bytes());
        buffer[34..36].copy_from_slice(&self.disk_number_start.to_le_bytes());
        buffer[36..38].copy_from_slice(&self.internal_file_attrs.to_le_bytes());
        buffer[38..42].copy_from_slice(&self.external_file_attrs.to_le_bytes());
        buffer[42..46].copy_from_slice(&self.local_header_offset.to_le_bytes());
        writer.write_all(&buffer)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_method_display() {
        assert_eq!(CompressionMethod::DEFLATE.to_string(), "8 (DEFLATE)");
        assert_eq!(CompressionMethod::STORE.to_string(), "0 (STORE)");
        assert_eq!(CompressionMethod::new(13).to_string(), "13 (UNKNOWN)");
    }

    #[test]
    pub fn trunc_comment_zips() {
        let data = [
            80, 75, 6, 7, 21, 0, 0, 0, 34, 0, 0, 0, 0, 0, 0, 0, 10, 0, 59, 59, 80, 75, 5, 6, 0,
            255, 255, 255, 255, 255, 255, 0, 0, 0, 80, 75, 6, 6, 0, 0, 0, 10,
        ];
        let archive = ZipArchive::from_slice(data);
        assert!(archive.is_err());
    }

    #[test]
    pub fn trunc_eocd64() {
        let data = [
            80, 75, 6, 7, 21, 0, 0, 0, 34, 0, 0, 0, 0, 0, 0, 0, 10, 0, 59, 59, 80, 75, 5, 6, 0,
            255, 255, 255, 255, 255, 255, 0, 0, 0, 80, 75, 6, 6, 0, 0, 6, 0, 0, 250, 255, 255, 255,
            255, 251, 0, 0, 0, 0, 80, 5, 6, 0, 0, 0, 0, 56, 0, 0, 0, 0, 10,
        ];

        let archive = ZipArchive::from_slice(data);
        assert!(archive.is_err());
    }

    #[test]
    pub fn trunc_eocd_entry() {
        let data = [
            80, 75, 1, 2, 159, 159, 159, 159, 159, 159, 159, 159, 159, 0, 241, 205, 0, 80, 75, 5,
            6, 0, 48, 249, 0, 250, 255, 255, 255, 255, 251, 42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            35, 0,
        ];

        let archive = ZipArchive::from_slice(data).unwrap();
        let mut entries = archive.entries();
        assert!(entries.next_entry().is_err());
    }

    #[test]
    fn data_descriptor_parse_widths_and_signature() {
        // 4-byte sizes, with the optional signature.
        let mut buf = Vec::new();
        buf.extend_from_slice(&DataDescriptor::SIGNATURE.to_le_bytes());
        buf.extend_from_slice(&0xdead_beefu32.to_le_bytes()); // crc
        buf.extend_from_slice(&100u32.to_le_bytes()); // compressed
        buf.extend_from_slice(&200u32.to_le_bytes()); // uncompressed
        let dd = DataDescriptor::parse(&buf, false).unwrap();
        assert_eq!(dd.crc, 0xdead_beef);
        assert_eq!(dd.compressed_size, 100);
        assert_eq!(dd.uncompressed_size, 200);

        // 4-byte sizes, without the signature (crc leads).
        let dd = DataDescriptor::parse(&buf[4..], false).unwrap();
        assert_eq!(dd.crc, 0xdead_beef);
        assert_eq!(dd.compressed_size, 100);
        assert_eq!(dd.uncompressed_size, 200);

        // 8-byte zip64 sizes, with the optional signature.
        let mut buf = Vec::new();
        buf.extend_from_slice(&DataDescriptor::SIGNATURE.to_le_bytes());
        buf.extend_from_slice(&0x0102_0304u32.to_le_bytes()); // crc
        buf.extend_from_slice(&0x1_0000_0000u64.to_le_bytes()); // compressed
        buf.extend_from_slice(&0x2_0000_0000u64.to_le_bytes()); // uncompressed
        let dd = DataDescriptor::parse(&buf, true).unwrap();
        assert_eq!(dd.crc, 0x0102_0304);
        assert_eq!(dd.compressed_size, 0x1_0000_0000);
        assert_eq!(dd.uncompressed_size, 0x2_0000_0000);

        // A buffer too short for the chosen width is rejected.
        assert!(DataDescriptor::parse(&buf[..12], true).is_err());
    }

    #[test]
    fn data_descriptor_width_ignores_offset_only_zip64() {
        // The sizes are small, but the local header offset overflowed past
        // 4 GiB, so the entry carries a zip64 extra field for the offset alone.
        // The descriptor's size width must stay 4-byte (false), derived from the
        // inline size sentinels rather than the offset-driven zip64 field.
        let header = ZipFileHeaderFixed {
            signature: 0x0201_4b50,
            version_made_by: 0,
            version_needed: 0,
            flags: EntryFlags::new(0x08),
            compression_method: CompressionMethod::STORE,
            last_mod_time: 0,
            last_mod_date: 0,
            crc32: 0,
            compressed_size: 100,
            uncompressed_size: 200,
            file_name_len: 0,
            extra_field_len: 12,
            file_comment_len: 0,
            disk_number_start: 0,
            internal_file_attrs: 0,
            external_file_attrs: 0,
            local_header_offset: u32::MAX,
        };

        // A zip64 extra field carrying only the 8-byte local header offset.
        let mut extra = Vec::new();
        extra.extend_from_slice(&ExtraFieldId::ZIP64.as_u16().to_le_bytes());
        extra.extend_from_slice(&8u16.to_le_bytes());
        extra.extend_from_slice(&0x1_0000_0000u64.to_le_bytes());

        let record = ZipFileHeaderRecord::from_parts(header, &[], &extra, &[], 0);
        assert!(record.is_zip64);
        assert_eq!(record.local_header_offset, 0x1_0000_0000);
        assert!(!record.data_descriptor_uses_zip64_sizes);
        assert!(!record.wayfinder().data_descriptor_uses_zip64_sizes);
    }
}
