use crate::{
    crc, errors::ErrorKind, time::{DosDateTime, EXTENDED_TIMESTAMP_ID}, CompressionMethod, DataDescriptor, Error, ZipDateTime, ZipFilePath,
    ZipLocalFileHeaderFixed, CENTRAL_HEADER_SIGNATURE, END_OF_CENTRAL_DIR_LOCATOR_SIGNATURE,
    END_OF_CENTRAL_DIR_SIGNATURE64, END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES,
};
use std::io::{self, Write};

// ZIP64 constants
const ZIP64_EXTRA_FIELD_ID: u16 = 0x0001;
const ZIP64_VERSION_NEEDED: u16 = 45; // 4.5
const ZIP64_EOCD_SIZE: usize = 56;

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
#[derive(Debug)]
pub struct ZipArchiveWriterBuilder {
    count: u64,
}

impl ZipArchiveWriterBuilder {
    /// Creates a new `ZipArchiveWriterBuilder`.
    pub fn new() -> Self {
        ZipArchiveWriterBuilder { count: 0 }
    }

    /// Builds a `ZipArchiveWriter` that writes to `writer`.
    pub fn build<W>(&self, writer: W) -> ZipArchiveWriter<W> {
        ZipArchiveWriter {
            writer: CountWriter::new(writer, self.count),
            files: Vec::new(),
        }
    }
}

impl Default for ZipArchiveWriterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a new Zip archive.
///
/// ```rust
/// use std::io::Write;
///
/// let mut output = std::io::Cursor::new(Vec::new());
/// let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
/// let mut file = archive.new_file("file.txt", rawzip::ZipEntryOptions::default()).unwrap();
/// let mut writer = rawzip::ZipDataWriter::new(&mut file);
/// writer.write_all(b"Hello, world!").unwrap();
/// let (_, output) = writer.finish().unwrap();
/// file.finish(output).unwrap();
/// archive.finish().unwrap();
/// ```
#[derive(Debug)]
pub struct ZipArchiveWriter<W> {
    files: Vec<FileHeader>,
    writer: CountWriter<W>,
}

impl ZipArchiveWriter<()> {
    /// Creates a `ZipArchiveWriterBuilder` that starts writing at `offset`.
    /// This is useful when the ZIP archive is appended to an existing file.
    pub fn at_offset(offset: u64) -> ZipArchiveWriterBuilder {
        ZipArchiveWriterBuilder { count: offset }
    }
}

impl<W> ZipArchiveWriter<W> {
    /// Creates a new `ZipArchiveWriter` that writes to `writer`.
    pub fn new(writer: W) -> Self {
        ZipArchiveWriterBuilder::new().build(writer)
    }
}

impl<W> ZipArchiveWriter<W>
where
    W: Write,
{
    /// Writes a local file header and extended timestamp extra field if present.
    /// Returns the local header offset.
    fn write_local_header(
        &mut self,
        safe_file_path: &str,
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

        // Calculate extra field length for extended timestamp
        let extra_field_len = if options.modification_time.is_some() {
            9u16 // Extended timestamp field: 2 bytes ID + 2 bytes size + 1 byte flags + 4 bytes timestamp
        } else {
            0u16
        };

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
            file_name_len: safe_file_path.len() as u16,
            extra_field_len,
        };

        header.write(&mut self.writer)?;
        self.writer.write_all(safe_file_path.as_bytes())?;

        // Write extended timestamp extra field if present
        if let Some(ref datetime) = options.modification_time {
            let unix_time = datetime.to_unix();
            self.writer.write_all(&EXTENDED_TIMESTAMP_ID.to_le_bytes())?;
            self.writer.write_all(&5u16.to_le_bytes())?; // Size: 1 byte flags + 4 bytes timestamp
            self.writer.write_all(&1u8.to_le_bytes())?; // Flags: modification time present
            self.writer.write_all(&unix_time.to_le_bytes())?; // Unix timestamp
        }

        Ok(())
    }

    /// Adds a new directory to the archive.
    ///
    /// The name of the directory must end with a `/`.
    pub fn new_dir(&mut self, name: &str, options: ZipEntryOptions) -> Result<(), Error> {
        let file_path = ZipFilePath::new(name.as_bytes());
        if !file_path.is_dir() {
            return Err(Error::from(ErrorKind::InvalidInput {
                msg: "not a directory".to_string(),
            }));
        }

        let safe_file_path = file_path.normalize()?.into_owned();
        if safe_file_path.len() > u16::MAX as usize {
            return Err(Error::from(ErrorKind::InvalidInput {
                msg: "directory name too long".to_string(),
            }));
        }

        let local_header_offset = self.writer.count();
        self.write_local_header(
            &safe_file_path,
            0x0, // flags for directory
            CompressionMethod::Store,
            &options,
        )?;

        let file_header = FileHeader {
            name: safe_file_path,
            compression_method: CompressionMethod::Store,
            local_header_offset,
            compressed_size: 0,
            uncompressed_size: 0,
            crc: 0,
            modification_time: options.modification_time.clone(),
        };
        self.files.push(file_header);

        Ok(())
    }

    /// Adds a new file to the archive and returns a writer for the file's content,
    /// which should be passed to a compressor.
    pub fn new_file<'a>(
        &'a mut self,
        name: &str,
        options: ZipEntryOptions,
    ) -> Result<ZipEntryWriter<'a, W>, Error> {
        let file_path = ZipFilePath::new(name.as_bytes());
        let safe_file_path = file_path.normalize()?.trim_end_matches('/').to_owned();

        if safe_file_path.len() > u16::MAX as usize {
            return Err(Error::from(ErrorKind::InvalidInput {
                msg: "file name too long".to_string(),
            }));
        }

        let local_header_offset = self.writer.count();
        self.write_local_header(
            &safe_file_path,
            0x8, // data descriptor flag for files
            options.compression_method,
            &options,
        )?;

        Ok(ZipEntryWriter::new(
            self,
            safe_file_path,
            local_header_offset,
            options.compression_method,
            options.modification_time.clone(),
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

            // Version made by - use ZIP64 version if needed
            let version_made_by = if needs_zip64 {
                ZIP64_VERSION_NEEDED
            } else {
                20
            };
            self.writer.write_all(&version_made_by.to_le_bytes())?;

            // Version needed to extract - use ZIP64 version if needed
            let version_needed = if needs_zip64 {
                ZIP64_VERSION_NEEDED
            } else {
                20
            };
            self.writer.write_all(&version_needed.to_le_bytes())?;

            // General purpose bit flag
            self.writer.write_all(&8u16.to_le_bytes())?;

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
            let extra_field_length = file.zip64_extra_field_size() + file.extended_timestamp_extra_field_size();
            self.writer.write_all(&extra_field_length.to_le_bytes())?;

            // File comment length
            self.writer.write_all(&0u16.to_le_bytes())?;

            // Disk number start, internal file attributes
            self.writer.write_all(&[0u8; 4])?;

            // External file attributes
            self.writer.write_all(&0u32.to_le_bytes())?;

            // Local header offset - use 0xFFFFFFFF if ZIP64
            let local_header_offset = file.local_header_offset.min(ZIP64_THRESHOLD_OFFSET) as u32;
            self.writer.write_all(&local_header_offset.to_le_bytes())?;

            // File name
            self.writer.write_all(file.name.as_bytes())?;

            // ZIP64 extended information extra field
            file.write_zip64_extra_field(&mut self.writer)?;
            
            // Extended timestamp extra field
            file.write_extended_timestamp_extra_field(&mut self.writer)?;
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
    name: String,
    local_header_offset: u64,
    compression_method: CompressionMethod,
    modification_time: Option<ZipDateTime>,
}

impl<'a, W> ZipEntryWriter<'a, W> {
    /// Creates a new `TrackingWriter` wrapping the given writer.
    pub(crate) fn new(
        inner: &'a mut ZipArchiveWriter<W>,
        name: String,
        local_header_offset: u64,
        compression_method: CompressionMethod,
        modification_time: Option<ZipDateTime>,
    ) -> Self {
        ZipEntryWriter {
            inner,
            compressed_bytes: 0,
            name,
            local_header_offset,
            compression_method,
            modification_time,
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
            modification_time: self.modification_time.clone(),
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
    name: String,
    compression_method: CompressionMethod,
    local_header_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    crc: u32,
    modification_time: Option<ZipDateTime>,
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

    /// Calculates the size of the extended timestamp extra field for this file header
    fn extended_timestamp_extra_field_size(&self) -> u16 {
        if self.modification_time.is_some() {
            9 // 2 bytes ID + 2 bytes size + 1 byte flags + 4 bytes timestamp
        } else {
            0
        }
    }

    /// Writes the extended timestamp extra field for this file header
    fn write_extended_timestamp_extra_field<W>(&self, writer: &mut W) -> Result<(), Error>
    where
        W: Write,
    {
        if let Some(ref datetime) = self.modification_time {
            let unix_time = datetime.to_unix();
            // Extended timestamp extra field header
            writer.write_all(&EXTENDED_TIMESTAMP_ID.to_le_bytes())?;
            writer.write_all(&5u16.to_le_bytes())?; // Size: 1 byte flags + 4 bytes timestamp
            writer.write_all(&1u8.to_le_bytes())?; // Flags: modification time present
            writer.write_all(&unix_time.to_le_bytes())?; // Unix timestamp
        }
        Ok(())
    }
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

/// Options for creating a new ZIP file entry.
///
/// The default compression method is `CompressionMethod::Store` (no compression).
#[derive(Debug, Clone)]
pub struct ZipEntryOptions {
    compression_method: CompressionMethod,
    modification_time: Option<ZipDateTime>,
}

impl Default for ZipEntryOptions {
    fn default() -> Self {
        ZipEntryOptions {
            compression_method: CompressionMethod::Store,
            modification_time: None,
        }
    }
}

impl ZipEntryOptions {
    /// Sets the compression method for the new file entry.
    pub fn compression_method(mut self, compression_method: CompressionMethod) -> Self {
        self.compression_method = compression_method;
        self
    }

    /// Sets the modification time for the new file entry.
    pub fn modification_time(mut self, modification_time: ZipDateTime) -> Self {
        self.modification_time = Some(modification_time);
        self
    }
}
