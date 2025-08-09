use crate::crc::crc32_chunk;
use crate::errors::{Error, ErrorKind};
use crate::mode::{
    msdos_mode_to_file_mode, unix_mode_to_file_mode, EntryMode, CREATOR_FAT, CREATOR_MACOS,
    CREATOR_NTFS, CREATOR_UNIX, CREATOR_VFAT,
};
use crate::path::{RawPath, ZipFilePath};
use crate::reader_at::{FileReader, MutexReader, ReaderAtExt};
use crate::time::{extract_best_timestamp, ZipDateTimeKind};
use crate::utils::{le_u16, le_u32, le_u64};
use crate::{EndOfCentralDirectoryRecord, EndOfCentralDirectoryRecordFixed, ReaderAt, ZipLocator};
use std::io::{Read, Seek, Write};
use std::num::NonZeroU64;

pub(crate) const END_OF_CENTRAL_DIR_SIGNATURE64: u32 = 0x06064b50;
pub(crate) const END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE: u32 = 0x07064b50;
pub(crate) const CENTRAL_HEADER_SIGNATURE: u32 = 0x02014b50;
/// The recommended buffer size to use when reading from a zip file.
///
/// This buffer size was chosen as it can hold an entire central directory
/// record as the spec states (4.4.10):
///
/// > the combined length of any directory and these three fields SHOULD NOT
/// > generally exceed 65,535 bytes.
pub const RECOMMENDED_BUFFER_SIZE: usize = 1 << 16;

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
///         println!("File: {}", entry.file_path().try_normalize()?.as_ref());
///     }
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ZipSliceArchive<T: AsRef<[u8]>> {
    data: T,
    eocd: EndOfCentralDirectory,
}

impl<T: AsRef<[u8]>> ZipSliceArchive<T> {
    pub(crate) fn new(data: T, eocd: EndOfCentralDirectory) -> Self {
        ZipSliceArchive { data, eocd }
    }

    /// Returns an iterator over the entries in the central directory of the archive.
    pub fn entries(&self) -> ZipSliceEntries {
        let data = self.data.as_ref();
        let entry_data =
            &data[(self.eocd.directory_offset() as usize)..self.eocd.head_eocd_offset() as usize];
        ZipSliceEntries {
            entry_data,
            base_offset: self.eocd.base_offset(),
        }
    }

    /// Returns the byte slice that represents the zip file.
    ///
    /// This will include the entire input slice.
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_ref()
    }

    /// Returns a hint for the total number of entries in the archive.
    ///
    /// This value is read from the End of Central Directory record.
    pub fn entries_hint(&self) -> u64 {
        self.eocd.entries()
    }

    /// Returns the offset of the End of Central Directory (EOCD) signature.
    ///
    /// See [`ZipSliceArchive::eocd_offset()`] for more details.
    pub fn eocd_offset(&self) -> u64 {
        self.eocd.tail_eocd_offset()
    }

    /// The comment of the zip file.
    pub fn comment(&self) -> ZipStr {
        let data = self.data.as_ref();
        let comment_start =
            self.eocd.tail_eocd_offset() as usize + EndOfCentralDirectoryRecordFixed::SIZE;
        let remaining = &data[comment_start..];
        let comment_len = self.eocd.comment_len();
        ZipStr::new(&remaining[..(comment_len).min(remaining.len())])
    }

    /// Converts the [`ZipSliceArchive`] into a general [`ZipArchive`].
    ///
    /// This is useful for unifying code that might handle both slice-based
    /// and reader-based archives.
    pub fn into_reader(self) -> ZipArchive<T> {
        let comment = self.comment().into_owned();
        ZipArchive {
            reader: self.data,
            comment,
            eocd: self.eocd,
        }
    }

    /// Retrieves a specific entry from the archive by its [`ZipArchiveEntryWayfinder`].
    ///
    /// A wayfinder can be obtained when iterating through the central directory
    ///
    /// # Errors
    ///
    /// Returns an `Error` if the entry cannot be found or read, or if the
    /// archive is malformed.
    pub fn get_entry(&self, entry: ZipArchiveEntryWayfinder) -> Result<ZipSliceEntry, Error> {
        let data = self.data.as_ref();
        let header = &data[(entry.local_header_offset as usize).min(data.len())..];
        let file_header = ZipLocalFileHeaderFixed::parse(header)?;
        let header = &header[ZipLocalFileHeaderFixed::SIZE..];

        let variable_length = file_header.variable_length();
        let rest = header
            .get(variable_length..)
            .ok_or(Error::from(ErrorKind::Eof))?;

        let (data, rest) = if rest.len() < entry.compressed_size_hint() as usize {
            return Err(Error::from(ErrorKind::Eof));
        } else {
            rest.split_at(entry.compressed_size_hint() as usize)
        };

        let expected_crc = if entry.has_data_descriptor {
            DataDescriptor::parse(rest)?.crc
        } else {
            entry.crc
        };

        let data_start_offset = entry.local_header_offset
            + ZipLocalFileHeaderFixed::SIZE as u64
            + file_header.variable_length() as u64;

        debug_assert!(std::ptr::eq(
            data.as_ptr(),
            self.data.as_ref()[data_start_offset as usize..].as_ptr()
        ));

        Ok(ZipSliceEntry {
            data,
            verifier: ZipVerification {
                crc: expected_crc,
                uncompressed_size: entry.uncompressed_size_hint(),
            },
            data_start_offset,
        })
    }
}

/// Represents a single entry (file or directory) within a `ZipSliceArchive`.
///
/// It provides access to the raw compressed data of the entry.
#[derive(Debug, Clone)]
pub struct ZipSliceEntry<'a> {
    data: &'a [u8],
    verifier: ZipVerification,
    data_start_offset: u64,
}

impl<'a> ZipSliceEntry<'a> {
    /// Returns the raw, compressed data of the entry as a byte slice.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Returns a verifier for the CRC and uncompressed size of the entry.
    ///
    /// Useful when it's more practical to oneshot decompress the data,
    /// otherwise use [`ZipSliceEntry::verifying_reader`] to stream
    /// decompression and verification.
    pub fn claim_verifier(&self) -> ZipVerification {
        self.verifier
    }

    /// Returns a reader that wraps a decompressor and verify the size and CRC
    /// of the decompressed data once finished.
    pub fn verifying_reader<D>(&self, reader: D) -> ZipSliceVerifier<D>
    where
        D: std::io::Read,
    {
        ZipSliceVerifier {
            reader,
            verifier: self.verifier,
            crc: 0,
            size: 0,
        }
    }

    /// Returns the byte range of the compressed data within the archive.
    ///
    /// See [`ZipEntry::compressed_data_range`] for more details.
    pub fn compressed_data_range(&self) -> (u64, u64) {
        (
            self.data_start_offset,
            self.data_start_offset + self.data.len() as u64,
        )
    }
}

/// Verifies the wrapped reader returns the expected CRC and uncompressed size
#[derive(Debug, Clone)]
pub struct ZipSliceVerifier<D> {
    reader: D,
    crc: u32,
    size: u64,
    verifier: ZipVerification,
}

impl<D> ZipSliceVerifier<D> {
    /// Consumes the `ZipSliceVerifier`, returning the underlying reader.
    pub fn into_inner(self) -> D {
        self.reader
    }
}

impl<D> std::io::Read for ZipSliceVerifier<D>
where
    D: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.reader.read(buf)?;
        self.crc = crc32_chunk(&buf[..read], self.crc);
        self.size += read as u64;

        if read == 0 || self.size >= self.verifier.size() {
            self.verifier
                .valid(ZipVerification {
                    crc: self.crc,
                    uncompressed_size: self.size,
                })
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        }

        Ok(read)
    }
}

/// An iterator over the central directory file header records.
///
/// Created from [`ZipSliceArchive::entries`].
#[derive(Debug, Clone)]
pub struct ZipSliceEntries<'data> {
    entry_data: &'data [u8],
    base_offset: u64,
}

impl<'data> ZipSliceEntries<'data> {
    /// Yield the next zip file entry in the central directory if there is any
    #[inline]
    pub fn next_entry(&mut self) -> Result<Option<ZipFileHeaderRecord<'data>>, Error> {
        if self.entry_data.is_empty() {
            return Ok(None);
        }

        let file_header = ZipFileHeaderFixed::parse(self.entry_data)?;
        self.entry_data = &self.entry_data[ZipFileHeaderFixed::SIZE..];
        let Some((file_name, extra_field, file_comment, entry_data)) =
            file_header.parse_variable_length(self.entry_data)
        else {
            return Err(Error::from(ErrorKind::Eof));
        };

        let mut entry =
            ZipFileHeaderRecord::from_parts(file_header, file_name, extra_field, file_comment);
        entry.local_header_offset += self.base_offset;
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
/// It can be created from a slice, a file, or any `Read + Seek` source.
///
/// # Examples
///
/// Creating from a file:
///
/// ```rust
/// # use rawzip::{ZipArchive, Error, RECOMMENDED_BUFFER_SIZE};
/// # use std::fs::File;
/// # use std::io;
/// fn example_from_file(file: File) -> Result<(), Error> {
///     let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
///     let archive = ZipArchive::from_file(file, &mut buffer)?;
///     Ok(())
/// }
/// ```
///
/// For more complex use cases, use the [`ZipLocator`] to locate an archive.
#[derive(Debug, Clone)]
pub struct ZipArchive<R> {
    reader: R,
    comment: ZipString,
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

    /// Parses an archive from a file by reading the End of Central Directory.
    ///
    /// A buffer is required to read parts of the file.
    /// [`RECOMMENDED_BUFFER_SIZE`] can be used to construct this buffer.
    pub fn from_file(
        file: std::fs::File,
        buffer: &mut [u8],
    ) -> Result<ZipArchive<FileReader>, Error> {
        ZipLocator::new()
            .locate_in_file(file, buffer)
            .map_err(|(_, e)| e)
    }

    /// Parses an archive from a seekable reader.
    ///
    /// Prefer [`ZipArchive::from_file`] and [`ZipArchive::from_slice`] when
    /// possible, as they are more efficient due to not wrapping the underlying
    /// reader in a mutex to support positioned io.
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, Error, RECOMMENDED_BUFFER_SIZE, ZipFileHeaderRecord};
    /// # use std::io::Cursor;
    /// fn example(zip_data: &[u8]) -> Result<(), Error> {
    ///     let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    ///     let archive = ZipArchive::from_seekable(Cursor::new(zip_data), &mut buffer)?;
    ///     Ok(())
    /// }
    /// ```
    pub fn from_seekable<R>(
        mut reader: R,
        buffer: &mut [u8],
    ) -> Result<ZipArchive<MutexReader<R>>, Error>
    where
        R: Read + Seek,
    {
        let end_offset = reader.seek(std::io::SeekFrom::End(0))?;
        let reader = MutexReader::new(reader);
        ZipLocator::new()
            .locate_in_reader(reader, buffer, end_offset)
            .map_err(|(_, e)| e)
    }
}

impl<R> ZipArchive<R> {
    pub(crate) fn new(reader: R, eocd: EndOfCentralDirectory, comment: ZipString) -> Self {
        ZipArchive {
            reader,
            eocd,
            comment,
        }
    }

    /// Returns a reference to the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.reader
    }

    /// Returns a lending iterator over the entries in the central directory of
    /// the archive.
    ///
    /// Requires a mutable buffer to read directory entries from the underlying
    /// reader.
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, Error, RECOMMENDED_BUFFER_SIZE, ZipFileHeaderRecord};
    /// # use std::fs::File;
    /// fn example(file: File) -> Result<(), Error> {
    ///     let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    ///     let archive = ZipArchive::from_file(file, &mut buffer)?;
    ///     let entries_hint = archive.entries_hint();
    ///     let mut actual_entries = 0;
    ///     let mut entries_iterator = archive.entries(&mut buffer);
    ///     while let Some(_) = entries_iterator.next_entry()? {
    ///         actual_entries += 1;
    ///     }
    ///     println!("Found {} entries (hint: {})", actual_entries, entries_hint);
    ///     Ok(())
    /// }
    /// ```
    pub fn entries<'archive, 'buf>(
        &'archive self,
        buffer: &'buf mut [u8],
    ) -> ZipEntries<'archive, 'buf, R> {
        ZipEntries {
            buffer,
            archive: self,
            pos: 0,
            end: 0,
            offset: self.eocd.directory_offset(),
            base_offset: self.eocd.base_offset(),
            central_dir_end_pos: self.eocd.head_eocd_offset(),
        }
    }

    /// Returns a hint for the total number of entries in the archive.
    ///
    /// This value is read from the End of Central Directory record.
    pub fn entries_hint(&self) -> u64 {
        self.eocd.entries()
    }

    /// Returns the comment of the zip archive, if any.
    pub fn comment(&self) -> ZipStr {
        self.comment.as_str()
    }

    /// Returns the offset of the End of Central Directory (EOCD) signature.
    ///
    /// This is the byte position where the EOCD signature (0x06054b50) was found.
    /// Useful for recovery scenarios when dealing with false EOCD signatures or
    /// when restarting archive searches from a known position.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, ZipLocator, RECOMMENDED_BUFFER_SIZE};
    /// # use std::fs::File;
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let file = File::open("assets/test.zip")?;
    /// # let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    /// let archive = ZipArchive::from_file(file, &mut buffer)?;
    /// let eocd_position = archive.eocd_offset();
    ///
    /// let locator = ZipLocator::new();
    /// let reader = archive.get_ref();
    /// let maybe_previous = locator.locate_in_reader(reader, &mut buffer, eocd_position);
    /// # Ok(())
    /// # }
    /// ```
    pub fn eocd_offset(&self) -> u64 {
        self.eocd.tail_eocd_offset()
    }
}

impl<R> ZipArchive<R>
where
    R: ReaderAt,
{
    /// Retrieves a specific entry from the archive by a wayfinder.
    pub fn get_entry(&self, entry: ZipArchiveEntryWayfinder) -> Result<ZipEntry<'_, R>, Error> {
        let mut buffer = [0u8; ZipLocalFileHeaderFixed::SIZE];
        self.reader
            .read_exact_at(&mut buffer, entry.local_header_offset)?;

        // The central directory is the source of truth so we really only parse
        // out the local file header to verify the signature and understand the
        // variable length. Not everyone uses this as the source of truth:
        // https://labs.redyops.com/index.php/2020/04/30/spending-a-night-reading-the-zip-file-format-specification/
        let file_header = ZipLocalFileHeaderFixed::parse(&buffer)?;
        let (body_offset, o1) = entry
            .local_header_offset
            .overflowing_add(ZipLocalFileHeaderFixed::SIZE as u64);
        let (body_offset, o2) = body_offset.overflowing_add(file_header.variable_length() as u64);
        let (body_end_offset, o3) = body_offset.overflowing_add(entry.compressed_size);

        if o1 || o2 || o3 {
            return Err(Error::from(ErrorKind::Eof));
        }

        Ok(ZipEntry {
            archive: self,
            entry,
            body_offset,
            body_end_offset,
        })
    }
}

/// Represents a single entry (file or directory) within a [`ZipArchive`]
#[derive(Debug, Clone)]
pub struct ZipEntry<'archive, R> {
    archive: &'archive ZipArchive<R>,
    body_offset: u64,
    body_end_offset: u64,
    entry: ZipArchiveEntryWayfinder,
}

impl<'archive, R> ZipEntry<'archive, R>
where
    R: ReaderAt,
{
    /// Returns a [`ZipReader`] for reading the compressed data of this entry.
    pub fn reader(&self) -> ZipReader<'archive, R> {
        ZipReader {
            archive: self.archive,
            entry: self.entry,
            offset: self.body_offset,
            end_offset: self.body_end_offset,
        }
    }

    /// Returns a reader that wraps a decompressor and verify the size and CRC
    /// of the decompressed data once finished.
    pub fn verifying_reader<D>(&self, reader: D) -> ZipVerifier<'archive, D, R>
    where
        D: std::io::Read,
    {
        ZipVerifier {
            reader,
            crc: 0,
            size: 0,
            archive: self.archive,
            end_offset: self.body_end_offset,
            wayfinder: self.entry,
        }
    }

    /// Returns a tuple of start and end byte offsets for the compressed data
    /// within the underlying reader.
    ///
    /// This method uses the information from the local file header in its
    /// calculations.
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
        (self.body_offset, self.body_end_offset)
    }
}

/// Holds the expected CRC32 checksum and uncompressed size for a Zip entry.
///
/// This struct is used to verify the integrity of decompressed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipVerification {
    pub crc: u32,
    pub uncompressed_size: u64,
}

impl ZipVerification {
    /// Returns the expected CRC32 checksum.
    pub fn crc(&self) -> u32 {
        self.crc
    }

    /// Returns the expected uncompressed size.
    pub fn size(&self) -> u64 {
        self.uncompressed_size
    }

    /// Validates the size and CRC of the entry.
    ///
    /// This function will return an error if the size or CRC does not match
    /// the expected values.
    pub fn valid(&self, rhs: ZipVerification) -> Result<(), Error> {
        if self.size() != rhs.size() {
            return Err(Error::from(ErrorKind::InvalidSize {
                expected: self.size(),
                actual: rhs.size(),
            }));
        }

        // If the CRC is 0, then it is not verified.
        if self.crc() != 0 && self.crc() != rhs.crc() {
            return Err(Error::from(ErrorKind::InvalidChecksum {
                expected: self.crc(),
                actual: rhs.crc(),
            }));
        }

        Ok(())
    }
}

/// Verifies the checksum of the decompressed data matches the checksum listed in the zip
#[derive(Debug, Clone)]
pub struct ZipVerifier<'archive, Decompressor, ReaderAt> {
    reader: Decompressor,
    crc: u32,
    size: u64,
    archive: &'archive ZipArchive<ReaderAt>,
    end_offset: u64,
    wayfinder: ZipArchiveEntryWayfinder,
}

impl<Decompressor, ReaderAt> ZipVerifier<'_, Decompressor, ReaderAt> {
    /// Consumes the [`ZipVerifier`], returning the underlying decompressor.
    pub fn into_inner(self) -> Decompressor {
        self.reader
    }
}

impl<Decompressor, Reader> std::io::Read for ZipVerifier<'_, Decompressor, Reader>
where
    Decompressor: std::io::Read,
    Reader: ReaderAt,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.reader.read(buf)?;
        self.crc = crc32_chunk(&buf[..read], self.crc);
        self.size += read as u64;

        if read == 0 || self.size >= self.wayfinder.uncompressed_size_hint() {
            let crc = if self.wayfinder.has_data_descriptor {
                DataDescriptor::read_at(&self.archive.reader, self.end_offset).map(|x| x.crc)
            } else {
                Ok(self.crc)
            };

            crc.and_then(|crc| {
                let expected = ZipVerification {
                    crc: self.crc,
                    uncompressed_size: self.wayfinder.uncompressed_size_hint(),
                };

                expected.valid(ZipVerification {
                    crc,
                    uncompressed_size: self.size,
                })
            })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        }

        Ok(read)
    }
}

/// A reader for a Zip entry's compressed data.
#[derive(Debug, Clone)]
pub struct ZipReader<'archive, R> {
    archive: &'archive ZipArchive<R>,
    entry: ZipArchiveEntryWayfinder,
    offset: u64,
    end_offset: u64,
}

impl<R> ZipReader<'_, R>
where
    R: ReaderAt,
{
    /// Returns an object that can be used to verify the size and checksum of
    /// inflated data
    ///
    /// Consumes the reader, so this should be called after all data has been read from the entry.
    ///
    /// The function will read the data descriptor if one is expected to exist.
    pub fn claim_verifier(self) -> Result<ZipVerification, Error> {
        let expected_size = self.entry.uncompressed_size_hint();

        let expected_crc = if self.entry.has_data_descriptor {
            DataDescriptor::read_at(&self.archive.reader, self.end_offset).map(|x| x.crc)?
        } else {
            self.entry.crc
        };

        Ok(ZipVerification {
            crc: expected_crc,
            uncompressed_size: expected_size,
        })
    }
}

impl<R> Read for ZipReader<'_, R>
where
    R: ReaderAt,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read_size = buf.len().min((self.end_offset - self.offset) as usize);
        let read = self
            .archive
            .reader
            .read_at(&mut buf[..read_size], self.offset)?;
        self.offset += read as u64;
        Ok(read)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DataDescriptor {
    crc: u32,
}

impl DataDescriptor {
    const SIZE: usize = 8;
    pub const SIGNATURE: u32 = 0x08074b50;

    fn parse(data: &[u8]) -> Result<DataDescriptor, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let mut pos = 0;

        let potential_signature = le_u32(&data[0..4]);
        if potential_signature == Self::SIGNATURE {
            pos += 4;
        }

        // The crc is followed by the compressed_size and then the
        // uncompressed_size but the spec allows for the sizes to be either 4
        // bytes each or 8 bytes in Zip64 mode. (spec 4.3.9.1). They aren't
        // needed, so we skip them.
        Ok(DataDescriptor {
            crc: le_u32(&data[pos..pos + 4]),
        })
    }

    fn read_at<R>(reader: R, offset: u64) -> Result<DataDescriptor, Error>
    where
        R: ReaderAt,
    {
        let mut buffer = [0u8; Self::SIZE];
        reader.read_exact_at(&mut buffer, offset)?;
        Self::parse(&buffer)
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

    /// the start of the zip file proper.
    #[inline]
    fn base_offset(&self) -> u64 {
        match self.zip64_eocd_offset {
            Some(_) => 0,
            None => {
                self.eocd_offset
                    .saturating_sub(self.central_dir_size)
                    .saturating_sub(self.central_dir_offset)

                // In the case that the base_offset is calculated to be non-zero
                // Go's zip reader will check if base_offset of zero would
                // correspond to a valid directory header and if so, set it to
                // zero anyways.
                // https://github.com/golang/go/blob/c0e149b6b1aa2daca64c00804809bc2279e21eee/src/archive/zip/reader.go#L636
                //
                // Neither rc-zip or rust's zip crate can handle the file so we
                // don't either
                //
                // See Go's test-badbase.zip and test-baddirsz.zip for test cases
            }
        }
    }

    /// The first end of the central directory signature offsets.
    ///
    /// This is offset where no new central directory records are expected.
    ///
    /// Will be equivalent to [`Self::tail_eocd_offset`] eocd for non-zip64 files
    #[inline]
    fn head_eocd_offset(&self) -> u64 {
        self.zip64_eocd_offset
            .map(|x| x.get())
            .unwrap_or(self.eocd_offset)
    }

    /// The last end of the central directory signature offsets.
    ///
    /// This will always be the byte offset of 0x06054b50
    #[inline]
    fn tail_eocd_offset(&self) -> u64 {
        self.eocd_offset
    }

    /// offset of the start of the central directory
    #[inline]
    fn directory_offset(&self) -> u64 {
        self.base_offset() + self.central_dir_offset
    }

    #[inline]
    fn entries(&self) -> u64 {
        self.num_entries
    }

    #[inline]
    fn comment_len(&self) -> usize {
        self.comment_len as usize
    }
}

/// A lending iterator over file header records in a [`ZipArchive`].
#[derive(Debug)]
pub struct ZipEntries<'archive, 'buf, R> {
    buffer: &'buf mut [u8],
    archive: &'archive ZipArchive<R>,
    pos: usize,
    end: usize,
    offset: u64,
    base_offset: u64,
    central_dir_end_pos: u64,
}

impl<R> ZipEntries<'_, '_, R>
where
    R: ReaderAt,
{
    /// Yield the next zip file entry in the central directory if there is any
    ///
    /// This method reads from the underlying archive reader into the provided
    /// buffer to parse entry headers.
    #[inline]
    pub fn next_entry(&mut self) -> Result<Option<ZipFileHeaderRecord>, Error> {
        if self.pos + ZipFileHeaderFixed::SIZE >= self.end {
            if self.offset >= self.central_dir_end_pos {
                return Ok(None);
            }

            let remaining = self.end - self.pos;
            self.buffer.copy_within(self.pos..self.end, 0);
            let max_read = ((self.central_dir_end_pos - self.offset) as usize)
                .min(self.buffer.len() - remaining);
            let read = self.archive.reader.read_at_least_at(
                &mut self.buffer[remaining..][..max_read],
                ZipFileHeaderFixed::SIZE,
                self.offset,
            )?;
            self.offset += read as u64;
            self.pos = 0;
            self.end = remaining + read;
        }

        let data = &self.buffer[self.pos..self.end];
        let file_header = ZipFileHeaderFixed::parse(data)?;
        self.pos += ZipFileHeaderFixed::SIZE;

        let variable_length = file_header.variable_length();
        if self.pos + variable_length > self.end {
            // Need to read more data
            let remaining = self.end - self.pos;
            self.buffer.copy_within(self.pos..self.end, 0);
            let max_read = ((self.central_dir_end_pos - self.offset) as usize)
                .min(self.buffer.len() - remaining);
            let read = self.archive.reader.read_at_least_at(
                &mut self.buffer[remaining..][..max_read],
                variable_length - remaining,
                self.offset,
            )?;
            self.offset += read as u64;
            self.pos = 0;
            self.end = remaining + read;
        }

        let data = &self.buffer[self.pos..self.end];
        let (file_name, extra_field, file_comment, _) = file_header
            .parse_variable_length(data)
            .expect("variable length precheck failed");
        let mut file_header =
            ZipFileHeaderRecord::from_parts(file_header, file_name, extra_field, file_comment);
        file_header.local_header_offset += self.base_offset;
        self.pos += variable_length;
        Ok(Some(file_header))
    }
}

/// 4.4.2
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VersionMadeBy(u16);

#[allow(dead_code)]
impl VersionMadeBy {
    pub fn as_u16(&self) -> u16 {
        self.0
    }

    /// The (major, minor) ZIP specification version supported by the software
    /// used to encode the file.
    ///
    /// 4.4.2.3: The lower byte, The value / 10 indicates the major version
    /// number, and the value mod 10 is the minor version number.
    pub fn version(&self) -> (u8, u8) {
        let v = (self.0 >> 8) as u8;
        (v / 10, v % 10)
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
            version_made_by: VersionMadeBy(le_u16(&data[12..14])),
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

/// A numeric identifier for a compression method used in a Zip archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionMethodId(u16);

impl CompressionMethodId {
    /// Returns the raw `u16` value of the compression method ID.
    #[inline]
    pub fn as_u16(&self) -> u16 {
        self.0
    }

    /// Converts the numeric ID to a `CompressionMethod` enum.
    #[inline]
    pub fn as_method(&self) -> CompressionMethod {
        match self.0 {
            0 => CompressionMethod::Store,
            1 => CompressionMethod::Shrunk,
            2 => CompressionMethod::Reduce1,
            3 => CompressionMethod::Reduce2,
            4 => CompressionMethod::Reduce3,
            5 => CompressionMethod::Reduce4,
            6 => CompressionMethod::Imploded,
            7 => CompressionMethod::Tokenizing,
            8 => CompressionMethod::Deflate,
            9 => CompressionMethod::Deflate64,
            10 => CompressionMethod::Terse,
            12 => CompressionMethod::Bzip2,
            14 => CompressionMethod::Lzma,
            18 => CompressionMethod::Lz77,
            20 => CompressionMethod::ZstdDeprecated,
            93 => CompressionMethod::Zstd,
            94 => CompressionMethod::Mp3,
            95 => CompressionMethod::Xz,
            96 => CompressionMethod::Jpeg,
            97 => CompressionMethod::WavPack,
            98 => CompressionMethod::Ppmd,
            99 => CompressionMethod::Aes,
            _ => CompressionMethod::Unknown(self.0),
        }
    }
}

/// The compression method used on an individual Zip archive entry
///
/// Documented in the spec under: 4.4.5
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum CompressionMethod {
    Store = 0,
    Shrunk = 1,
    Reduce1 = 2,
    Reduce2 = 3,
    Reduce3 = 4,
    Reduce4 = 5,
    Imploded = 6,
    Tokenizing = 7,
    Deflate = 8,
    Deflate64 = 9,
    Terse = 10,
    Bzip2 = 12,
    Lzma = 14,
    Lz77 = 18,
    ZstdDeprecated = 20,
    Zstd = 93,
    Mp3 = 94,
    Xz = 95,
    Jpeg = 96,
    WavPack = 97,
    Ppmd = 98,
    Aes = 99,
    Unknown(u16),
}

impl CompressionMethod {
    /// Return the numeric id of this compression method.
    #[inline]
    pub fn as_id(&self) -> CompressionMethodId {
        let value = match self {
            CompressionMethod::Store => 0,
            CompressionMethod::Shrunk => 1,
            CompressionMethod::Reduce1 => 2,
            CompressionMethod::Reduce2 => 3,
            CompressionMethod::Reduce3 => 4,
            CompressionMethod::Reduce4 => 5,
            CompressionMethod::Imploded => 6,
            CompressionMethod::Tokenizing => 7,
            CompressionMethod::Deflate => 8,
            CompressionMethod::Deflate64 => 9,
            CompressionMethod::Terse => 10,
            CompressionMethod::Bzip2 => 12,
            CompressionMethod::Lzma => 14,
            CompressionMethod::Lz77 => 18,
            CompressionMethod::ZstdDeprecated => 20,
            CompressionMethod::Zstd => 93,
            CompressionMethod::Mp3 => 94,
            CompressionMethod::Xz => 95,
            CompressionMethod::Jpeg => 96,
            CompressionMethod::WavPack => 97,
            CompressionMethod::Ppmd => 98,
            CompressionMethod::Aes => 99,
            CompressionMethod::Unknown(id) => *id,
        };
        CompressionMethodId(value)
    }
}

impl From<u16> for CompressionMethod {
    fn from(id: u16) -> Self {
        CompressionMethodId(id).as_method()
    }
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
    #[inline]
    pub fn into_owned(&self) -> ZipString {
        ZipString::new(self.0.to_vec())
    }
}

/// An owned string (`Vec<u8>`) from a Zip archive, typically for comments or non-path text.
///
/// Similar to `ZipStr`, but owns its data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ZipString(Vec<u8>);

impl ZipString {
    /// Creates a new `ZipString` from a vector of bytes.
    #[inline]
    pub fn new(data: Vec<u8>) -> Self {
        Self(data)
    }

    /// Returns a borrowed `ZipStr` view of this `ZipString`.
    #[inline]
    pub fn as_str(&self) -> ZipStr {
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
    flags: u16,
    compression_method: CompressionMethodId,
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
    file_name: ZipFilePath<RawPath<'a>>,
    extra_field: &'a [u8],
    file_comment: ZipStr<'a>,
    is_zip64: bool,
}

impl<'a> ZipFileHeaderRecord<'a> {
    #[inline]
    fn from_parts(
        header: ZipFileHeaderFixed,
        file_name: &'a [u8],
        extra_field: &'a [u8],
        file_comment: &'a [u8],
    ) -> Self {
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
            file_name: ZipFilePath::from_bytes(file_name),
            extra_field,
            file_comment: ZipStr::new(file_comment),
            is_zip64: false,
        };

        if result.uncompressed_size != u64::from(u32::MAX)
            && result.compressed_size != u64::from(u32::MAX)
            && result.local_header_offset != u64::from(u32::MAX)
            && result.disk_number_start != u32::from(u16::MAX)
        {
            return result;
        }

        let mut extra_fields = extra_field;

        loop {
            let Some(kind) = extra_fields.get(0..2).map(le_u16) else {
                break;
            };

            let Some(size) = extra_fields.get(2..4).map(le_u16) else {
                break;
            };

            extra_fields = &extra_fields[4..];
            let end_pos = (size as usize).min(extra_fields.len());
            let (mut field, rest) = extra_fields.split_at(end_pos);
            extra_fields = rest;

            const ZIP64_EXTRA_FIELD: u16 = 0x0001;
            if kind != ZIP64_EXTRA_FIELD {
                continue;
            }

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

    /// Returns true if the entry has a data descriptor that follows its
    /// compressed data.
    ///
    /// From the spec (4.3.9.1):
    ///
    /// > This descriptor MUST exist if bit 3 of the general purpose bit flag is
    /// > set
    #[inline]
    pub fn has_data_descriptor(&self) -> bool {
        self.flags & 0x08 != 0
    }

    /// Describes where the file's data is located within the archive.
    #[inline]
    pub fn wayfinder(&self) -> ZipArchiveEntryWayfinder {
        ZipArchiveEntryWayfinder {
            uncompressed_size: self.uncompressed_size,
            compressed_size: self.compressed_size,
            local_header_offset: self.local_header_offset,
            has_data_descriptor: self.has_data_descriptor(),
            crc: self.crc32,
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

    /// The offset to the local file header within the Zip archive.
    #[inline]
    pub fn local_header_offset(&self) -> u64 {
        self.local_header_offset
    }

    /// The compression method used to compress the data
    #[inline]
    pub fn compression_method(&self) -> CompressionMethod {
        self.compression_method.as_method()
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
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let data = include_bytes!("../assets/test.zip");
    /// # let archive = ZipArchive::from_slice(data)?;
    /// # let mut entries = archive.entries();
    /// # let entry = entries.next_entry()?.unwrap();
    /// // Get raw path (potentially unsafe)
    /// let raw_path = entry.file_path();
    ///
    /// // Convert to safe path
    /// let safe_path = raw_path.try_normalize()?;
    /// println!("Safe path: {}", safe_path.as_ref());
    ///
    /// // Check if it's a directory
    /// if safe_path.is_dir() {
    ///     println!("This is a directory");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn file_path(&self) -> ZipFilePath<RawPath<'a>> {
        self.file_name
    }

    /// Returns the last modification date and time.
    ///
    /// This method parses the extra field data to locate more accurate timestamps.
    #[inline]
    pub fn last_modified(&self) -> ZipDateTimeKind {
        extract_best_timestamp(self.extra_field, self.last_mod_time, self.last_mod_date)
    }

    /// Returns the file mode information extracted from the external file attributes.
    #[inline]
    pub fn mode(&self) -> EntryMode {
        let creator_version = self.version_made_by >> 8;

        let mut mode = match creator_version {
            // Unix and macOS
            CREATOR_UNIX | CREATOR_MACOS => unix_mode_to_file_mode(self.external_file_attrs >> 16),
            // NTFS, VFAT, FAT
            CREATOR_NTFS | CREATOR_VFAT | CREATOR_FAT => {
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
}

/// Contains directions to where the Zip entry's data is located within the Zip archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipArchiveEntryWayfinder {
    uncompressed_size: u64,
    compressed_size: u64,
    local_header_offset: u64,
    crc: u32,
    has_data_descriptor: bool,
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

#[derive(Debug, Clone)]
pub(crate) struct ZipLocalFileHeaderFixed {
    pub(crate) signature: u32,
    pub(crate) version_needed: u16,
    pub(crate) flags: u16,
    pub(crate) compression_method: CompressionMethodId,
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
            flags: le_u16(&data[6..8]),
            compression_method: CompressionMethodId(le_u16(&data[8..10])),
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

    pub fn write<W>(&self, mut writer: W) -> Result<(), Error>
    where
        W: Write,
    {
        writer.write_all(&self.signature.to_le_bytes())?;
        writer.write_all(&self.version_needed.to_le_bytes())?;
        writer.write_all(&self.flags.to_le_bytes())?;
        writer.write_all(&self.compression_method.0.to_le_bytes())?;
        writer.write_all(&self.last_mod_time.to_le_bytes())?;
        writer.write_all(&self.last_mod_date.to_le_bytes())?;
        writer.write_all(&self.crc32.to_le_bytes())?;
        writer.write_all(&self.compressed_size.to_le_bytes())?;
        writer.write_all(&self.uncompressed_size.to_le_bytes())?;
        writer.write_all(&self.file_name_len.to_le_bytes())?;
        writer.write_all(&self.extra_field_len.to_le_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ZipFileHeaderFixed {
    pub signature: u32,
    pub version_made_by: u16,
    pub version_needed: u16,
    pub flags: u16,
    pub compression_method: CompressionMethodId,
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
    const SIZE: usize = 46;

    #[inline]
    pub fn parse(data: &[u8]) -> Result<ZipFileHeaderFixed, Error> {
        if data.len() < Self::SIZE {
            return Err(Error::from(ErrorKind::Eof));
        }

        let result = ZipFileHeaderFixed {
            signature: le_u32(&data[0..4]),
            version_made_by: le_u16(&data[4..6]),
            version_needed: le_u16(&data[6..8]),
            flags: le_u16(&data[8..10]),
            compression_method: CompressionMethodId(le_u16(&data[10..12])),
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
    pub fn parse_variable_length<'a>(&self, data: &'a [u8]) -> Option<VariableFields<'a>> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    pub fn blank_zip_archive() {
        let data = [80, 75, 5, 6];
        let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let archive = ZipArchive::from_seekable(Cursor::new(data), &mut buf);
        assert!(archive.is_err());
    }

    #[test]
    pub fn trunc_comment_zips() {
        let data = [
            80, 75, 6, 7, 21, 0, 0, 0, 34, 0, 0, 0, 0, 0, 0, 0, 10, 0, 59, 59, 80, 75, 5, 6, 0,
            255, 255, 255, 255, 255, 255, 0, 0, 0, 80, 75, 6, 6, 0, 0, 0, 10,
        ];
        let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let archive = ZipArchive::from_seekable(Cursor::new(data), &mut buf);
        assert!(archive.is_err());

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

        let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let archive = ZipArchive::from_seekable(Cursor::new(data), &mut buf);
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

        let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let archive = ZipArchive::from_seekable(Cursor::new(data), &mut buf).unwrap();
        let mut entries = archive.entries(&mut buf);
        assert!(entries.next_entry().is_err());
    }

    #[test]
    fn test_compressed_data_range() {
        let test_zip = std::fs::read("assets/test.zip").unwrap();

        // Test ZipSliceEntry API (from slice)
        let slice_archive = ZipArchive::from_slice(&test_zip).unwrap();
        let slice_header_records: Vec<_> = slice_archive
            .entries()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(slice_header_records.len(), 2);

        let entry1_wayfinder = slice_header_records[0].wayfinder();
        let slice_entry1 = slice_archive.get_entry(entry1_wayfinder).unwrap();
        let slice_range1 = slice_entry1.compressed_data_range();
        assert_eq!(
            slice_range1,
            (66, 91),
            "test.txt compressed data should be at bytes 66-91"
        );

        let entry2_wayfinder = slice_header_records[1].wayfinder();
        let slice_entry2 = slice_archive.get_entry(entry2_wayfinder).unwrap();
        let slice_range2 = slice_entry2.compressed_data_range();
        assert_eq!(
            slice_range2,
            (169, 954),
            "gophercolor16x16.png compressed data should be at bytes 169-954"
        );

        // Test ZipEntry API
        let file = std::fs::File::open("assets/test.zip").unwrap();
        let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let reader_archive = ZipArchive::from_file(file, &mut buffer).unwrap();

        // Get wayfinders from the slice archive since they should be identical
        let reader_entry1 = reader_archive.get_entry(entry1_wayfinder).unwrap();
        let reader_range1 = reader_entry1.compressed_data_range();

        let reader_entry2 = reader_archive.get_entry(entry2_wayfinder).unwrap();
        let reader_range2 = reader_entry2.compressed_data_range();

        // Verify both APIs return identical ranges
        assert_eq!(slice_range1, reader_range1);
        assert_eq!(slice_range2, reader_range2);
    }
}
