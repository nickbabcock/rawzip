use crate::{
    crc,
    errors::ErrorKind,
    extra_fields::ExtraFieldId,
    mode::CREATOR_UNIX,
    path::{NormalizedPath, NormalizedPathBuf, ZipFilePath},
    time::{DosDateTime, UtcDateTime},
    CompressionMethod, DataDescriptor, Error, ZipLocalFileHeaderFixed, CENTRAL_HEADER_SIGNATURE,
    END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE, END_OF_CENTRAL_DIR_SIGNATURE64,
    END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES,
};
use std::io::{self, Write};

// ZIP64 constants
const ZIP64_EXTRA_FIELD_ID: u16 = 0x0001;
const ZIP64_VERSION_NEEDED: u16 = 45; // 4.5
const ZIP64_EOCD_SIZE: usize = 56;

// General purpose bit flags
const FLAG_DATA_DESCRIPTOR: u16 = 0x08; // bit 3: data descriptor present
const FLAG_UTF8_ENCODING: u16 = 0x800; // bit 11: UTF-8 encoding flag (EFS)

// ZIP64 thresholds - when to switch to ZIP64 format
const ZIP64_THRESHOLD_FILE_SIZE: u64 = u32::MAX as u64;
const ZIP64_THRESHOLD_OFFSET: u64 = u32::MAX as u64;
const ZIP64_THRESHOLD_ENTRIES: usize = u16::MAX as usize;

#[derive(Debug)]
struct CountWriter<W> {
    writer: W,
    count: u64,
}

impl<W> CountWriter<W> {
    fn new(writer: W, count: u64) -> Self {
        CountWriter { writer, count }
    }

    fn count(&self) -> u64 {
        self.count
    }
}

impl<W: Write> Write for CountWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let bytes_written = self.writer.write(buf)?;
        self.count += bytes_written as u64;
        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Builds a `ZipArchiveWriter`.
#[derive(Debug, Default)]
pub struct ZipArchiveWriterBuilder {
    count: u64,
    capacity: usize,
}

impl ZipArchiveWriterBuilder {
    /// Creates a new `ZipArchiveWriterBuilder`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the anticipated number of files to optimize memory allocation.
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Sets the starting offset for writing. Useful when there is prelude data
    /// prior to the zip archive.
    ///
    /// When there is prelude data, setting the offset may not technically be
    /// required, but it is recommended. For standard zip files, many zip
    /// readers can self correct when the prelude data isn't properly declared.
    /// However for zip64 archives, setting the correct offset is required.
    ///
    /// # Example: Appending ZIP to existing data
    /// ```rust
    /// use std::io::{Cursor, Write, Seek, SeekFrom};
    ///
    /// // Create a file with some prefix data
    /// let mut output = Cursor::new(Vec::new());
    /// output.write_all(b"This is a custom header or prefix data\n").unwrap();
    /// let zip_start_offset = output.position();
    ///
    /// // Create ZIP archive starting after the prefix data
    /// let mut archive = rawzip::ZipArchiveWriter::builder()
    ///     .with_offset(zip_start_offset)  // Tell the archive where it starts
    ///     .build(&mut output);
    ///
    /// // Add files normally
    /// let mut file = archive.new_file("data.txt").create().unwrap();
    /// let mut writer = rawzip::ZipDataWriter::new(&mut file);
    /// writer.write_all(b"File content").unwrap();
    /// let (_, desc) = writer.finish().unwrap();
    /// file.finish(desc).unwrap();
    /// archive.finish().unwrap();
    ///
    /// // The resulting file contains both prefix data and the ZIP archive
    /// let final_data = output.into_inner();
    /// assert!(final_data.starts_with(b"This is a custom header"));
    /// ```
    pub fn with_offset(mut self, offset: u64) -> Self {
        self.count = offset;
        self
    }

    /// Builds a `ZipArchiveWriter` that writes to `writer`.
    pub fn build<W>(&self, writer: W) -> ZipArchiveWriter<W> {
        ZipArchiveWriter {
            writer: CountWriter::new(writer, self.count),
            files: Vec::with_capacity(self.capacity),
        }
    }
}

/// Create a new Zip archive.
///
/// Basic usage:
/// ```rust
/// use std::io::Write;
///
/// let mut output = std::io::Cursor::new(Vec::new());
/// let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
/// let mut file = archive.new_file("file.txt").create().unwrap();
/// let mut writer = rawzip::ZipDataWriter::new(&mut file);
/// writer.write_all(b"Hello, world!").unwrap();
/// let (_, output) = writer.finish().unwrap();
/// file.finish(output).unwrap();
/// archive.finish().unwrap();
/// ```
///
/// Use the builder for customization:
/// ```rust
/// use std::io::Write;
///
/// let mut output = std::io::Cursor::new(Vec::<u8>::new());
/// let mut _archive = rawzip::ZipArchiveWriter::builder()
///     .with_capacity(1000)  // Optimize for 1000 anticipated files
///     .build(&mut output);
/// // ... add files as usual
/// ```
#[derive(Debug)]
pub struct ZipArchiveWriter<W> {
    files: Vec<FileHeader>,
    writer: CountWriter<W>,
}

impl ZipArchiveWriter<()> {
    /// Creates a `ZipArchiveWriterBuilder` for configuring the writer.
    pub fn builder() -> ZipArchiveWriterBuilder {
        ZipArchiveWriterBuilder::new()
    }
}

impl<W> ZipArchiveWriter<W> {
    /// Creates a new `ZipArchiveWriter` that writes to `writer`.
    pub fn new(writer: W) -> Self {
        ZipArchiveWriterBuilder::new().build(writer)
    }
}

/// A builder for creating a new file entry in a ZIP archive.
#[derive(Debug)]
pub struct ZipFileBuilder<'archive, 'name, W> {
    archive: &'archive mut ZipArchiveWriter<W>,
    name: &'name str,
    compression_method: CompressionMethod,
    modification_time: Option<UtcDateTime>,
    unix_permissions: Option<u32>,
}

impl<'archive, W> ZipFileBuilder<'archive, '_, W>
where
    W: Write,
{
    /// Sets the compression method for the file entry.
    #[must_use]
    #[inline]
    pub fn compression_method(mut self, compression_method: CompressionMethod) -> Self {
        self.compression_method = compression_method;
        self
    }

    /// Sets the modification time for the file entry.
    ///
    /// Only accepts UTC timestamps to ensure Extended Timestamp fields are written correctly.
    #[must_use]
    #[inline]
    pub fn last_modified(mut self, modification_time: UtcDateTime) -> Self {
        self.modification_time = Some(modification_time);
        self
    }

    /// Sets the Unix permissions for the file entry.
    ///
    /// Accepts either:
    /// - Basic permission bits (e.g., 0o644 for rw-r--r--, 0o755 for rwxr-xr-x)
    /// - Full Unix mode including file type (e.g., 0o100644 for regular file, 0o040755 for directory)
    /// - Special permission bits are preserved (SUID: 0o4000, SGID: 0o2000, sticky: 0o1000)
    ///
    /// When set, the archive will be created with Unix-compatible "version made by" field
    /// to ensure proper interpretation of the permissions by zip readers.
    #[must_use]
    #[inline]
    pub fn unix_permissions(mut self, permissions: u32) -> Self {
        self.unix_permissions = Some(permissions);
        self
    }

    /// Creates the file entry and returns a writer for the file's content.
    pub fn create(self) -> Result<ZipEntryWriter<'archive, W>, Error> {
        let options = ZipEntryOptions {
            compression_method: self.compression_method,
            modification_time: self.modification_time,
            unix_permissions: self.unix_permissions,
        };
        self.archive.new_file_with_options(self.name, options)
    }
}

/// A builder for creating a new directory entry in a ZIP archive.
#[derive(Debug)]
pub struct ZipDirBuilder<'a, W> {
    archive: &'a mut ZipArchiveWriter<W>,
    name: &'a str,
    modification_time: Option<UtcDateTime>,
    unix_permissions: Option<u32>,
}

impl<W> ZipDirBuilder<'_, W>
where
    W: Write,
{
    /// Sets the modification time for the directory entry.
    ///
    /// See [`ZipFileBuilder::last_modified`] for details.
    #[must_use]
    #[inline]
    pub fn last_modified(mut self, modification_time: UtcDateTime) -> Self {
        self.modification_time = Some(modification_time);
        self
    }

    /// Sets the Unix permissions for the directory entry.
    ///
    /// See [`ZipFileBuilder::unix_permissions`] for details.
    #[must_use]
    #[inline]
    pub fn unix_permissions(mut self, permissions: u32) -> Self {
        self.unix_permissions = Some(permissions);
        self
    }

    /// Creates the directory entry.
    pub fn create(self) -> Result<(), Error> {
        let options = ZipEntryOptions {
            compression_method: CompressionMethod::Store, // Directories always use Store
            modification_time: self.modification_time,
            unix_permissions: self.unix_permissions,
        };
        self.archive.new_dir_with_options(self.name, options)
    }
}

impl<W> ZipArchiveWriter<W>
where
    W: Write,
{
    /// Writes a local file header and extended timestamp extra field if present.
    fn write_local_header(
        &mut self,
        file_path: &ZipFilePath<NormalizedPath>,
        flags: u16,
        compression_method: CompressionMethod,
        options: &ZipEntryOptions,
    ) -> Result<(), Error> {
        // Get DOS timestamp from options or use 0 as default
        let (dos_time, dos_date) = options
            .modification_time
            .as_ref()
            .map(|dt| DosDateTime::from(dt).into_parts())
            .unwrap_or((0, 0));

        let extra_field_len =
            extended_timestamp_extra_field_size(options.modification_time.as_ref());

        let header = ZipLocalFileHeaderFixed {
            signature: ZipLocalFileHeaderFixed::SIGNATURE,
            version_needed: 20,
            flags,
            compression_method: compression_method.as_id(),
            last_mod_time: dos_time,
            last_mod_date: dos_date,
            crc32: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            file_name_len: file_path.len() as u16,
            extra_field_len,
        };

        header.write(&mut self.writer)?;
        self.writer.write_all(file_path.as_ref().as_bytes())?;
        write_extended_timestamp_field(&mut self.writer, options.modification_time.as_ref())?;

        Ok(())
    }

    /// Creates a builder for adding a new directory to the archive.
    ///
    /// The name of the directory must end with a `/`.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::io::Cursor;
    /// # let mut output = Cursor::new(Vec::new());
    /// # let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
    /// archive.new_dir("my-dir/")
    ///     .unix_permissions(0o755)
    ///     .create()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn new_dir<'a>(&'a mut self, name: &'a str) -> ZipDirBuilder<'a, W> {
        ZipDirBuilder {
            archive: self,
            name,
            modification_time: None,
            unix_permissions: None,
        }
    }

    /// Adds a new directory to the archive with options (internal method).
    ///
    /// The name of the directory must end with a `/`.
    fn new_dir_with_options(&mut self, name: &str, options: ZipEntryOptions) -> Result<(), Error> {
        let file_path = ZipFilePath::from_str(name);
        if !file_path.is_dir() {
            return Err(Error::from(ErrorKind::InvalidInput {
                msg: "not a directory".to_string(),
            }));
        }

        if file_path.len() > u16::MAX as usize {
            return Err(Error::from(ErrorKind::InvalidInput {
                msg: "directory name too long".to_string(),
            }));
        }

        let local_header_offset = self.writer.count();
        let mut flags = 0u16;
        if file_path.needs_utf8_encoding() {
            flags |= FLAG_UTF8_ENCODING;
        } else {
            flags &= !FLAG_UTF8_ENCODING;
        }

        self.write_local_header(&file_path, flags, CompressionMethod::Store, &options)?;

        let file_header = FileHeader {
            name: file_path.into_owned(),
            compression_method: CompressionMethod::Store,
            local_header_offset,
            compressed_size: 0,
            uncompressed_size: 0,
            crc: 0,
            flags,
            modification_time: options.modification_time,
            unix_permissions: options.unix_permissions,
        };
        self.files.push(file_header);

        Ok(())
    }

    /// Creates a builder for adding a new file to the archive.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::io::{Cursor, Write};
    /// # let mut output = Cursor::new(Vec::new());
    /// # let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
    /// let mut file = archive.new_file("my-file")
    ///     .compression_method(rawzip::CompressionMethod::Deflate)
    ///     .unix_permissions(0o644)
    ///     .create()?;
    /// let mut writer = rawzip::ZipDataWriter::new(&mut file);
    /// writer.write_all(b"Hello, world!")?;
    /// let (_, output) = writer.finish()?;
    /// file.finish(output)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn new_file<'name>(&mut self, name: &'name str) -> ZipFileBuilder<'_, 'name, W> {
        ZipFileBuilder {
            archive: self,
            name,
            compression_method: CompressionMethod::Store,
            modification_time: None,
            unix_permissions: None,
        }
    }

    /// Adds a new file to the archive with options (internal method).
    fn new_file_with_options(
        &mut self,
        name: &str,
        options: ZipEntryOptions,
    ) -> Result<ZipEntryWriter<'_, W>, Error> {
        let file_path = ZipFilePath::from_str(name.trim_end_matches('/'));

        if file_path.len() > u16::MAX as usize {
            return Err(Error::from(ErrorKind::InvalidInput {
                msg: "file name too long".to_string(),
            }));
        }

        let local_header_offset = self.writer.count();
        let mut flags = FLAG_DATA_DESCRIPTOR;
        if file_path.needs_utf8_encoding() {
            flags |= FLAG_UTF8_ENCODING;
        } else {
            flags &= !FLAG_UTF8_ENCODING;
        }

        self.write_local_header(&file_path, flags, options.compression_method, &options)?;

        Ok(ZipEntryWriter::new(
            self,
            file_path.into_owned(),
            local_header_offset,
            options.compression_method,
            flags,
            options.modification_time,
            options.unix_permissions,
        ))
    }

    /// Finishes writing the archive and returns the underlying writer.
    ///
    /// This writes the central directory and the end of central directory
    /// record. ZIP64 format is used automatically when thresholds are exceeded.
    pub fn finish(mut self) -> Result<W, Error>
    where
        W: Write,
    {
        let central_directory_offset = self.writer.count();
        let total_entries = self.files.len();

        // Determine if we need ZIP64 format
        let needs_zip64 = total_entries >= ZIP64_THRESHOLD_ENTRIES
            || central_directory_offset >= ZIP64_THRESHOLD_OFFSET
            || self.files.iter().any(|f| f.needs_zip64());

        // Write central directory entries
        for file in &self.files {
            // Central file header signature
            self.writer
                .write_all(&CENTRAL_HEADER_SIGNATURE.to_le_bytes())?;

            // Version made by and version needed to extract
            let version_needed = if file.needs_zip64() {
                ZIP64_VERSION_NEEDED
            } else {
                20
            };

            // Set version_made_by to indicate Unix when Unix permissions are present
            let version_made_by_hi = file.unix_permissions.map(|_| CREATOR_UNIX).unwrap_or(0);
            let version_made_by = (version_made_by_hi << 8) | version_needed;

            self.writer.write_all(&version_made_by.to_le_bytes())?; // Version made by
            self.writer.write_all(&version_needed.to_le_bytes())?; // Version needed to extract

            // General purpose bit flag
            self.writer.write_all(&file.flags.to_le_bytes())?;

            // Compression method
            self.writer
                .write_all(&file.compression_method.as_id().as_u16().to_le_bytes())?;

            // Last mod file time and date
            let (dos_time, dos_date) = file
                .modification_time
                .as_ref()
                .map(|dt| DosDateTime::from(dt).into_parts())
                .unwrap_or((0, 0));
            self.writer.write_all(&dos_time.to_le_bytes())?;
            self.writer.write_all(&dos_date.to_le_bytes())?;

            // CRC-32
            self.writer.write_all(&file.crc.to_le_bytes())?;

            // Compressed size - use 0xFFFFFFFF if ZIP64
            let compressed_size = file.compressed_size.min(ZIP64_THRESHOLD_FILE_SIZE) as u32;
            self.writer.write_all(&compressed_size.to_le_bytes())?;

            // Uncompressed size - use 0xFFFFFFFF if ZIP64
            let uncompressed_size = file.uncompressed_size.min(ZIP64_THRESHOLD_FILE_SIZE) as u32;
            self.writer.write_all(&uncompressed_size.to_le_bytes())?;

            // File name length
            self.writer
                .write_all(&(file.name.len() as u16).to_le_bytes())?;

            // Extra field length
            let extra_field_length = file.zip64_extra_field_size()
                + extended_timestamp_extra_field_size(file.modification_time.as_ref());
            self.writer.write_all(&extra_field_length.to_le_bytes())?;

            // File comment length
            self.writer.write_all(&0u16.to_le_bytes())?;

            // Disk number start, internal file attributes
            self.writer.write_all(&[0u8; 4])?;

            // External file attributes
            let external_attrs = file.unix_permissions.map(|x| x << 16).unwrap_or(0);
            self.writer.write_all(&external_attrs.to_le_bytes())?;

            // Local header offset - use 0xFFFFFFFF if ZIP64
            let local_header_offset = file.local_header_offset.min(ZIP64_THRESHOLD_OFFSET) as u32;
            self.writer.write_all(&local_header_offset.to_le_bytes())?;

            // File name
            self.writer.write_all(file.name.as_ref().as_bytes())?;

            // ZIP64 extended information extra field
            file.write_zip64_extra_field(&mut self.writer)?;

            write_extended_timestamp_field(&mut self.writer, file.modification_time.as_ref())?;
        }

        let central_directory_end = self.writer.count();
        let central_directory_size = central_directory_end - central_directory_offset;

        // Write ZIP64 structures if needed
        if needs_zip64 {
            let zip64_eocd_offset = self.writer.count();

            // Write ZIP64 End of Central Directory Record
            write_zip64_eocd(
                &mut self.writer,
                total_entries as u64,
                central_directory_size,
                central_directory_offset,
            )?;

            // Write ZIP64 End of Central Directory Locator
            write_zip64_eocd_locator(&mut self.writer, zip64_eocd_offset)?;
        }

        // Write regular End of Central Directory Record
        self.writer.write_all(&END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES)?;

        // Disk numbers
        self.writer.write_all(&[0u8; 4])?;

        // Number of entries - use 0xFFFF if ZIP64
        let entries_count = total_entries.min(ZIP64_THRESHOLD_ENTRIES) as u16;
        self.writer.write_all(&entries_count.to_le_bytes())?;
        self.writer.write_all(&entries_count.to_le_bytes())?;

        // Central directory size - use 0xFFFFFFFF if ZIP64
        let cd_size = central_directory_size.min(ZIP64_THRESHOLD_OFFSET) as u32;
        self.writer.write_all(&cd_size.to_le_bytes())?;

        // Central directory offset - use 0xFFFFFFFF if ZIP64
        let cd_offset = central_directory_offset.min(ZIP64_THRESHOLD_OFFSET) as u32;
        self.writer.write_all(&cd_offset.to_le_bytes())?;

        // Comment length
        self.writer.write_all(&0u16.to_le_bytes())?;

        self.writer.flush()?;
        Ok(self.writer.writer)
    }
}

/// A writer for a file in a ZIP archive.
///
/// This writer is created by `ZipArchiveWriter::new_file`.
/// Data written to this writer is compressed and written to the underlying archive.
///
/// After writing all data, call `finish` to complete the entry.
pub struct ZipEntryWriter<'a, W> {
    inner: &'a mut ZipArchiveWriter<W>,
    compressed_bytes: u64,
    name: ZipFilePath<NormalizedPathBuf>,
    local_header_offset: u64,
    compression_method: CompressionMethod,
    flags: u16,
    modification_time: Option<UtcDateTime>,
    unix_permissions: Option<u32>,
}

impl<'a, W> ZipEntryWriter<'a, W> {
    /// Creates a new `TrackingWriter` wrapping the given writer.
    pub(crate) fn new(
        inner: &'a mut ZipArchiveWriter<W>,
        name: ZipFilePath<NormalizedPathBuf>,
        local_header_offset: u64,
        compression_method: CompressionMethod,
        flags: u16,
        modification_time: Option<UtcDateTime>,
        unix_permissions: Option<u32>,
    ) -> Self {
        ZipEntryWriter {
            inner,
            compressed_bytes: 0,
            name,
            local_header_offset,
            compression_method,
            flags,
            modification_time,
            unix_permissions,
        }
    }

    /// Returns the total number of bytes successfully written (bytes out).
    pub fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    /// Finishes writing the file entry.
    ///
    /// This writes the data descriptor if necessary and adds the file entry to the central directory.
    pub fn finish(self, mut output: DataDescriptorOutput) -> Result<u64, Error>
    where
        W: Write,
    {
        output.compressed_size = self.compressed_bytes;

        // Write data descriptor
        self.inner
            .writer
            .write_all(&DataDescriptor::SIGNATURE.to_le_bytes())?;

        self.inner.writer.write_all(&output.crc.to_le_bytes())?;

        if output.compressed_size >= ZIP64_THRESHOLD_FILE_SIZE
            || output.uncompressed_size >= ZIP64_THRESHOLD_FILE_SIZE
        {
            // Use 64-bit sizes for ZIP64
            self.inner
                .writer
                .write_all(&output.compressed_size.to_le_bytes())?;
            self.inner
                .writer
                .write_all(&output.uncompressed_size.to_le_bytes())?;
        } else {
            // Use 32-bit sizes for standard ZIP
            self.inner
                .writer
                .write_all(&(output.compressed_size as u32).to_le_bytes())?;
            self.inner
                .writer
                .write_all(&(output.uncompressed_size as u32).to_le_bytes())?;
        }

        let file_header = FileHeader {
            name: self.name,
            compression_method: self.compression_method,
            local_header_offset: self.local_header_offset,
            compressed_size: output.compressed_size,
            uncompressed_size: output.uncompressed_size,
            crc: output.crc,
            flags: self.flags,
            modification_time: self.modification_time,
            unix_permissions: self.unix_permissions,
        };
        self.inner.files.push(file_header);

        Ok(self.compressed_bytes)
    }
}

impl<W> Write for ZipEntryWriter<'_, W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let bytes_written = self.inner.writer.write(buf)?;
        self.compressed_bytes += bytes_written as u64;
        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.writer.flush()
    }
}

/// A writer for the uncompressed data of a Zip file entry.
///
/// This writer will keep track of the data necessary to write the data
/// descriptor (ie: number of bytes written and the CRC32 checksum).
///
/// Once all the data has been written, invoke the `finish` method to receive the
/// `DataDescriptorOutput` necessary to finalize the entry.
#[derive(Debug)]
pub struct ZipDataWriter<W> {
    inner: W,
    uncompressed_bytes: u64,
    crc: u32,
}

impl<W> ZipDataWriter<W> {
    /// Creates a new `ZipDataWriter` that writes to an underlying writer.
    pub fn new(inner: W) -> Self {
        ZipDataWriter {
            inner,
            uncompressed_bytes: 0,
            crc: 0,
        }
    }

    /// Gets a mutable reference to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consumes self and returns the inner writer and the data descriptor to be
    /// passed to a `ZipEntryWriter`.
    ///
    /// The writer is returned to facilitate situations where the underlying
    /// compressor needs to be notified that no more data will be written so it
    /// can write any sort of necesssary epilogue (think zstd).
    ///
    /// The `DataDescriptorOutput` contains the CRC32 checksum and uncompressed size,
    /// which is needed by `ZipEntryWriter::finish`.
    pub fn finish(mut self) -> Result<(W, DataDescriptorOutput), Error>
    where
        W: Write,
    {
        self.flush()?;
        let output = DataDescriptorOutput {
            crc: self.crc,
            compressed_size: 0,
            uncompressed_size: self.uncompressed_bytes,
        };

        Ok((self.inner, output))
    }
}

impl<W> Write for ZipDataWriter<W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let bytes_written = self.inner.write(buf)?;
        self.uncompressed_bytes += bytes_written as u64;
        self.crc = crc::crc32_chunk(&buf[..bytes_written], self.crc);
        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Contains information written in the data descriptor after the file data.
#[derive(Debug, Clone)]
pub struct DataDescriptorOutput {
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
}

impl DataDescriptorOutput {
    /// Returns the CRC32 checksum of the uncompressed data.
    pub fn crc(&self) -> u32 {
        self.crc
    }

    /// Returns the uncompressed size of the data.
    pub fn uncompressed_size(&self) -> u64 {
        self.uncompressed_size
    }
}

#[derive(Debug)]
struct FileHeader {
    name: ZipFilePath<NormalizedPathBuf>,
    compression_method: CompressionMethod,
    local_header_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    crc: u32,
    flags: u16,
    modification_time: Option<UtcDateTime>,
    unix_permissions: Option<u32>,
}

impl FileHeader {
    fn needs_zip64(&self) -> bool {
        self.compressed_size >= ZIP64_THRESHOLD_FILE_SIZE
            || self.uncompressed_size >= ZIP64_THRESHOLD_FILE_SIZE
            || self.local_header_offset >= ZIP64_THRESHOLD_OFFSET
    }

    /// Writes the ZIP64 extended information extra field for this file header
    fn write_zip64_extra_field<W>(&self, writer: &mut W) -> Result<(), Error>
    where
        W: Write,
    {
        if !self.needs_zip64() {
            return Ok(());
        }

        // ZIP64 Extended Information Extra Field header
        writer.write_all(&ZIP64_EXTRA_FIELD_ID.to_le_bytes())?;

        // Calculate size of data portion
        let mut data_size = 0u16;
        if self.uncompressed_size >= ZIP64_THRESHOLD_FILE_SIZE {
            data_size += 8;
        }
        if self.compressed_size >= ZIP64_THRESHOLD_FILE_SIZE {
            data_size += 8;
        }
        if self.local_header_offset >= ZIP64_THRESHOLD_OFFSET {
            data_size += 8;
        }

        writer.write_all(&data_size.to_le_bytes())?;

        // Write the actual data fields in the order specified by the spec
        if self.uncompressed_size >= ZIP64_THRESHOLD_FILE_SIZE {
            writer.write_all(&self.uncompressed_size.to_le_bytes())?;
        }
        if self.compressed_size >= ZIP64_THRESHOLD_FILE_SIZE {
            writer.write_all(&self.compressed_size.to_le_bytes())?;
        }
        if self.local_header_offset >= ZIP64_THRESHOLD_OFFSET {
            writer.write_all(&self.local_header_offset.to_le_bytes())?;
        }

        Ok(())
    }

    /// Calculates the size of the ZIP64 extra field for this file header
    fn zip64_extra_field_size(&self) -> u16 {
        if !self.needs_zip64() {
            return 0;
        }

        let mut size = 4u16; // Header (ID + size)
        if self.uncompressed_size >= ZIP64_THRESHOLD_FILE_SIZE {
            size += 8;
        }
        if self.compressed_size >= ZIP64_THRESHOLD_FILE_SIZE {
            size += 8;
        }
        if self.local_header_offset >= ZIP64_THRESHOLD_OFFSET {
            size += 8;
        }
        size
    }
}

fn extended_timestamp_extra_field_size(modification_time: Option<&UtcDateTime>) -> u16 {
    if modification_time.is_some() {
        9 // 2 bytes ID + 2 bytes size + 1 byte flags + 4 bytes timestamp
    } else {
        0
    }
}

fn write_extended_timestamp_field<W>(
    writer: &mut W,
    datetime: Option<&UtcDateTime>,
) -> Result<(), Error>
where
    W: Write,
{
    let Some(datetime) = datetime else {
        return Ok(());
    };
    let unix_time = datetime.to_unix().max(0) as u32; // ZIP format uses u32 for Unix timestamps, clamp negatives to 0
    writer.write_all(&ExtraFieldId::EXTENDED_TIMESTAMP.as_u16().to_le_bytes())?;
    writer.write_all(&5u16.to_le_bytes())?; // Size: 1 byte flags + 4 bytes timestamp
    writer.write_all(&1u8.to_le_bytes())?; // Flags: modification time present
    writer.write_all(&unix_time.to_le_bytes())?; // Unix timestamp
    Ok(())
}

/// Writes the ZIP64 End of Central Directory Record
fn write_zip64_eocd<W>(
    writer: &mut W,
    total_entries: u64,
    central_directory_size: u64,
    central_directory_offset: u64,
) -> Result<(), Error>
where
    W: Write,
{
    // ZIP64 End of Central Directory Record signature
    writer.write_all(&END_OF_CENTRAL_DIR_SIGNATURE64.to_le_bytes())?;

    // Size of ZIP64 end of central directory record (excluding signature and this field)
    let record_size = (ZIP64_EOCD_SIZE - 12) as u64;
    writer.write_all(&record_size.to_le_bytes())?;

    // Version made by
    writer.write_all(&ZIP64_VERSION_NEEDED.to_le_bytes())?;

    // Version needed to extract
    writer.write_all(&ZIP64_VERSION_NEEDED.to_le_bytes())?;

    // Number of this disk
    writer.write_all(&0u32.to_le_bytes())?;

    // Number of the disk with the start of the central directory
    writer.write_all(&0u32.to_le_bytes())?;

    // Total number of entries in the central directory on this disk
    writer.write_all(&total_entries.to_le_bytes())?;

    // Total number of entries in the central directory
    writer.write_all(&total_entries.to_le_bytes())?;

    // Size of the central directory
    writer.write_all(&central_directory_size.to_le_bytes())?;

    // Offset of start of central directory with respect to the starting disk number
    writer.write_all(&central_directory_offset.to_le_bytes())?;

    Ok(())
}

/// Writes the ZIP64 End of Central Directory Locator
fn write_zip64_eocd_locator<W>(writer: &mut W, zip64_eocd_offset: u64) -> Result<(), Error>
where
    W: Write,
{
    // ZIP64 End of Central Directory Locator signature
    writer.write_all(&END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE.to_le_bytes())?;

    // Number of the disk with the start of the ZIP64 end of central directory
    writer.write_all(&0u32.to_le_bytes())?;

    // Relative offset of the ZIP64 end of central directory record
    writer.write_all(&zip64_eocd_offset.to_le_bytes())?;

    // Total number of disks
    writer.write_all(&1u32.to_le_bytes())?;

    Ok(())
}

#[derive(Debug, Clone)]
struct ZipEntryOptions {
    compression_method: CompressionMethod,
    modification_time: Option<UtcDateTime>,
    unix_permissions: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_name_lifetime_independence() {
        let mut output = Cursor::new(Vec::new());
        let mut archive = ZipArchiveWriter::new(&mut output);

        // Test file builder with temporary name
        {
            let mut file = {
                let temp_name = format!("temp-{}.txt", 42);
                archive.new_file(&temp_name).create().unwrap()
            };
            let mut writer = ZipDataWriter::new(&mut file);
            writer.write_all(b"test").unwrap();
            let (_, desc) = writer.finish().unwrap();
            file.finish(desc).unwrap();
        }

        archive.finish().unwrap();
    }

    #[test]
    fn test_builder_with_offset_and_capacity() {
        let mut output = Cursor::new(Vec::new());

        output.write_all(b"PREFIX DATA").unwrap();
        let offset = output.position();

        let mut archive = ZipArchiveWriterBuilder::new()
            .with_capacity(5)
            .with_offset(offset)
            .build(&mut output);

        let mut file = archive.new_file("test.txt").create().unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello World").unwrap();
        let (_, desc) = writer.finish().unwrap();
        file.finish(desc).unwrap();

        archive.finish().unwrap();
    }
}
