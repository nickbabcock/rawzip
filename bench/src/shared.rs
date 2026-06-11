use rawzip::{time::UtcDateTime, ReaderAt, ZipArchive, ZipArchiveWriter, ZipSliceArchive};
use std::io::{Cursor, Write};

pub(crate) const SIZE_CASES: [usize; 9] = [1, 4, 16, 64, 256, 1024, 4096, 16384, 65536];
pub(crate) const LARGE_ENTRY_COUNT: usize = 200_000;
pub(crate) const WRITER_ENTRY_COUNT: usize = 5_000;
pub(crate) const STORED_FILE_DATA: &[u8] = b"x";

pub struct ReaderFixture {
    pub zip_data: Vec<u8>,
    pub buffer: Vec<u8>,
    pub end_offset: u64,
    pub expected_total_size: u64,
}

pub struct SliceEntriesFixture {
    pub archive: ZipSliceArchive<Vec<u8>>,
    pub expected_total_size: u64,
}

pub struct ReaderEntriesFixture {
    pub archive: ZipArchive<Cursor<Vec<u8>>>,
    pub buffer: Vec<u8>,
    pub expected_total_size: u64,
}

pub struct WriterFixture {
    pub file_names: Vec<String>,
    pub last_modified: Option<UtcDateTime>,
    pub output: Vec<u8>,
}

pub fn filled_bytes<const BYTE: u8>(size: usize) -> Vec<u8> {
    vec![BYTE; size]
}

/// Repeating near-miss of the EOCD signature: `0x50 0x4b 0xff 0x06`.
pub fn near_miss_bytes(size: usize) -> Vec<u8> {
    const PATTERN: [u8; 4] = [0x50, 0x4b, 0xff, 0x06];
    (0..size).map(|i| PATTERN[i % PATTERN.len()]).collect()
}

pub fn deterministic_random_bytes(size: usize) -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173u64;
    let mut output = Vec::with_capacity(size);

    for _ in 0..size {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        output.push(state.wrapping_mul(0x2545_f491_4f6c_dd1d) as u8);
    }

    output
}

pub fn setup_reader_fixture(entry_count: usize) -> ReaderFixture {
    let zip_data = create_test_zip(entry_count);
    ReaderFixture {
        end_offset: zip_data.len() as u64,
        expected_total_size: expected_total_size(entry_count),
        zip_data,
        buffer: vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE],
    }
}

pub fn setup_slice_entries_fixture(entry_count: usize) -> SliceEntriesFixture {
    let zip_data = create_test_zip(entry_count);
    let archive = rawzip::ZipArchive::from_slice(zip_data).unwrap();

    SliceEntriesFixture {
        archive,
        expected_total_size: expected_total_size(entry_count),
    }
}

pub fn setup_reader_entries_fixture(entry_count: usize) -> ReaderEntriesFixture {
    let zip_data = create_test_zip(entry_count);
    let archive = rawzip::ZipArchive::from_slice(zip_data)
        .unwrap()
        .into_cursor_archive();

    ReaderEntriesFixture {
        archive,
        buffer: vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE],
        expected_total_size: expected_total_size(entry_count),
    }
}

pub fn setup_writer_fixture_with_timestamps(entry_count: usize) -> WriterFixture {
    WriterFixture {
        file_names: file_names(entry_count),
        last_modified: Some(default_timestamp()),
        output: Vec::with_capacity(entry_count * 128),
    }
}

pub fn setup_writer_fixture_minimal(entry_count: usize) -> WriterFixture {
    WriterFixture {
        file_names: file_names(entry_count),
        last_modified: None,
        output: Vec::with_capacity(entry_count * 96),
    }
}

pub fn sum_slice_entries<T: AsRef<[u8]>>(archive: &ZipSliceArchive<T>) -> u64 {
    let mut total_size = 0u64;
    let mut entries = archive.entries();
    while let Ok(Some(entry)) = entries.next_entry() {
        total_size += entry.uncompressed_size_hint();
    }
    total_size
}

pub fn sum_reader_entries<R: ReaderAt>(archive: &ZipArchive<R>, buffer: &mut [u8]) -> u64 {
    let mut total_size = 0u64;
    let mut entries = archive.entries(buffer);
    while let Ok(Some(entry)) = entries.next_entry() {
        total_size += entry.uncompressed_size_hint();
    }
    total_size
}

pub fn create_test_zip(entry_count: usize) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::builder()
        .with_capacity(entry_count)
        .build(&mut output);

    for file_name in file_names(entry_count) {
        let (mut entry, config) = archive
            .new_file(&file_name)
            .compression_method(rawzip::CompressionMethod::Store)
            .start()
            .unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(STORED_FILE_DATA).unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }

    archive.finish().unwrap();
    output.into_inner()
}

fn file_names(entry_count: usize) -> Vec<String> {
    (0..entry_count)
        .map(|index| format!("file{index:06}.txt"))
        .collect()
}

fn expected_total_size(entry_count: usize) -> u64 {
    entry_count as u64 * STORED_FILE_DATA.len() as u64
}

fn default_timestamp() -> UtcDateTime {
    UtcDateTime::from_components(2000, 1, 1, 0, 0, 0, 0).unwrap()
}
