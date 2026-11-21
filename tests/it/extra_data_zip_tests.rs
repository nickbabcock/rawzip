use rawzip::path::EntryPath;
use rawzip::{ZipArchive, ZipArchiveWriter};
use rstest::rstest;
use std::io::{Cursor, Write};

/// Size of an end-of-central-directory record that carries no comment.
const EOCD_SIZE: usize = 22;

/// A digital signature record (`PK\x05\x05`) wrapping `payload`.
pub(crate) fn digital_signature_record(payload: &[u8]) -> Vec<u8> {
    let mut record = Vec::from(*b"PK\x05\x05");
    record.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    record.extend_from_slice(payload);
    record
}

/// Build a single-entry zip, splicing `trailer` between the central directory
/// and the EOCD and growing the recorded central directory size to cover it.
///
/// The writer never emits a digital signature record itself, so we construct
/// one here where every offset is known by construction rather than recovered
/// by scanning the bytes.
pub(crate) fn zip_with_cd_trailer<'a>(name: impl Into<EntryPath<'a>>, trailer: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let mut archive = ZipArchiveWriter::new(&mut data);
        let (mut entry, config) = archive.new_file(name).start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"contents").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    let eocd = data.len() - EOCD_SIZE;
    let cd_size = u32::from_le_bytes(data[eocd + 12..eocd + 16].try_into().unwrap());
    data[eocd + 12..eocd + 16].copy_from_slice(&(cd_size + trailer.len() as u32).to_le_bytes());
    data.splice(eocd..eocd, trailer.iter().copied());
    data
}

/// Collect the entry file paths, asserting the slice and reader readers agree.
fn entry_paths(data: &[u8]) -> Vec<Vec<u8>> {
    let slice: Vec<Vec<u8>> = ZipArchive::from_slice(data)
        .unwrap()
        .entries()
        .map(|entry| entry.unwrap().file_path().as_ref().to_vec())
        .collect();

    let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = rawzip::ZipLocator::new()
        .locate_in_reader(data, &mut buffer, data.len() as u64)
        .unwrap();
    let mut entries = archive.entries(&mut buffer);
    let mut reader = Vec::new();
    while let Some(entry) = entries.next_entry().unwrap() {
        reader.push(entry.file_path().as_ref().to_vec());
    }

    assert_eq!(slice, reader, "slice and reader readers disagree");
    slice
}

/// Helper function to find the start of ZIP data by finding the minimum local header offset
fn find_zip_data_start_offset_slice<T: AsRef<[u8]>>(archive: &rawzip::ZipSliceArchive<T>) -> u64 {
    archive
        .entries()
        .map(|x| x.unwrap().local_header_offset())
        .min()
        .unwrap_or(archive.directory_offset())
}

/// Helper function to find the start of ZIP data by finding the minimum local header offset
fn find_zip_data_start_offset_reader<R: rawzip::ReaderAt>(archive: &rawzip::ZipArchive<R>) -> u64 {
    let mut min_offset = archive.directory_offset();

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let mut entries = archive.entries(&mut buf);

    while let Some(entry) = entries.next_entry().unwrap() {
        min_offset = min_offset.min(entry.local_header_offset());
    }

    min_offset
}

/// Test basic concatenated ZIP functionality: two ZIP files with prefix data
#[test]
fn test_concatenated_zip_files() {
    // Create two concatenated ZIP files with prefix data
    let data = {
        let mut data = Vec::new();

        // First ZIP with prefix
        data.extend_from_slice(b"PREFIX_FOR_FIRST_ZIP\n");
        {
            let mut archive = ZipArchiveWriter::new(&mut data);
            let (mut entry, config) = archive.new_file("first.txt").start().unwrap();
            let mut writer = config.wrap(&mut entry);
            writer.write_all(b"First ZIP content").unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            entry.finish(descriptor).unwrap();
            archive.finish().unwrap();
        }

        // Second ZIP with prefix
        data.extend_from_slice(b"PREFIX_FOR_SECOND_ZIP\n");
        {
            let mut archive = ZipArchiveWriter::new(&mut data);
            let (mut entry, config) = archive.new_file("second.txt").start().unwrap();
            let mut writer = config.wrap(&mut entry);
            writer.write_all(b"Second ZIP content").unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            entry.finish(descriptor).unwrap();
            archive.finish().unwrap();
        }
        data
    };

    // Start off by reading the zip as one normally does
    let second_archive = ZipArchive::from_slice(&data).unwrap();

    // Verify that the last concatenated ZIP would be detected first
    let entries: Vec<_> = second_archive.entries().collect();
    assert_eq!(entries.len(), 1);
    let entry = entries[0].as_ref().unwrap();
    assert_eq!(entry.file_path().as_ref(), b"second.txt");

    // Find the start of the second ZIP's data by getting the minimum local header offset
    let second_zip_start = find_zip_data_start_offset_slice(&second_archive);

    // Realize that the zip data start is not zero so there is prefix data
    assert_ne!(second_zip_start, 0);

    // Attempt to see if there are additional zips in the data. In this test we
    // could just pass a subset of the slice to the locator
    // `ZipArchive::from_slice`, but let's emulate what the code would look like
    // if it was a 100GB file.
    let locator = rawzip::ZipLocator::new();
    let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let reader = std::io::Cursor::new(&data);
    let first_archive = locator
        .locate_in_reader(reader, &mut buffer, second_zip_start)
        .unwrap();
    let first_zip_start = find_zip_data_start_offset_reader(&first_archive);

    // Verify prefix data extraction
    let prefix = &data[..first_zip_start as usize];
    assert_eq!(prefix, b"PREFIX_FOR_FIRST_ZIP\n");

    let mut entries_iter = first_archive.entries(&mut buffer);
    let entry = entries_iter.next_entry().unwrap().unwrap();
    assert_eq!(entry.file_path().as_ref(), b"first.txt");

    // Verify that we can also recover the prefix data for the second ZIP
    let first_archive_end = first_archive.end_offset();
    let second_prefix = &data[first_archive_end as usize..second_zip_start as usize];
    assert_eq!(second_prefix, b"PREFIX_FOR_SECOND_ZIP\n");
}

/// A secret prelude is prepended data the archive's offsets omit. The 64-byte
/// buffer forces the reader to read the zip64 records instead of reusing them.
#[rstest]
fn test_zip_with_secret_prelude(
    #[values(
        "assets/test.zip",
        "assets/zip64.zip",
        "assets/missing-zip64-sentinels.zip"
    )]
    asset: &str,
    #[values(64, rawzip::RECOMMENDED_BUFFER_SIZE)] locator_buffer_size: usize,
) {
    let original = std::fs::read(asset).unwrap();
    let entries_count = ZipArchive::from_slice(&original).unwrap().entries().count();
    let data = [&[0u8; 1000], original.as_slice()].concat();

    let archive = ZipArchive::from_slice(&data).unwrap();
    assert_eq!(find_zip_data_start_offset_slice(&archive), 1000);
    assert_eq!(
        archive
            .entries()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .len(),
        entries_count
    );

    let mut buf = vec![0u8; locator_buffer_size];
    let archive = rawzip::ZipLocator::new()
        .locate_in_reader(&data, &mut buf, data.len() as u64)
        .unwrap();
    assert_eq!(find_zip_data_start_offset_reader(&archive), 1000);
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let mut entries = archive.entries(&mut buf);
    let mut count = 0;
    while entries.next_entry().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, entries_count);
}

#[rstest]
#[case(0)]
#[case(100)]
#[case(65536)]
fn test_zip_declared_prelude(#[case] entry_count: usize) {
    let mut output = Cursor::new(Vec::new());
    output.write_all(&[0u8; 1000]).unwrap();
    let mut archive = rawzip::ZipArchiveWriter::builder()
        .with_offset(output.position())
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

    let archive = rawzip::ZipArchive::from_slice(&data).unwrap();
    let zip_start_offset = find_zip_data_start_offset_slice(&archive);
    assert_eq!(zip_start_offset, 1000);
    assert_eq!(archive.entries().count(), entry_count);
}

#[test]
fn central_directory_digital_signature() {
    let data = zip_with_cd_trailer("README", &digital_signature_record(b"signed"));
    assert_eq!(entry_paths(&data), [b"README".to_vec()]);
}

#[test]
fn truncated_central_directory_digital_signature() {
    // A degenerate record: only the 4-byte marker, with no size or payload.
    let data = zip_with_cd_trailer("README", b"PK\x05\x05");
    assert_eq!(entry_paths(&data), [b"README".to_vec()]);
}

#[test]
fn digital_signature_marker_inside_record_data() {
    // The marker appears both in a file name and inside the signature payload;
    // neither should be mistaken for the terminator.
    let name = EntryPath::verbatim(b"PK\x05\x05ME".as_slice());
    let data = zip_with_cd_trailer(name, &digital_signature_record(b"PK\x05\x05"));
    assert_eq!(entry_paths(&data), [b"PK\x05\x05ME".to_vec()]);
}

/// A buffer too small for the shifted zip64 record is an error, not a silent
/// fallback to a classic EOCD that can describe a different directory
#[rstest]
fn secret_prelude_with_buffer_smaller_than_zip64_record(#[values(46, 55)] buffer_size: usize) {
    let original = std::fs::read("assets/missing-zip64-sentinels.zip").unwrap();
    let data = [&[0u8; 1000], original.as_slice()].concat();
    let mut buf = vec![0u8; buffer_size];
    let (_, err) = rawzip::ZipLocator::new()
        .locate_in_reader(&data, &mut buf, data.len() as u64)
        .unwrap_err();
    assert!(matches!(
        err.kind(),
        rawzip::ErrorKind::BufferTooSmall { required: 56 }
    ));
}

/// Returns the central directory range declared by the zip64 end of central
/// directory record of `data`, an archive without a comment
fn declared_zip64_central_directory(data: &[u8]) -> std::ops::Range<usize> {
    let record = data.len() - EOCD_SIZE - 20 - 56;
    let field = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap()) as usize;
    let (size, offset) = (field(record + 40), field(record + 48));
    offset..offset + size
}

/// A central directory planted where the stale offsets point is ignored, as in
/// CPython and Info-ZIP
#[test]
fn secret_zip64_prelude_ignores_central_directory_in_prelude() {
    let original = std::fs::read("assets/zip64.zip").unwrap();
    let central_directory = declared_zip64_central_directory(&original);
    let planted = original[central_directory.clone()]
        .iter()
        .map(|&b| if b == b'R' { b'X' } else { b })
        .collect::<Vec<_>>();

    let mut data = vec![b'#'; central_directory.end + 100];
    data[central_directory].copy_from_slice(&planted);
    data.extend_from_slice(&original);

    assert_eq!(entry_paths(&data), [b"README".to_vec()]);
}

/// No shifted lookup when the declared offset has the zip64 signature, as in
/// CPython after CVE-2025-8291
#[test]
fn secret_zip64_prelude_requires_declared_offset_without_zip64_signature() {
    let original = std::fs::read("assets/zip64.zip").unwrap();
    let declared = original.len() - EOCD_SIZE - 20 - 56;
    let mut data = vec![0u8; 1000];
    data[declared..declared + 4].copy_from_slice(b"PK\x06\x06");
    data.extend_from_slice(&original);

    // The classic record holds sentinels, so falling back to it fails
    let err = ZipArchive::from_slice(&data).unwrap_err();
    assert!(matches!(
        err.kind(),
        rawzip::ErrorKind::InvalidEndOfCentralDirectory
    ));

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let (_, err) = rawzip::ZipLocator::new()
        .locate_in_reader(&data, &mut buf, data.len() as u64)
        .unwrap_err();
    assert!(matches!(
        err.kind(),
        rawzip::ErrorKind::InvalidEndOfCentralDirectory
    ));
}

/// Asserts that both locators reject `data`, an archive with sentinels whose
/// zip64 record was not recovered
fn assert_zip64_not_recovered(data: &[u8]) {
    let err = ZipArchive::from_slice(data).unwrap_err();
    assert!(matches!(
        err.kind(),
        rawzip::ErrorKind::InvalidEndOfCentralDirectory
    ));

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let (_, err) = rawzip::ZipLocator::new()
        .locate_in_reader(data, &mut buf, data.len() as u64)
        .unwrap_err();
    assert!(matches!(
        err.kind(),
        rawzip::ErrorKind::InvalidEndOfCentralDirectory
    ));
}

/// Without this check the declared offset alone would position the central
/// directory, here onto a copy in the prelude
#[test]
fn secret_zip64_prelude_requires_central_directory_to_end_at_declared_offset() {
    let original = std::fs::read("assets/zip64.zip").unwrap();
    let central_directory = declared_zip64_central_directory(&original);
    let shift = 500;
    let mut data = vec![b'#'; 1000];
    data[central_directory.start + 1000 - shift..][..central_directory.len()]
        .copy_from_slice(&original[central_directory.clone()]);
    data.extend_from_slice(&original);

    let locator = data.len() - EOCD_SIZE - 20;
    let declared = (central_directory.end + shift) as u64;
    data[locator + 8..locator + 16].copy_from_slice(&declared.to_le_bytes());
    assert_zip64_not_recovered(&data);
}

/// A declared offset inside the zip64 record is not a prelude
#[test]
fn secret_zip64_prelude_requires_declared_offset_before_record() {
    let mut data = std::fs::read("assets/zip64.zip").unwrap();
    let record = data.len() - EOCD_SIZE - 20 - 56;
    let locator = record + 56;
    let size = u64::from_le_bytes(data[record + 40..record + 48].try_into().unwrap());
    data[record + 40..record + 48].copy_from_slice(&(size + 4).to_le_bytes());
    data[locator + 8..locator + 16].copy_from_slice(&(record as u64 + 4).to_le_bytes());
    assert_zip64_not_recovered(&data);
}
