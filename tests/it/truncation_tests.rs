//! Regression tests for truncation and overflow of ZIP offsets and lengths.

use rawzip::extra_fields::ExtraFieldId;
use rawzip::{
    ErrorKind, Header, RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipArchiveWriter, ZipLocator,
};
use std::io::Cursor;

/// Offset that wraps to zero when narrowed on 32-bit targets.
const WRAPPED_OFFSET: u64 = 1 << 32;

/// Builds an empty entry whose local-header offset is 4 GiB.
fn wrapped_offset_zip() -> Vec<u8> {
    zip64_offset_zip(WRAPPED_OFFSET)
}

/// Builds an empty entry with the ZIP64 local-header offset `offset`.
fn zip64_offset_zip(offset: u64) -> Vec<u8> {
    let mut data = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut data);
    let (mut entry, config) = archive
        .new_file("a")
        .extra_field(ExtraFieldId::ZIP64, &offset.to_le_bytes(), Header::CENTRAL)
        .unwrap()
        .start()
        .unwrap();
    let writer = config.wrap(&mut entry);
    let (_, descriptor) = writer.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();

    // The writer cannot emit a false offset, so patch the central header.
    let directory_offset = ZipArchive::from_slice(&data).unwrap().directory_offset() as usize;
    data[directory_offset + 42..directory_offset + 46].copy_from_slice(&u32::MAX.to_le_bytes());
    data
}

#[test]
fn slice_local_header_offset_past_4gib_is_eof() {
    let data = wrapped_offset_zip();
    let archive = ZipArchive::from_slice(&data).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    assert_eq!(entry.local_header_offset(), WRAPPED_OFFSET);
    let wayfinder = entry.wayfinder();
    assert!(archive.get_entry(wayfinder).is_err());
}

#[test]
fn reader_local_header_offset_past_4gib_is_eof() {
    let data = wrapped_offset_zip();
    let archive = ZipArchive::from_slice(data).unwrap().into_reader_archive();
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let mut entries = archive.entries(&mut buffer);
    let entry = entries.next_entry().unwrap().unwrap();
    assert_eq!(entry.local_header_offset(), WRAPPED_OFFSET);
    let wayfinder = entry.wayfinder();
    assert!(archive.get_entry(wayfinder).is_err());
}

#[test]
fn max_search_space_past_4gib_finds_eocd() {
    let data = wrapped_offset_zip();
    // On 32-bit targets, these narrow to 0 and 10, too small for the 22-byte EOCD.
    for max_search_space in [WRAPPED_OFFSET, WRAPPED_OFFSET + 10, u64::MAX] {
        let locator = ZipLocator::new().max_search_space(max_search_space);
        assert!(locator.locate_in_slice(&data).is_ok());

        let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let end = data.len() as u64;
        assert!(
            locator
                .locate_in_reader(Cursor::new(&data), &mut buffer, end)
                .is_ok()
        );
    }
}

/// Builds a ZIP whose base and local-header offsets overflow `u64`.
fn overflow_offset_zip() -> Vec<u8> {
    const PRELUDE_LEN: u64 = 128;

    let mut data = vec![0u8; PRELUDE_LEN as usize];
    data.extend_from_slice(&zip64_offset_zip(u64::MAX - PRELUDE_LEN + 1));
    data
}

#[test]
fn slice_base_offset_overflow_is_eof() {
    // This used to panic in debug builds and wrap to 0 in release builds.
    let data = overflow_offset_zip();
    let archive = ZipArchive::from_slice(&data).unwrap();
    let mut entries = archive.entries();
    let err = entries.next_entry().unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Eof), "{err:?}");
}

#[test]
fn reader_base_offset_overflow_is_eof() {
    let data = overflow_offset_zip();
    let archive = ZipArchive::from_slice(data).unwrap().into_reader_archive();
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let mut entries = archive.entries(&mut buffer);
    let err = entries.next_entry().unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Eof), "{err:?}");
}
