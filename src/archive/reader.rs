use super::*;
use crate::Crc32;
use crate::reader_at::{FileReader, MutexReader, RangeReader, ReaderAt, ReaderAtExt};
use crate::utils::le_u32;
use std::io::{Read, Seek};

#[derive(Debug, Clone)]
pub struct ZipSliceVerifier<Decompressor>(pub(super) ZipVerifier<Decompressor>);

impl<Decompressor> ZipSliceVerifier<Decompressor> {
    /// Consumes the [`ZipSliceVerifier`], returning the underlying decompressor
    /// without verifying.
    pub fn into_inner(self) -> Decompressor {
        self.0.into_inner()
    }
}

impl<Decompressor> std::io::Read for ZipSliceVerifier<Decompressor>
where
    Decompressor: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl ZipArchive<()> {
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
    pub(crate) fn new(reader: R, eocd: EndOfCentralDirectory) -> Self {
        ZipArchive { reader, eocd }
    }

    /// Returns a reference to the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.reader
    }

    /// Returns a mutable reference to the underlying reader.
    ///
    /// This is in contrast to [`ZipSliceArchive`], which cannot safely expose
    /// mutable access, as it relies on offsets and direct indexing.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Consumes this archive and returns the underlying reader.
    pub fn into_inner(self) -> R {
        self.reader
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

    /// Returns a Read implementation for the comment of the zip archive.
    ///
    /// Use [`RangeReader::remaining()`] to get the comment length before
    /// reading. It is guaranteed to be less than `u16::MAX`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use rawzip::{ZipArchive, ZipStr, RECOMMENDED_BUFFER_SIZE};
    /// use std::io::Read;
    /// use std::fs::File;
    ///
    /// let file = File::open("assets/test.zip")?;
    /// let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    /// let archive = ZipArchive::from_file(file, &mut buffer)?;
    ///
    /// let mut comment_reader = archive.comment();
    /// let comment_len = comment_reader.remaining() as usize;
    /// comment_reader.read_exact(&mut buffer[..comment_len])?;
    ///
    /// let actual = ZipStr::new(&buffer[..comment_len]);
    /// let expected = ZipStr::new(b"This is a zipfile comment.");
    /// assert_eq!(expected, actual);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn comment(&self) -> RangeReader<&R> {
        let comment_start =
            self.eocd.tail_eocd_offset() + EndOfCentralDirectoryRecordFixed::SIZE as u64;
        let comment_end = comment_start + self.eocd.comment_len() as u64;
        RangeReader::new(&self.reader, comment_start..comment_end)
    }

    /// Returns the offset of the End of Central Directory (EOCD) signature.
    ///
    /// This has the same semantics as [`ZipSliceArchive::eocd_offset`].
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

    /// The declared offset of the start of the central directory.
    ///
    /// This has the same semantics as [`ZipSliceArchive::directory_offset`].
    /// To verify the validity of this offset, start iterating through the
    /// central directory via [`ZipArchive::entries`]. Ensure no errors are
    /// returned on the first entry.
    pub fn directory_offset(&self) -> u64 {
        self.eocd.directory_offset()
    }

    /// The offset where the end of central directory record begins.
    ///
    /// This has the same semantics as
    /// [`ZipSliceArchive::central_directory_end`]: for zip64 archives it is the
    /// offset of the zip64 end of central directory record, otherwise the offset
    /// of the end of central directory record (differing from
    /// [`Self::eocd_offset`] only for zip64 archives). In a conventional archive
    /// the last central directory entry ends here, but an archive may place data
    /// between the central directory and the end of central directory record.
    /// Together with [`ZipEntries::position`] this bounds that region: once
    /// iteration stops, `[position, central_directory_end)` holds any such
    /// trailing bytes.
    pub fn central_directory_end(&self) -> u64 {
        self.eocd.head_eocd_offset()
    }

    /// Returns the offset where the ZIP archive ends.
    ///
    /// This has the same semantics as [`ZipSliceArchive::end_offset`].
    ///
    /// This can be used in conjunction with the starting offset calculation
    /// start offset as shown in [`RangeReader`] to determine the exact byte
    /// range (and thus size) of the ZIP archive within a context of a larger
    /// file.
    pub fn end_offset(&self) -> u64 {
        self.eocd.tail_eocd_offset()
            + EndOfCentralDirectoryRecordFixed::SIZE as u64
            + self.comment().remaining()
    }
}

impl<R> ZipArchive<R>
where
    R: ReaderAt,
{
    /// Seeks to the given file entry in the zip archive.
    ///
    /// This is the reader-backed equivalent of [`ZipSliceArchive::get_entry`].
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

impl<T: ReaderAt> ZipSliceArchive<T> {
    /// Converts the [`ZipSliceArchive`] into a general [`ZipArchive`].
    ///
    /// This is useful for unifying code that might handle both slice-based and
    /// reader-based archives. Because the underlying data already implements
    /// [`ReaderAt`], the conversion is zero-cost.
    pub fn into_reader(self) -> ZipArchive<T> {
        ZipArchive::from(self)
    }
}

impl<R> From<ZipSliceArchive<R>> for ZipArchive<R>
where
    R: ReaderAt,
{
    fn from(slice_archive: ZipSliceArchive<R>) -> Self {
        ZipArchive {
            reader: slice_archive.data,
            eocd: slice_archive.eocd,
        }
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
    pub fn reader(&self) -> ZipReader<&'archive R> {
        ZipReader {
            entry: self.entry,
            range_reader: RangeReader::new(
                self.archive.get_ref(),
                self.body_offset..self.body_end_offset,
            ),
        }
    }

    /// Returns a reader that wraps a decompressor and verify the size and CRC
    /// of the decompressed data once finished.
    ///
    /// For slice-backed entries, use [`ZipSliceEntry::verifying_reader`].
    pub fn verifying_reader<D>(&self, reader: D) -> ZipVerifier<D>
    where
        D: std::io::Read,
    {
        ZipVerifier {
            reader,
            crc: Crc32::new(),
            size: 0,
            verifier: ZipVerification {
                crc: self.entry.crc,
                uncompressed_size: self.entry.uncompressed_size_hint(),
            },
        }
    }

    /// Returns a tuple of start and end byte offsets for the compressed data
    /// within the underlying reader.
    ///
    /// This has the same semantics as
    /// [`ZipSliceEntry::compressed_data_range`], which carries a worked example
    /// of using these ranges to detect overlapping (zip bomb) entries.
    pub fn compressed_data_range(&self) -> (u64, u64) {
        (self.body_offset, self.body_end_offset)
    }

    /// Returns the local file header information.
    ///
    /// This has the same semantics as [`ZipSliceEntry::local_header`], but
    /// requires a caller-provided buffer because the local header is read from
    /// the underlying reader.
    ///
    /// The buffer argument must be large enough to hold both the filename and
    /// extra fields from the local header or a too small error will be
    /// returned.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use rawzip::{ZipArchive, RECOMMENDED_BUFFER_SIZE, extra_fields::ExtraFieldId};
    /// # use std::fs::File;
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Test with filename mismatch test fixture
    /// let file = File::open("assets/filename_mismatch_test.zip")?;
    /// let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    /// let archive = ZipArchive::from_file(file, &mut buf)?;
    ///
    /// let mut entries = archive.entries(&mut buf);
    /// let entry_header = entries.next_entry()?.unwrap();
    ///
    /// // Central directory shows one filename
    /// assert_eq!(entry_header.file_path().as_ref(), b"malware.exe");
    /// let wayfinder = entry_header.wayfinder();
    /// let entry = archive.get_entry(wayfinder)?;
    ///
    /// // Read the local header
    /// let mut local_buffer = vec![0u8; 1024];
    /// let local_header = entry.local_header(&mut local_buffer)?;
    ///
    /// // Local header shows different filename
    /// assert_eq!(local_header.file_path().as_ref(), b"safe_file.txt");
    ///
    /// // Access extra fields from local header
    /// let mut found_fields = 0;
    /// for (field_id, _data) in local_header.extra_fields() {
    ///     found_fields += 1;
    ///     // Could check for specific extra field types here
    ///     println!("Found extra field: {:04x}", field_id.as_u16());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn local_header<'a>(&self, buffer: &'a mut [u8]) -> Result<ZipLocalFileHeader<'a>, Error> {
        let mut header_buffer = [0u8; ZipLocalFileHeaderFixed::SIZE];

        // Read the local file header
        self.archive
            .get_ref()
            .read_exact_at(&mut header_buffer, self.entry.local_header_offset)?;

        let local_header_fixed =
            ZipLocalFileHeaderFixed::parse(&header_buffer).expect("header has already been parsed");
        let file_name_len = local_header_fixed.file_name_len as usize;
        let extra_field_len = local_header_fixed.extra_field_len as usize;
        let total_variable_len = file_name_len + extra_field_len;

        // Check if buffer is large enough for both filename and extra fields
        if buffer.len() < total_variable_len {
            return Err(Error::from(ErrorKind::BufferTooSmall {
                required: total_variable_len,
            }));
        }

        let variable_data = &mut buffer[..total_variable_len];
        let variable_data_offset =
            self.entry.local_header_offset + ZipLocalFileHeaderFixed::SIZE as u64;
        self.archive
            .get_ref()
            .read_exact_at(variable_data, variable_data_offset)?;

        let (filename_data, extra_field_data) = variable_data.split_at(file_name_len);

        let (compressed_size, uncompressed_size) =
            local_header_size_hints(&local_header_fixed, extra_field_data);

        Ok(ZipLocalFileHeader {
            fixed: local_header_fixed,
            compressed_size,
            uncompressed_size,
            file_path: ZipFilePath::from_bytes(filename_data),
            extra_field: extra_field_data,
        })
    }
}

/// Verifies the checksum of the decompressed data matches the checksum listed
/// in the central directory.
#[derive(Debug, Clone)]
pub struct ZipVerifier<Decompressor> {
    pub(super) reader: Decompressor,
    pub(super) crc: Crc32,
    pub(super) size: u64,
    pub(super) verifier: ZipVerification,
}

impl<Decompressor> ZipVerifier<Decompressor> {
    /// Consumes the [`ZipVerifier`], returning the underlying decompressor
    /// without verifying.
    pub fn into_inner(self) -> Decompressor {
        self.reader
    }
}

impl<Decompressor> std::io::Read for ZipVerifier<Decompressor>
where
    Decompressor: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let read = self.reader.read(buf)?;
        self.crc.update(&buf[..read]);
        self.size += read as u64;

        if read == 0 || self.size >= self.verifier.uncompressed_size {
            self.verifier
                .valid(ZipVerification {
                    crc: self.crc.checksum(),
                    uncompressed_size: self.size,
                })
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        }

        Ok(read)
    }
}

/// A reader for a Zip entry's compressed data.
#[derive(Debug, Clone)]
pub struct ZipReader<R> {
    entry: ZipArchiveEntryWayfinder,
    range_reader: RangeReader<R>,
}

impl<R> ZipReader<R>
where
    R: ReaderAt,
{
    /// Returns the expected data verification of the inflated data.
    ///
    /// This has the same semantics as [`ZipSliceEntry::claim_verifier`].
    pub fn claim_verifier(&self) -> ZipVerification {
        ZipVerification {
            crc: self.entry.crc,
            uncompressed_size: self.entry.uncompressed_size_hint(),
        }
    }

    /// Reads the trailing [`ZipDataDescriptor`] for this entry, if present.
    pub fn data_descriptor(&self) -> Result<Option<ZipDataDescriptor>, Error> {
        if !self.entry.has_data_descriptor {
            return Ok(None);
        }

        let descriptor = DataDescriptor::read_at(
            self.range_reader.get_ref(),
            self.range_reader.end_offset(),
            self.entry.data_descriptor_uses_zip64_sizes,
        )?;
        Ok(Some(ZipDataDescriptor {
            crc: descriptor.crc,
            compressed_size: descriptor.compressed_size,
            uncompressed_size: descriptor.uncompressed_size,
        }))
    }
}

impl<R> Read for ZipReader<R>
where
    R: ReaderAt,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.range_reader.read(buf)
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
    pub fn next_entry(&mut self) -> Result<Option<ZipFileHeaderRecord<'_>>, Error> {
        // Ensure a full fixed header is buffered before parsing. When fewer than
        // a header's worth of central directory bytes remain, hand off to the
        // cold tail which distinguishes a clean stop from a truncated record.
        if self.pos + ZipFileHeaderFixed::SIZE > self.end {
            // Keep this in u64: on a 32-bit target the central directory region
            // can exceed usize, so casting before the comparison could truncate
            // a large remainder to a small value and stop iteration early. Once
            // the comparison establishes the remainder is below a fixed header,
            // narrowing to usize is lossless.
            let cd_remaining =
                (self.end - self.pos) as u64 + (self.central_dir_end_pos - self.offset);
            if cd_remaining < ZipFileHeaderFixed::SIZE as u64 {
                return self.next_entry_tail(cd_remaining as usize);
            }

            // The buffer is too small to ever hold a fixed header; report the
            // real requirement, not the (smaller) shortfall left to read.
            if self.buffer.len() < ZipFileHeaderFixed::SIZE {
                return Err(Error::from(ErrorKind::BufferTooSmall {
                    required: ZipFileHeaderFixed::SIZE,
                }));
            }
            self.refill(ZipFileHeaderFixed::SIZE)?;
        }

        let central_directory_offset = self.position();
        let data = &self.buffer[self.pos..self.end];
        let file_header = match ZipFileHeaderFixed::parse(data) {
            Ok(file_header) => file_header,
            // A record that does not begin with the central directory header
            // signature marks the end of the central directory. Any trailing
            // bytes between here and the end of central directory record are
            // left for the caller to inspect via `position` and
            // `central_directory_end`.
            Err(e) if matches!(e.kind(), ErrorKind::InvalidSignature { .. }) => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        self.pos += ZipFileHeaderFixed::SIZE;

        let variable_length = file_header.variable_length();
        if self.pos + variable_length > self.end {
            // Need to read more data
            let remaining = self.end - self.pos;

            // The buffer can never hold this record's variable section.
            if variable_length > self.buffer.len() {
                return Err(Error::from(ErrorKind::BufferTooSmall {
                    required: variable_length,
                }));
            }

            // The variable section runs past the end of the central directory,
            // so the archive is truncated or corrupt.
            let cd_remaining = remaining + (self.central_dir_end_pos - self.offset) as usize;
            if variable_length > cd_remaining {
                return Err(Error::from(ErrorKind::Eof));
            }

            self.refill(variable_length)?;
        }

        let data = &self.buffer[self.pos..self.end];
        let (file_name, extra_field, file_comment, _) = file_header
            .parse_variable_length(data)
            .expect("variable length precheck failed");
        let mut file_header = ZipFileHeaderRecord::from_parts(
            file_header,
            file_name,
            extra_field,
            file_comment,
            central_directory_offset,
        );
        file_header.local_header_offset += self.base_offset;
        self.pos += variable_length;
        Ok(Some(file_header))
    }

    /// Handles the end of the central directory, where fewer than a fixed
    /// header's worth of bytes (`cd_remaining`) remain. Decides between a clean
    /// stop and a truncated record using just the 4-byte signature, so that
    /// trailing data smaller than a header is never misread as one.
    #[cold]
    fn next_entry_tail(
        &mut self,
        cd_remaining: usize,
    ) -> Result<Option<ZipFileHeaderRecord<'_>>, Error> {
        // Fewer than a signature's worth of bytes remain before the end of
        // central directory record, so iteration is complete.
        if cd_remaining < 4 {
            return Ok(None);
        }

        // Buffer the signature so we can tell whether a (truncated) central
        // directory header follows.
        if self.end - self.pos < 4 {
            // The buffer is too small to hold even a signature; report the real
            // requirement, not the (smaller) shortfall left to read.
            if self.buffer.len() < 4 {
                return Err(Error::from(ErrorKind::BufferTooSmall { required: 4 }));
            }
            self.refill(4)?;
        }

        if le_u32(&self.buffer[self.pos..self.pos + 4]) == CENTRAL_HEADER_SIGNATURE {
            Err(Error::from(ErrorKind::Eof))
        } else {
            Ok(None)
        }
    }

    /// Slides the unconsumed bytes to the front of the buffer and reads from the
    /// central directory until at least `need` bytes are buffered.
    ///
    /// The caller must guarantee that the buffer can hold `need` bytes and that
    /// at least `need` bytes remain before the end of the central directory;
    /// reads never cross into the end of central directory record.
    #[inline]
    fn refill(&mut self, need: usize) -> Result<(), Error> {
        let remaining = self.end - self.pos;
        self.buffer.copy_within(self.pos..self.end, 0);
        let max_read = ((self.central_dir_end_pos - self.offset) as usize)
            .min(self.buffer.len() - remaining);
        let read = self.archive.reader.read_at_least_at(
            &mut self.buffer[remaining..][..max_read],
            need - remaining,
            self.offset,
        )?;
        self.offset += read as u64;
        self.pos = 0;
        self.end = remaining + read;
        Ok(())
    }

    /// The offset immediately following the last yielded entry.
    ///
    /// Before any entry is yielded this equals
    /// [`ZipArchive::directory_offset`]. Once iteration has stopped, the region
    /// `[position, central_directory_end)` holds any bytes between the last
    /// central directory entry and the end of central directory record.
    /// Validating the number of yielded entries against
    /// [`ZipArchive::entries_hint`] is left to the caller.
    #[inline]
    pub fn position(&self) -> u64 {
        self.offset - (self.end - self.pos) as u64
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

        let (s1, e1) = slice_range1;
        assert!(std::ptr::eq(
            slice_entry1.data(),
            &test_zip[s1 as usize..e1 as usize]
        ));

        let (s2, e2) = slice_range2;
        assert!(std::ptr::eq(
            slice_entry2.data(),
            &test_zip[s2 as usize..e2 as usize]
        ));

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
