use rawzip::{ZipArchive, ZipArchiveWriter};
use rstest::rstest;
use std::io::{Cursor, Write};

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

#[test]
fn test_zip_with_secret_prelude() {
    // A secret prelude is where a non zip64 file is preceded by data but is not
    // captured in the reported central directory offset. This is recoverable
    // (only for non zip64 files) as we can compare the expected eocd offset
    // with the actual offset.
    let cases = [("assets/test.zip", 2)];
    for (asset, entries_count) in cases {
        let data = std::fs::read(asset).unwrap();
        let data = [&[0u8; 1000], data.as_slice()].concat();
        let archive = rawzip::ZipArchive::from_slice(&data).unwrap();
        let zip_start_offset = find_zip_data_start_offset_slice(&archive);
        let extracted_prefix = &data[..zip_start_offset as usize];
        assert_eq!(extracted_prefix, &[0u8; 1000]);
        let entries: Vec<_> = archive.entries().collect();
        assert_eq!(entries.len(), entries_count);
    }

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    for (asset, entries_count) in cases {
        let data = std::fs::read(asset).unwrap();
        let data = [&[0u8; 1000], data.as_slice()].concat();
        let locator = rawzip::ZipLocator::new();
        let archive = locator
            .locate_in_reader(&data, &mut buf, data.len() as u64)
            .unwrap();
        let zip_start_offset = find_zip_data_start_offset_reader(&archive);
        let extracted_prefix = &data[..zip_start_offset as usize];
        assert_eq!(extracted_prefix, &[0u8; 1000]);
        let mut count = 0;
        let mut entries = archive.entries(&mut buf);
        while entries.next_entry().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, entries_count);
    }
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
        let filename = format!("file_{:05}.txt", i);
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
