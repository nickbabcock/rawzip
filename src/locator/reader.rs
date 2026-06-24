use super::*;
use crate::reader_at::{FileReader, ReaderAtExt};
use crate::{ReaderAt, ZipArchive};
use core::cell::RefCell;
use std::fs::File;
use std::io::Seek;

impl ZipLocator {
    /// Locates the EOCD record within a file.
    ///
    /// A mutable byte slice to use for reading data from the file. The buffer
    /// should be large enough to hold the EOCD record and potentially parts of
    /// the ZIP64 EOCD locator if present. A common size might be a few
    /// kilobytes.
    ///
    /// On failure, returns the original file and an `Error`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use rawzip::ZipLocator;
    /// use std::fs::File;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let file = File::open("assets/readme.zip")?;
    /// let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    /// let locator = ZipLocator::new();
    ///
    /// match locator.locate_in_file(file, &mut buffer) {
    ///     Ok(archive) => {
    ///         println!("Found EOCD in file, archive has {} files.", archive.entries_hint());
    ///     }
    ///     Err((_file, e)) => {
    ///         eprintln!("Failed to locate EOCD in file: {:?}", e);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn locate_in_file(
        &self,
        file: std::fs::File,
        buffer: &mut [u8],
    ) -> Result<ZipArchive<FileReader>, (File, Error)> {
        let mut reader = FileReader::from(file);
        let end_offset = match reader.seek(std::io::SeekFrom::End(0)) {
            Ok(offset) => offset,
            Err(e) => return Err((reader.into_inner(), Error::from(e))),
        };
        self.locate_in_reader(reader, buffer, end_offset)
            .map_err(|(fr, e)| (fr.into_inner(), e))
    }

    /// Locates the EOCD record in a reader, treating the specified end offset
    /// as the starting point when searching backwards.
    ///
    /// This method is useful for several scenarios:
    ///
    /// - Zip archive is nowhere near the end of the reader
    /// - Zip archives are concatenated
    ///
    /// For seekable readers, you can determine the end_offset by seeking to the
    /// end of the stream.
    ///
    /// Note that the zip locator may request data passed the end offset in
    /// order to read the entire end of the central directory record + comment.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use rawzip::{ZipLocator, FileReader};
    /// use std::fs::File;
    /// use std::io::Seek;
    ///
    /// # fn main() -> Result<(), rawzip::Error> {
    /// let file = File::open("assets/test.zip").unwrap();
    /// let mut reader = FileReader::from(file);
    /// let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    /// let locator = ZipLocator::new();
    ///
    /// // An example of determining the end offset when you don't
    /// // the length but have a seekable reader.
    /// let end_offset = reader.seek(std::io::SeekFrom::End(0)).unwrap();
    /// let archive = locator.locate_in_reader(reader, &mut buffer, end_offset)
    ///     .map_err(|(_, e)| e)?;
    ///
    /// // Maybe there is another zip archive to be found.
    /// // To find where the current archive starts, we need the minimum local header
    /// // offset. Below we are being conservative and iterating through the entire central
    /// // directory for the start offset, but in reality out of order central directories
    /// // are an edge case.
    /// let zip_start = {
    ///     let mut min_offset = u64::MAX;
    ///     let mut entries = archive.entries(&mut buffer);
    ///     while let Ok(Some(entry)) = entries.next_entry() {
    ///         min_offset = min_offset.min(entry.local_header_offset());
    ///     }
    ///     if min_offset == u64::MAX { 0 } else { min_offset }
    /// };
    /// match locator.locate_in_reader(archive.get_ref(), &mut buffer, zip_start) {
    ///    Ok(previous_archive) => {
    ///        println!("Found previous ZIP archive!");
    ///    }
    ///    Err((_, _)) => println!("No previous ZIP archive found"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn locate_in_reader<R>(
        &self,
        mut reader: R,
        buffer: &mut [u8],
        end_offset: u64,
    ) -> Result<ZipArchive<R>, (R, Error)>
    where
        R: ReaderAt,
    {
        let location_result =
            find_end_of_central_dir(&mut reader, buffer, self.max_search_space, end_offset);

        let (eocd_offset, buffer_pos, buffer_valid_len) = match location_result {
            Ok(Some(location_tuple)) => location_tuple,
            Ok(None) => {
                return Err((reader, Error::from(ErrorKind::MissingEndOfCentralDirectory)));
            }
            Err(error) => {
                return Err((reader, Error::io(error)));
            }
        };

        let (reader, mut eocd) = self
            .locate_in_reader_impl(reader, buffer, eocd_offset, buffer_pos, buffer_valid_len)
            .map_err(|(reader, e)| (reader, e.with_eocd_offset(eocd_offset)))?;

        // Check first entry in central directory, see
        // `ZipLocator::locate_in_byte_slice` for more info
        let first_entry = reader
            .read_exact_at(
                &mut buffer[..ZipFileHeaderFixed::SIZE],
                eocd.central_dir_offset,
            )
            .ok()
            .filter(|_| ZipFileHeaderFixed::parse(buffer).is_ok());

        match first_entry {
            None if !eocd.is_zip64() => {
                let cd_offset = eocd.eocd_offset.saturating_sub(eocd.central_dir_size);

                let first_entry = reader
                    .read_exact_at(&mut buffer[..ZipFileHeaderFixed::SIZE], cd_offset)
                    .ok()
                    .filter(|_| ZipFileHeaderFixed::parse(buffer).is_ok());

                if first_entry.is_some() {
                    eocd.base_offset = cd_offset.saturating_sub(eocd.central_dir_offset);
                    eocd.central_dir_offset = cd_offset;
                }

                Ok(ZipArchive::new(reader, eocd))
            }
            _ => Ok(ZipArchive::new(reader, eocd)),
        }
    }

    fn locate_in_reader_impl<R>(
        &self,
        reader: R,
        buffer: &mut [u8],
        eocd_offset: u64,
        buffer_pos: usize,
        buffer_valid_len: usize,
    ) -> Result<(R, EndOfCentralDirectory), (R, Error)>
    where
        R: ReaderAt,
    {
        // Most likely the single read to find the end of the central directory
        // will fill the buffer with entire end of the central directory (and
        // optionally zip64 end of central directory). So let's try and reuse
        // the the data already in memory as much as possible.
        let reader = Marker::new(reader);

        let mut end_of_central_directory = &buffer[buffer_pos..buffer_valid_len];
        let eocd = loop {
            match EndOfCentralDirectoryRecordFixed::parse(end_of_central_directory) {
                Ok(record) => break record,
                Err(e) if e.is_eof() => {
                    // Unhappy path: the end of central directory crossed over read boundaries
                    let read = reader.read_at_least_at(
                        buffer,
                        EndOfCentralDirectoryRecordFixed::SIZE,
                        eocd_offset,
                    );

                    let read = match read {
                        Ok(read) => read,
                        Err(e) => return Err((reader.inner, e)),
                    };

                    end_of_central_directory = &buffer[..read];
                }
                Err(e) => return Err((reader.inner, e)),
            }
        };

        let has_zip64_sentinel = eocd.has_zip64_sentinel();

        end_of_central_directory =
            &end_of_central_directory[EndOfCentralDirectoryRecordFixed::SIZE..];

        let comment_len = eocd.comment_len as usize;

        // Check if the rest of the buffer doesn't completely contain the comment.
        if end_of_central_directory.len() < comment_len {
            let pos = end_of_central_directory.len();
            let comment_offset =
                eocd_offset + EndOfCentralDirectoryRecordFixed::SIZE as u64 + pos as u64;
            let remaining_comment_len = comment_len - pos;

            // Try to read a single byte to validate the rest of the comment is accessible
            let mut temp_buf = [0u8; 1];
            let end_comment_offset = comment_offset + remaining_comment_len as u64 - 1;
            if let Err(e) = reader.read_exact_at(&mut temp_buf, end_comment_offset) {
                return Err((reader.inner, Error::io(e)));
            }
        }

        let eocd = EndOfCentralDirectoryRecord::from_parts(eocd_offset, eocd);
        if !has_zip64_sentinel {
            return match EndOfCentralDirectory::create(eocd) {
                Ok(eocd) => Ok((reader.inner, eocd)),
                Err(e) => Err((reader.inner, e)),
            };
        }

        let eocd64l_size = Zip64EndOfCentralDirectoryLocatorRecord::SIZE;

        // A sentinel EOCD field only hints at zip64. We need to support classic
        // zips with 65535 entries.
        if (eocd64l_size as u64) > eocd_offset {
            return match EndOfCentralDirectory::create(eocd) {
                Ok(eocd) => Ok((reader.inner, eocd)),
                Err(e) => Err((reader.inner, e)),
            };
        }

        // Unhappy path: if we needed to issue any reads since the original
        // eocd or don't have enough data in the buffer
        let eocd64l_pos = if reader.is_marked() || eocd64l_size > buffer_pos {
            let read = reader.read_exact_at(
                &mut buffer[..eocd64l_size],
                eocd_offset - eocd64l_size as u64,
            );

            match read {
                Ok(_) => 0,
                Err(e) => return Err((reader.inner, Error::io(e))),
            }
        } else {
            buffer_pos - eocd64l_size
        };

        let zip64l_eocd = &buffer[eocd64l_pos..eocd64l_pos + eocd64l_size];
        let zip64_locator = match Zip64EndOfCentralDirectoryLocatorRecord::parse(zip64l_eocd) {
            Ok(locator) => locator,
            Err(_) => {
                return match EndOfCentralDirectory::create(eocd) {
                    Ok(eocd) => Ok((reader.inner, eocd)),
                    Err(e) => Err((reader.inner, e)),
                };
            }
        };

        let zip64_eocd_fixed_size = Zip64EndOfCentralDirectoryRecord::SIZE;

        // Unhappy path: zip64 eocd is not in the original buffer
        let (eocd64_start, eocd64_end) = if reader.is_marked()
            || zip64_locator.directory_offset > eocd_offset
            || eocd_offset - zip64_locator.directory_offset > buffer_pos as u64
        {
            let read = reader.try_read_at_least_at(
                buffer,
                zip64_eocd_fixed_size,
                zip64_locator.directory_offset,
            );

            match read {
                Ok(read) => (0, read),
                Err(e) => {
                    return Err((reader.inner, Error::io(e)));
                }
            }
        } else {
            (
                buffer_pos - (eocd_offset - zip64_locator.directory_offset) as usize,
                buffer_valid_len,
            )
        };

        let zip64_eocd = &buffer[eocd64_start..eocd64_end];
        let zip64_record = match Zip64EndOfCentralDirectoryRecord::parse(zip64_eocd) {
            Ok(record) => record,
            Err(e) => return Err((reader.inner, e)),
        };

        // todo: zip64 extensible data sector

        let zip_eocd =
            Zip64EndOfCentralDirectory::from_parts(zip64_locator.directory_offset, zip64_record);
        match EndOfCentralDirectory::create_zip64(eocd, zip_eocd) {
            Ok(eocd) => Ok((reader.inner, eocd)),
            Err(e) => Err((reader.inner, e)),
        }
    }
}

struct Marker<T> {
    inner: T,
    marked: RefCell<bool>,
}

impl<T> Marker<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            marked: RefCell::new(false),
        }
    }

    fn is_marked(&self) -> bool {
        *self.marked.borrow()
    }
}

impl<T> ReaderAt for Marker<T>
where
    T: ReaderAt,
{
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        match self.inner.read_at(buf, offset) {
            Ok(n) if n > 0 => {
                *self.marked.borrow_mut() = true;
                Ok(n)
            }
            x => x,
        }
    }
}

impl<T> std::io::Seek for Marker<T>
where
    T: std::io::Seek,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

pub(super) const INIT_SCAN_WINDOW: usize = 1024;
pub(super) const MAX_SCAN_WINDOW: usize = 64 * 1024;

pub(super) fn find_end_of_central_dir<T>(
    reader: T,
    buffer: &mut [u8],
    max_search_space: u64,
    end_offset: u64,
) -> std::io::Result<Option<(u64, usize, usize)>>
where
    T: ReaderAt,
{
    if buffer.len() < END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len() {
        debug_assert!(false, "buffer not big enough to hold signature");
        return Ok(None);
    }

    // Cap the search space buffer to 64 KiB
    let buffer_len = buffer.len().min(MAX_SCAN_WINDOW);
    let buffer = &mut buffer[..buffer_len];

    let max_back = end_offset.saturating_sub(max_search_space);
    let mut chunk_end = end_offset;

    // The first search span is smaller as most zips do not have comments (or
    // have short comments if present).
    let mut window = INIT_SCAN_WINDOW.min(buffer.len());
    while chunk_end > max_back {
        let read_size = window.min((chunk_end - max_back) as usize);
        let chunk_start = chunk_end - read_size as u64;
        let haystack = &mut buffer[..read_size];

        reader.read_exact_at(haystack, chunk_start)?;

        if let Some(i) = rfind::<END_OF_CENTRAL_DIR_SIGNAUTRE>(haystack) {
            return Ok(Some((chunk_start + i as u64, i, read_size)));
        }

        if chunk_start == max_back {
            break;
        }

        // Instead of worrying about copying data around in the buffer, just
        // re-read a few bytes again.
        chunk_end = chunk_start + END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len() as u64 - 1;

        // first iteration used a smaller window, so on future iterations let's
        // go larger.
        window = buffer.len();
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;
    use rstest::rstest;
    use std::io::Cursor;

    #[rstest]
    #[case(&[], 4, 1000, None)]
    #[case(&[6], 4, 1000, None)]
    #[case(&[5, 6], 4, 1000, None)]
    #[case(&[b'K', 5, 6], 4, 1000, None)]
    #[case(&[0, 6, 0, 0, 0], 4, 1000, None)]
    #[case(&[b'P', b'K', 5, 6], 4, 1000, Some(0))]
    #[case(&[b'P', b'K', 5, 6], 5, 1000, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 5, 6], 5, 1000, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 6, 0, 0, 0], 4, 1000, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 0, 0, 0, 0], 4, 1000, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 0, 0, 0], 4, 1000, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 0], 4, 1000, Some(0))]
    #[case(&[5, 6, b'P', b'K', 5, 6], 4, 1000, Some(2))]
    #[case(&[5, 6, b'P', b'K', 5, 6], 5, 1000, Some(2))]
    #[case(&[5, 6, b'P', b'K', 5, 6, 5, 6], 4, 1000, Some(2))]
    #[case(&[5, 6, b'P', b'K', 5, 6, 5, 6], 5, 1000, Some(2))]
    #[case(&[b'P', b'K', 5, 6, b'P', b'K', 5, 6, 5, 6], 5, 1000, Some(4))]
    #[case(&[b'P', b'K', 5, 6, b'P', b'K', 5, 6, 5, 6], 32, 1000, Some(4))]
    #[case(&[b'P', b'K', 5, 6], 5, 4, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 5, 6], 5, 5, None)]
    #[case(&[b'P', b'K', 5, 6, 6, 0, 0, 0], 4, 8, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 0, 0, 0], 4, 8, Some(0))]
    #[case(&[b'P', b'K', 5, 6, 0], 4, 4, None)]
    #[case(&[5, 6, b'P', b'K', 5, 6], 4, 4, Some(2))]
    #[case(&[5, 6, b'P', b'K', 5, 6], 5, 4, Some(2))]
    #[case(&[5, 6, b'P', b'K', 5, 6, 5, 6], 4, 4, None)]
    #[case(&[5, 6, b'P', b'K', 5, 6, 5, 6], 5, 4, None)]
    #[case(&[b'P', b'K', 5, 6, b'P', b'K', 5, 6, 5, 6], 5, 6, Some(4))]
    #[case(&[b'P', b'K', 5, 6, b'P', b'K', 5, 6, 5, 6], 32, 10, Some(4))]
    fn test_find_end_of_central_dir_signature_cases(
        #[case] input: &[u8],
        #[case] buffer_size: usize,
        #[case] max_search_space: u64,
        #[case] expected: Option<u64>,
    ) {
        let result = find_end_of_central_dir_signature(input, max_search_space as usize);
        assert_eq!(result.map(|x| x as u64), expected);

        let cursor = Cursor::new(input);
        let mut buffer = vec![0u8; buffer_size];
        let found =
            find_end_of_central_dir(cursor, &mut buffer, max_search_space, input.len() as u64)
                .unwrap();
        let found_result = found.map(|(a, _, _)| a);
        assert_eq!(found_result, expected);

        if expected.is_some() {
            let (_, buffer_pos, buffer_valid_len) = found.unwrap();
            assert!(buffer_valid_len > 0, "buffer_valid_len should be positive");
            assert!(
                buffer_valid_len <= buffer_size,
                "buffer_valid_len should not exceed buffer capacity"
            );
            assert!(
                buffer_pos < buffer_valid_len,
                "buffer_index should be within buffer_valid_len"
            );
            assert!(
                buffer_pos + END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len() <= buffer_valid_len,
                "signature should be within valid part of buffer"
            );
            assert_eq!(
                buffer[buffer_pos..buffer_pos + 4],
                END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES
            );
        }
    }

    #[quickcheck]
    fn test_find_end_of_central_dir_signature(mut data: Vec<u8>, offset: usize, chunk_size: u16) {
        if data.len() < 4 {
            return;
        }

        let max_search_space = END_OF_CENTRAL_DIR_MAX_OFFSET;
        let pos = (offset % data.len()).saturating_sub(END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len());
        data[pos..pos + 4].copy_from_slice(&END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES);

        let result = find_end_of_central_dir_signature(&data, max_search_space as usize).unwrap();

        let mut buffer = vec![0u8; chunk_size.max(4) as usize];
        let reader = std::io::Cursor::new(&data);
        let (index, buffer_index, buffer_valid_len) =
            find_end_of_central_dir(reader, &mut buffer, max_search_space, data.len() as u64)
                .unwrap()
                .unwrap();

        assert_eq!(index, result as u64);
        assert!(buffer_valid_len > 0, "buffer_valid_len should be positive");
        assert!(
            buffer_valid_len <= buffer.len(),
            "buffer_valid_len should not exceed buffer capacity"
        );
        assert!(
            buffer_index < buffer_valid_len,
            "buffer_index should be within buffer_valid_len"
        );
        assert!(
            buffer_index + END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len() <= buffer_valid_len,
            "signature should be within valid part of buffer"
        );
        assert_eq!(
            buffer[buffer_index..buffer_index + 4],
            END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES
        );
    }

    #[quickcheck]
    fn test_find_end_of_central_dir_signature_random(
        data: Vec<u8>,
        chunk_size: u16,
        max_search_space: u64,
    ) {
        let mem = find_end_of_central_dir_signature(&data, max_search_space as usize);

        let mut buffer = vec![0u8; chunk_size.max(4) as usize];
        let reader = std::io::Cursor::new(&data);
        let curse =
            find_end_of_central_dir(reader, &mut buffer, max_search_space, data.len() as u64)
                .unwrap();

        let mem_result = mem.map(|x| x as u64);
        let curse_result = curse.map(|(a, _, _)| a);
        assert_eq!(mem_result, curse_result);

        if let Some((_, buffer_index, buffer_valid_len)) = curse {
            assert!(buffer_valid_len > 0, "buffer_valid_len should be positive");
            assert!(
                buffer_valid_len <= buffer.len(),
                "buffer_valid_len should not exceed buffer capacity"
            );
            assert!(
                buffer_index < buffer_valid_len,
                "buffer_index should be within buffer_valid_len"
            );
            assert!(
                buffer_index + END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len() <= buffer_valid_len,
                "signature should be within valid part of buffer"
            );
        }
    }

    #[rstest]
    #[case(1)]
    #[case(2)]
    #[case(3)]
    #[case(4)]
    fn test_find_end_of_central_dir_grows_initial_scan_window(#[case] bytes_before_window: usize) {
        let mut data = vec![0u8; INIT_SCAN_WINDOW * 4];
        let expected = data.len() - INIT_SCAN_WINDOW - bytes_before_window;
        data[expected..expected + 4].copy_from_slice(&END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES);

        let mut buffer = vec![0xa5; MAX_SCAN_WINDOW * 2];
        let (offset, buffer_pos, buffer_valid_len) = find_end_of_central_dir(
            Cursor::new(&data),
            &mut buffer,
            END_OF_CENTRAL_DIR_MAX_OFFSET,
            data.len() as u64,
        )
        .unwrap()
        .unwrap();

        assert_eq!(offset, expected as u64);
        assert!(buffer_valid_len <= MAX_SCAN_WINDOW);
        assert!(buffer_pos + END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len() <= buffer_valid_len);
        assert_eq!(
            buffer[buffer_pos..buffer_pos + END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES.len()],
            END_OF_CENTRAL_DIR_SIGNAUTRE_BYTES
        );
        assert!(buffer[MAX_SCAN_WINDOW..].iter().all(|byte| *byte == 0xa5));
    }
}
