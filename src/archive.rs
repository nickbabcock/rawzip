use crate::crc::crc32_chunk;
use crate::errors::{Error, ErrorKind};
use crate::mode::{msdos_mode_to_file_mode, unix_mode_to_file_mode, EntryMode};
use crate::reader_at::{FileReader, MutexReader, ReaderAtExt};
use crate::time::{extract_best_timestamp, ZipDateTime};
use crate::utils::{le_u16, le_u32, le_u64};
use crate::{EndOfCentralDirectoryRecordFixed, ReaderAt, ZipLocator};
use std::{
    borrow::Cow,
    io::{Read, Seek, Write},
};

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
///         println!("File: {}", entry.file_safe_path()?);
///     }
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ZipSliceArchive<T: AsRef<[u8]>> {
    pub(crate) data: T,
    pub(crate) eocd: EndOfCentralDirectory,
}

impl<T: AsRef<[u8]>> ZipSliceArchive<T> {
    /// Returns an iterator over the entries in the central directory of the archive.
    pub fn entries(&self) -> ZipSliceEntries {
        let data = self.data.as_ref();
        let entry_data = &data[(self.eocd.offset() as usize).min(data.len())..];
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

    /// Returns the offset of the start of the zip file data.
    ///
    /// This is typically 0, but can be non-zero if the zip archive
    /// is embedded within a larger file (e.g., a self-extracting archive).
    pub fn base_offset(&self) -> u64 {
        self.eocd.base_offset()
    }

    /// The comment of the zip file.
    pub fn comment(&self) -> ZipStr {
        let data = self.data.as_ref();
        let comment_start = self.eocd.stream_pos as usize + EndOfCentralDirectoryRecordFixed::SIZE;
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

        Ok(ZipSliceEntry {
            data,
            verifier: ZipVerification {
                crc: expected_crc,
                uncompressed_size: entry.uncompressed_size_hint(),
            },
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
        let Ok(file_header) = ZipFileHeaderFixed::parse(self.entry_data) else {
            return Ok(None);
        };
        self.entry_data = &self.entry_data[ZipFileHeaderFixed::SIZE..];

        if self.entry_data.len() < file_header.file_name_len as usize {
            return Err(Error::from(ErrorKind::Eof));
        }
        let (file_name, rest) = self.entry_data.split_at(file_header.file_name_len as usize);

        if rest.len() < file_header.extra_field_len as usize {
            return Err(Error::from(ErrorKind::Eof));
        }
        let (extra_field, rest) = rest.split_at(file_header.extra_field_len as usize);

        if rest.len() < file_header.file_comment_len as usize {
            return Err(Error::from(ErrorKind::Eof));
        }
        let (file_comment, entry_data) = rest.split_at(file_header.file_comment_len as usize);

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
#[derive(Debug, Clone)]
pub struct ZipArchive<R> {
    pub(crate) reader: R,
    pub(crate) comment: ZipString,
    pub(crate) eocd: EndOfCentralDirectory,
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
        reader: R,
        buffer: &mut [u8],
    ) -> Result<ZipArchive<MutexReader<R>>, Error>
    where
        R: Read + Seek,
    {
        let reader = MutexReader::new(reader);
        ZipLocator::new()
            .locate_in_reader(reader, buffer)
            .map_err(|(_, e)| e)
    }
}

impl<R> ZipArchive<R> {
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
            entries_yielded: 0,
            pos: 0,
            end: 0,
            offset: self.eocd.offset(),
            base_offset: self.eocd.base_offset(),
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

    /// Returns the offset of the start of the zip file data.
    ///
    /// This is typically 0, but can be non-zero if the zip archive
    /// is embedded within a larger file (e.g., a self-extracting archive).
    pub fn base_offset(&self) -> u64 {
        self.eocd.base_offset()
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
        let body_offset = entry.local_header_offset
            + ZipLocalFileHeaderFixed::SIZE as u64
            + file_header.variable_length() as u64;

        Ok(ZipEntry {
            archive: self,
            entry,
            body_offset,
            body_end_offset: entry.compressed_size + body_offset,
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
    pub(crate) zip64: Option<Zip64EndOfCentralDirectoryRecord>,
    pub(crate) eocd: EndOfCentralDirectoryRecordFixed,
    pub(crate) stream_pos: u64,
}

impl EndOfCentralDirectory {
    /// the start of the zip file proper.
    #[inline]
    fn base_offset(&self) -> u64 {
        match &self.zip64 {
            Some(_) => 0,
            None => {
                let size = u64::from(self.eocd.central_dir_size);
                let offset = u64::from(self.eocd.central_dir_offset);
                self.stream_pos.saturating_sub(size).saturating_sub(offset)

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

    /// offset of the start of the central directory
    #[inline]
    fn offset(&self) -> u64 {
        self.zip64
            .as_ref()
            .map(|x| x.central_dir_offset)
            .unwrap_or_else(|| self.base_offset() + u64::from(self.eocd.central_dir_offset))
    }

    #[inline]
    fn entries(&self) -> u64 {
        self.zip64
            .as_ref()
            .map(|z| z.num_entries)
            .unwrap_or(u64::from(self.eocd.num_entries))
    }

    #[inline]
    fn comment_len(&self) -> usize {
        self.eocd.comment_len as usize
    }
}

/// A lending iterator over file header records in a [`ZipArchive`].
#[derive(Debug)]
pub struct ZipEntries<'archive, 'buf, R> {
    buffer: &'buf mut [u8],
    archive: &'archive ZipArchive<R>,
    entries_yielded: u64,
    pos: usize,
    end: usize,
    offset: u64,
    base_offset: u64,
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
        let file_header = loop {
            let data = &self.buffer[self.pos..self.end];
            match ZipFileHeaderFixed::parse(data) {
                Ok(file_header) => break file_header,
                Err(_) if self.entries_yielded == self.archive.entries_hint() => {
                    return Ok(None);
                }
                Err(e) if e.is_eof() => {
                    let remaining = data.len();
                    self.buffer.copy_within(self.pos..self.end, 0);
                    let read = self.archive.reader.try_read_at_least_at(
                        &mut self.buffer[remaining..],
                        ZipFileHeaderFixed::SIZE,
                        self.offset,
                    )?;
                    self.offset += read as u64;
                    self.pos = 0;
                    self.end = remaining + read;
                    if self.end < ZipFileHeaderFixed::SIZE {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        };

        self.pos += ZipFileHeaderFixed::SIZE;

        let variable_length = file_header.variable_length();

        let remaining = self.end - self.pos;
        if remaining < variable_length {
            self.buffer.copy_within(self.pos..self.end, 0);
            let read = self.archive.reader.read_at_least_at(
                &mut self.buffer[remaining..],
                variable_length - remaining,
                self.offset,
            )?;
            self.offset += read as u64;
            self.pos = 0;
            self.end = remaining + read;
        }

        let mut file_header = ZipFileHeaderRecord::from_data(file_header, &self.buffer[self.pos..]);
        file_header.local_header_offset += self.base_offset;
        self.pos += variable_length;
        self.entries_yielded += 1;
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
#[allow(dead_code)]
pub(crate) struct Zip64EndOfCentralDirectoryRecord {
    /// zip64 end of central dir signature
    pub signature: u32,

    /// size of zip64 end of central directory record
    pub size: u64,

    /// version made by
    pub version_made_by: VersionMadeBy,

    /// version needed to extract
    pub version_needed: u16,

    /// number of this disk
    pub disk_number: u32,

    /// number of the disk with the start of the central directory
    pub cd_disk: u32,

    /// total number of entries in the central directory on this disk
    pub num_entries: u64,

    /// total number of entries in the central directory
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

/// Represents a file path within a Zip archive, as a borrowed byte slice.
///
/// File paths in Zip archives are not guaranteed to be UTF-8, and may contain
/// relative paths that try and escape the zip or absolute paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ZipFilePath<'a>(ZipStr<'a>);

impl<'a> ZipFilePath<'a> {
    /// Creates a Zip file path from a byte slice.
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self(ZipStr::new(data))
    }

    /// Return the raw bytes of the Zip file path.
    ///
    /// **WARNING**: this may contain be an absolute path or contain a file path
    /// capable of zip slips. Prefer [`normalize`](ZipFilePath::normalize).
    #[inline]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0.as_bytes()
    }

    fn normalize_alloc(s: &str) -> String {
        // 4.4.17.1 All slashes MUST be forward slashes '/'
        let s = s.replace('\\', "/");

        // 4.4.17.1 MUST NOT contain a drive or device letter
        let s = s.split(':').next_back().unwrap_or_default();

        // resolve path components
        let splits = s.split('/');
        let mut result = String::new();
        for split in splits {
            if split.is_empty() || split == "." {
                continue;
            }

            if split == ".." {
                let last = result.rfind('/');
                result.truncate(last.unwrap_or(0));
                continue;
            }

            if !result.is_empty() {
                result.push('/');
            }

            result.push_str(split);
        }

        result
    }

    /// Returns true if the file path is a directory.
    ///
    /// This is determined by the file path ending in a slash,
    /// but it's a common convention as otherwise it would be an invalid file.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rawzip::ZipFilePath;
    /// let path = ZipFilePath::new(b"dir/");
    /// assert!(path.is_dir());
    ///
    /// let path = ZipFilePath::new(b"dir/file.txt");
    /// assert!(!path.is_dir());
    /// ```
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.0.as_bytes().last() == Some(&b'/')
    }

    /// Normalizes the Zip file path.
    ///
    /// The normalization process includes:
    /// - Attempting to interpret the path as UTF-8.
    /// - Replacing backslashes (`\`) with forward slashes (`/`).
    /// - Removing redundant slashes (e.g., `//`).
    /// - Resolving relative path components (`.` and `..`).
    /// - Stripping leading slashes and parent directory traversals that would
    ///   escape the archive's root (e.g., `/foo` becomes `foo`, `../foo`
    ///   becomes `foo`).
    ///
    /// In the case where no allocation needs to occur when normalizing, an
    /// original reference to the data is returned.
    ///
    /// # Examples
    ///
    /// Basic path normalization:
    /// ```
    /// # use rawzip::ZipFilePath;
    /// let path = ZipFilePath::new(b"dir/test.txt");
    /// assert_eq!(path.normalize().unwrap(), "dir/test.txt");
    ///
    /// // Converts backslashes to forward slashes
    /// let path = ZipFilePath::new(b"dir\\test.txt");
    /// assert_eq!(path.normalize().unwrap(), "dir/test.txt");
    ///
    /// // Removes redundant slashes
    /// let path = ZipFilePath::new(b"dir//test.txt");
    /// assert_eq!(path.normalize().unwrap(), "dir/test.txt");
    /// ```
    ///
    /// Handling relative and absolute paths:
    /// ```
    /// # use rawzip::ZipFilePath;
    /// // Removes leading slashes
    /// let path = ZipFilePath::new(b"/test.txt");
    /// assert_eq!(path.normalize().unwrap(), "test.txt");
    ///
    /// // Resolves current directory references
    /// let path = ZipFilePath::new(b"./test.txt");
    /// assert_eq!(path.normalize().unwrap(), "test.txt");
    ///
    /// // Resolves parent directory references
    /// let path = ZipFilePath::new(b"dir/../test.txt");
    /// assert_eq!(path.normalize().unwrap(), "test.txt");
    ///
    /// let path = ZipFilePath::new(b"a/b/c/d/../../test.txt");
    /// assert_eq!(path.normalize().unwrap(), "a/b/test.txt");
    ///
    /// let path = ZipFilePath::new(b"dir/");
    /// assert_eq!(path.normalize().unwrap(), "dir/");
    /// ```
    ///
    /// Invalid paths:
    /// ```
    /// # use rawzip::ZipFilePath;
    /// // Invalid UTF-8 sequences result in an error
    /// let path = ZipFilePath::new(&[0xFF]);
    /// assert!(path.normalize().is_err());
    ///
    /// let path = ZipFilePath::new(&[b't', b'e', b's', b't', 0xFF]);
    /// assert!(path.normalize().is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// - if the file path is not valid UTF-8.
    ///
    /// [Note that zip file names aren't always UTF-8][1]
    ///
    /// [1]: https://fasterthanli.me/articles/the-case-for-sans-io#character-encoding-differences
    pub fn normalize(&self) -> Result<Cow<str>, Error> {
        let mut name = std::str::from_utf8(self.as_bytes()).map_err(Error::utf8)?;
        let mut last = 0;
        for &c in name.as_bytes() {
            if matches!(
                (c, last),
                (b'\\', _) | (b'/', b'/') | (b'.', b'.') | (b'.', b'/') | (b':', _)
            ) {
                // slow path: intrusive string manipulations required
                return Ok(Cow::Owned(Self::normalize_alloc(name)));
            }
            last = c;
        }

        loop {
            // Fast path: before we trim, do a quick check if they are even necessary.
            name = match name.as_bytes() {
                [b'.', b'.', b'/', ..] => name.trim_start_matches("../"),
                [b'.', b'/', ..] => name.trim_start_matches("./"),
                [b'/', ..] => name.trim_start_matches('/'),
                _ => return Ok(Cow::Borrowed(name)),
            }
        }
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
    file_name: ZipFilePath<'a>,
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
            file_name: ZipFilePath::new(file_name),
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

    fn from_data(header: ZipFileHeaderFixed, data: &'a [u8]) -> Self {
        let file_name = &data[..header.file_name_len as usize];
        let data = &data[header.file_name_len as usize..];
        let extra_field = &data[..header.extra_field_len as usize];
        let data = &data[header.extra_field_len as usize..];
        let file_comment = &data[..header.file_comment_len as usize];
        Self::from_parts(header, file_name, extra_field, file_comment)
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

    /// Return the sanitized file path.
    ///
    /// See [`ZipFilePath::normalize`] for more information.
    #[inline]
    pub fn file_safe_path(&self) -> Result<Cow<str>, Error> {
        self.file_name.normalize()
    }

    /// Return the raw bytes of the file path
    ///
    /// **WARNING**: this may contain be an absolute path or contain a file path
    /// capable of zip slips. Prefer [`Self::file_safe_path`].
    #[inline]
    pub fn file_raw_path(&self) -> &[u8] {
        self.file_name.as_bytes()
    }

    /// Returns the last modification date and time.
    ///
    /// This method parses the extra field data to locate more accurate timestamps.
    #[inline]
    pub fn last_modified(&self) -> ZipDateTime {
        extract_best_timestamp(self.extra_field, self.last_mod_time, self.last_mod_date)
    }

    /// Returns the file mode information extracted from the external file attributes.
    #[inline]
    pub fn mode(&self) -> EntryMode {
        let creator_version = self.version_made_by >> 8;

        const UNIX: u16 = 3;
        const MACOS: u16 = 19;
        const NTFS: u16 = 11;
        const VFAT: u16 = 14;
        const FAT: u16 = 0;

        let mut mode = match creator_version {
            // Unix and macOS
            UNIX | MACOS => unix_mode_to_file_mode(self.external_file_attrs >> 16),
            // NTFS, VFAT, FAT
            NTFS | VFAT | FAT => msdos_mode_to_file_mode(self.external_file_attrs),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::io::Cursor;

    #[rstest]
    #[case(b"test.txt", "test.txt")]
    #[case(b"dir/test.txt", "dir/test.txt")]
    #[case(b"dir\\test.txt", "dir/test.txt")]
    #[case(b"dir//test.txt", "dir/test.txt")]
    #[case(b"/test.txt", "test.txt")]
    #[case(b"../test.txt", "test.txt")]
    #[case(b"dir/../test.txt", "test.txt")]
    #[case(b"./test.txt", "test.txt")]
    #[case(b"dir/./test.txt", "dir/test.txt")]
    #[case(b"dir/./../test.txt", "test.txt")]
    #[case(b"dir/sub/../test.txt", "dir/test.txt")]
    #[case(b"dir/../../test.txt", "test.txt")]
    #[case(b"../../../test.txt", "test.txt")]
    #[case(b"a/b/../../test.txt", "test.txt")]
    #[case(b"a/b/c/../../../test.txt", "test.txt")]
    #[case(b"a/b/c/d/../../test.txt", "a/b/test.txt")]
    #[case(b"C:\\hello\\test.txt", "hello/test.txt")]
    #[case(b"C:/hello\\test.txt", "hello/test.txt")]
    #[case(b"C:/hello/test.txt", "hello/test.txt")]
    fn test_zip_path_normalized(#[case] input: &[u8], #[case] expected: &str) {
        assert_eq!(ZipFilePath::new(input).normalize().unwrap(), expected);
    }

    #[rstest]
    #[case(&[0xFF])]
    #[case(&[b't', b'e', b's', b't', 0xFF])]
    fn test_zip_path_normalized_invalid_utf8(#[case] input: &[u8]) {
        assert!(ZipFilePath::new(input).normalize().is_err());
    }

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
}
