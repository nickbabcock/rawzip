//! Tests for data that sits between the last central directory entry and the
//! end of central directory record.
//!
//! Central directory iteration stops at the first non-central-directory-header
//! signature, so the slice and reader paths agree regardless of any trailing
//! bytes, and those bytes remain inspectable via `position()` /
//! `central_directory_end()`.

use rawzip::{RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipLocator};
use std::io::Write;

/// Build a small, well-formed two-entry zip with no prelude.
fn build_two_entry_zip() -> Vec<u8> {
    let mut data = Vec::new();
    let mut archive = rawzip::ZipArchiveWriter::new(&mut data);
    for name in ["alpha.txt", "beta.txt"] {
        let (mut entry, config) = archive.new_file(name).start().unwrap();
        let mut w = config.wrap(&mut entry);
        w.write_all(b"hello world").unwrap();
        let (_, descriptor) = w.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }
    archive.finish().unwrap();
    data
}

/// Offset of the final end of central directory record signature.
fn find_eocd(data: &[u8]) -> usize {
    let sig = [0x50, 0x4b, 0x05, 0x06];
    (0..=data.len() - 4)
        .rev()
        .find(|&i| data[i..i + 4] == sig)
        .expect("missing EOCD signature")
}

/// Splice `gap` bytes of `0xAB` immediately before the EOCD record, without
/// touching the declared central directory size.
fn splice_gap_before_eocd(data: &[u8], gap: usize) -> Vec<u8> {
    let eocd = find_eocd(data);
    let mut out = Vec::with_capacity(data.len() + gap);
    out.extend_from_slice(&data[..eocd]);
    out.extend(std::iter::repeat_n(0xABu8, gap));
    out.extend_from_slice(&data[eocd..]);
    out
}

fn slice_names(data: &[u8]) -> Vec<Vec<u8>> {
    let archive = ZipArchive::from_slice(data).unwrap();
    archive
        .entries()
        .map(|e| e.unwrap().file_path().as_ref().to_vec())
        .collect()
}

fn reader_names(data: &[u8]) -> Vec<Vec<u8>> {
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(data, &mut buf, data.len() as u64)
        .map_err(|(_, e)| e)
        .unwrap();
    let mut names = Vec::new();
    let mut entries = archive.entries(&mut buf);
    while let Some(entry) = entries.next_entry().unwrap() {
        names.push(entry.file_path().as_ref().to_vec());
    }
    names
}

/// Across a range of gap sizes (notably below, at, and above the 46-byte fixed
/// header size) the slice and reader paths must agree and yield exactly the two
/// declared entries.
#[test]
fn slice_and_reader_agree_with_trailing_gap() {
    let base = build_two_entry_zip();
    let expected: Vec<Vec<u8>> = vec![b"alpha.txt".to_vec(), b"beta.txt".to_vec()];

    for gap in [0usize, 1, 2, 4, 16, 30, 45, 46, 47, 100, 1000] {
        let data = splice_gap_before_eocd(&base, gap);
        let slice = slice_names(&data);
        let reader = reader_names(&data);
        assert_eq!(slice, reader, "slice/reader disagree at gap={gap}");
        assert_eq!(slice, expected, "unexpected entries at gap={gap}");
    }
}

/// The bytes between the last entry and the EOCD are recoverable from both
/// paths via `position()` and `central_directory_end()`.
#[test]
fn trailing_gap_is_recoverable() {
    let base = build_two_entry_zip();
    let gap = 37usize;
    let data = splice_gap_before_eocd(&base, gap);
    let expected_gap = vec![0xABu8; gap];

    // Slice path: the trailing region is a direct sub-slice.
    let archive = ZipArchive::from_slice(&data).unwrap();
    let mut entries = archive.entries();
    while entries.next_entry().unwrap().is_some() {}
    let start = entries.position() as usize;
    let end = archive.central_directory_end() as usize;
    assert_eq!(&data[start..end], expected_gap.as_slice());

    // Reader path: the same region, read out of the underlying reader.
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(&data[..], &mut buf, data.len() as u64)
        .map_err(|(_, e)| e)
        .unwrap();
    let (start, end) = {
        let mut entries = archive.entries(&mut buf);
        while entries.next_entry().unwrap().is_some() {}
        (entries.position(), archive.central_directory_end())
    };
    assert_eq!(end - start, gap as u64);
    assert_eq!(&data[start as usize..end as usize], expected_gap.as_slice());
}

/// A central directory larger than the read buffer forces the reader to refill
/// across buffer boundaries; the staged signature peek must still stop cleanly
/// at the trailing gap and agree with the slice path.
#[test]
fn large_directory_with_trailing_gap_across_buffer_boundaries() {
    let entry_count = 2000usize;
    let mut data = Vec::new();
    let mut archive = rawzip::ZipArchiveWriter::builder()
        .with_capacity(entry_count)
        .build(&mut data);
    for i in 0..entry_count {
        let name = format!("file_{i:05}.txt");
        let (mut entry, config) = archive.new_file(&name).start().unwrap();
        let mut w = config.wrap(&mut entry);
        w.write_all(b"x").unwrap();
        let (_, descriptor) = w.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }
    archive.finish().unwrap();

    let data = splice_gap_before_eocd(&data, 13);

    // Slice path.
    let archive = ZipArchive::from_slice(&data).unwrap();
    let mut slice_count = 0usize;
    let mut slice_entries = archive.entries();
    while slice_entries.next_entry().unwrap().is_some() {
        slice_count += 1;
    }
    assert_eq!(slice_count, entry_count);

    // Reader path with a small buffer to force a refill on nearly every entry.
    let mut buf = vec![0u8; 256];
    let archive = ZipLocator::new()
        .locate_in_reader(&data[..], &mut buf, data.len() as u64)
        .map_err(|(_, e)| e)
        .unwrap();
    let mut reader_count = 0usize;
    let position = {
        let mut entries = archive.entries(&mut buf);
        while entries.next_entry().unwrap().is_some() {
            reader_count += 1;
        }
        entries.position()
    };
    assert_eq!(reader_count, entry_count);
    assert_eq!(archive.central_directory_end() - position, 13);
}

/// Before any iteration, `position()` reports the start of the central
/// directory on both paths.
#[test]
fn position_starts_at_directory_offset() {
    let data = build_two_entry_zip();

    let archive = ZipArchive::from_slice(&data).unwrap();
    assert_eq!(archive.entries().position(), archive.directory_offset());

    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_slice(&data).unwrap();
    let reader_archive = ZipLocator::new()
        .locate_in_reader(&data[..], &mut buf, data.len() as u64)
        .map_err(|(_, e)| e)
        .unwrap();
    assert_eq!(
        reader_archive.entries(&mut buf).position(),
        archive.directory_offset()
    );
}
