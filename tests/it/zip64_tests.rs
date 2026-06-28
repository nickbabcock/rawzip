use rawzip::{
    CompressionMethod, Crc32Option, RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipArchiveWriter,
    ZipLocator,
};
use rstest::rstest;
use std::io::{Cursor, Write};

// ZIP64 signatures to check for
const ZIP64_EOCD_SIGNATURE: u32 = 0x06064b50;
const ZIP64_EOCD_LOCATOR_SIGNATURE: u32 = 0x07064b50;

/// Helper function to check if ZIP64 structures are present in the archive
fn contains_zip64_signatures(data: &[u8]) -> bool {
    let zip64_eocd_sig_bytes = ZIP64_EOCD_SIGNATURE.to_le_bytes();
    let zip64_locator_sig_bytes = ZIP64_EOCD_LOCATOR_SIGNATURE.to_le_bytes();

    let has_eocd = data.windows(4).any(|w| w == zip64_eocd_sig_bytes);
    let has_locator = data.windows(4).any(|w| w == zip64_locator_sig_bytes);

    has_eocd && has_locator
}

fn verify_expected_entries(data: &[u8], expected_count: u64) {
    // Verify with slice
    let read_archive = ZipArchive::from_slice(data).unwrap();
    assert_eq!(read_archive.entries_hint(), expected_count);
    let entries = read_archive.entries();
    let mut count = 0;
    for _ in entries {
        count += 1;
    }
    assert_eq!(count, expected_count as usize);

    // Verify with reader
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let read_archive = ZipArchive::from_seekable(Cursor::new(data), &mut buffer).unwrap();
    assert_eq!(read_archive.entries_hint(), expected_count);
    let mut entries = read_archive.entries(&mut buffer);
    let mut count = 0;
    while entries.next_entry().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, expected_count as usize);
}

/// Test ZIP64 threshold behavior with different entry counts
#[rstest]
#[case(65534, false)]
#[case(65535, true)]
#[case(65536, true)]
fn test_zip64_threshold_entries(#[case] entry_count: usize, #[case] should_be_zip64: bool) {
    let output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::builder()
        .with_capacity(entry_count)
        .build(output);

    for i in 0..entry_count {
        let filename = format!("file_{i:05}.txt");
        let (mut entry, config) = archive.new_file(&filename).start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"x").unwrap();
        let (_, descriptor_output) = writer.finish().unwrap();

        entry.finish(descriptor_output).unwrap();
    }

    let writer = archive.finish().unwrap();
    let data = writer.into_inner();

    let archive_type = if should_be_zip64 {
        "ZIP64"
    } else {
        "standard ZIP"
    };
    println!("Created {archive_type} archive with {entry_count} entries");

    // Verify ZIP64 signatures presence matches expectation
    let has_zip64 = contains_zip64_signatures(&data);
    assert_eq!(
        has_zip64, should_be_zip64,
        "{entry_count} entries expected zip64: {should_be_zip64}"
    );

    verify_expected_entries(&data, entry_count as u64);
}

// `zip64-cd-size-sentinel.zip`: two empty files and a classic EOCD whose
// central-directory-size field is the 0xFFFFFFFF sentinel, paired with a valid
// zip64 EOCD record + locator carrying the true (small) size.
//
// Cross-validated by 7-Zip, python zipfile, and unzip.
//
// Go bug: golang/go#56249
#[test]
fn read_zip64_from_cd_size_sentinel() {
    let data = std::fs::read("assets/zip64-cd-size-sentinel.zip").unwrap();
    verify_expected_entries(&data, 2);
}

fn is_all_zero(buf: &[u8]) -> bool {
    const ZEROS: [u8; 256] = [0u8; 256];
    let mut chunks = buf.chunks_exact(ZEROS.len());
    chunks.all(|chunk| chunk == ZEROS) && chunks.remainder().iter().all(|&b| b == 0)
}

/// A `Write` sink recording only non-zero writes plus a running byte count.
/// All-zero writes (the multi-GiB filler) are dropped, keeping just their length.
/// This technique has been borrowed from go's zip64_sparse_test.go.
#[derive(Default)]
struct SparseBuffer {
    size: u64,
    spans: Vec<(u64, Vec<u8>)>,
}

impl Write for SparseBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !is_all_zero(buf) {
            self.spans.push((self.size, buf.to_vec()));
        }
        self.size += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A `ReaderAt` view over the recorded spans, zeros elsewhere.
struct SparseFile {
    size: u64,
    spans: Vec<(u64, Vec<u8>)>,
}

impl rawzip::ReaderAt for SparseFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }
        let end = (offset + buf.len() as u64).min(self.size);
        let n = (end - offset) as usize;
        buf[..n].fill(0);
        for (span_off, data) in &self.spans {
            let span_end = span_off + data.len() as u64;
            if span_end <= offset || *span_off >= end {
                continue;
            }
            let from = (*span_off).max(offset);
            let to = span_end.min(end);
            let dst = (from - offset) as usize..(to - offset) as usize;
            let src = (from - span_off) as usize..(to - span_off) as usize;
            buf[dst].copy_from_slice(&data[src]);
        }
        Ok(n)
    }
}

#[test]
fn directory_entry_past_4gib_offset_round_trips() {
    let mut sink = SparseBuffer::default();
    let mut archive = ZipArchiveWriter::new(&mut sink);

    // A >4 GiB stored entry so the following directory's local header offset
    // trips the ZIP64 offset threshold. CRC is skipped so we don't hash 4 GiB.
    let (mut entry, config) = archive
        .new_file("filler.bin")
        .compression_method(CompressionMethod::STORE)
        .crc32(Crc32Option::Skip)
        .start()
        .unwrap();
    let mut data_writer = config.wrap(&mut entry);
    let zeros = vec![0u8; 1024 * 1024];
    for _ in 0..4096 {
        data_writer.write_all(&zeros).unwrap();
    }
    let (_, output) = data_writer.finish().unwrap();
    entry.finish(output).unwrap();

    archive.new_dir("past_4gib/").create().unwrap();
    archive.finish().unwrap();

    let sparse = SparseFile {
        size: sink.size,
        spans: sink.spans,
    };
    assert!(
        sparse.size > u32::MAX as u64,
        "archive should span past 4 GiB, got {}",
        sparse.size
    );

    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let end = sparse.size;
    let archive = ZipLocator::new()
        .locate_in_reader(sparse, &mut buffer, end)
        .map_err(|(_, e)| e)
        .unwrap();

    let mut dir = None;
    let mut entries = archive.entries(&mut buffer);
    while let Some(entry) = entries.next_entry().unwrap() {
        if entry.file_path().as_ref() == b"past_4gib/" {
            dir = Some(entry.wayfinder());
        }
    }
    let dir = dir.expect("past_4gib/ directory entry present in central directory");

    // Assert that we can seek and read the local header
    archive
        .get_entry(dir)
        .expect("directory local header must be reachable");
}
