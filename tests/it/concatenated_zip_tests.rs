use rawzip::{ZipArchive, ZipArchiveWriter, ZipDataWriter};
use std::io::Write;

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
            let mut file = archive.new_file("first.txt").create().unwrap();
            let mut writer = ZipDataWriter::new(&mut file);
            writer.write_all(b"First ZIP content").unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            file.finish(descriptor).unwrap();
            archive.finish().unwrap();
        }

        // Second ZIP with prefix
        data.extend_from_slice(b"PREFIX_FOR_SECOND_ZIP\n");
        {
            let mut archive = ZipArchiveWriter::new(&mut data);
            let mut file = archive.new_file("second.txt").create().unwrap();
            let mut writer = ZipDataWriter::new(&mut file);
            writer.write_all(b"Second ZIP content").unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            file.finish(descriptor).unwrap();
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

    // Realize that the base offset is not zero so there is prefix data
    assert_ne!(second_archive.base_offset(), 0);

    // Attempt to see if there are additional zips in the data. In this test we
    // could just pass a subset of the slice to the locator
    // `ZipArchive::from_slice`, but let's emulate what the code would look like
    // if it was a 100GB file.
    let locator = rawzip::ZipLocator::new();
    let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let reader = std::io::Cursor::new(&data);
    let first_archive = locator
        .locate_in_reader(reader, &mut buffer, second_archive.base_offset())
        .unwrap();
    let first_base_offset = first_archive.base_offset();

    // Verify prefix data extraction
    let prefix = &data[..first_base_offset as usize];
    assert_eq!(prefix, b"PREFIX_FOR_FIRST_ZIP\n");

    let mut entries_iter = first_archive.entries(&mut buffer);
    let entry = entries_iter.next_entry().unwrap().unwrap();
    assert_eq!(entry.file_path().as_ref(), b"first.txt");

    // Verify that we can also recover the prefix data for the second ZIP
    let first_archive_end = first_archive.end_offset();
    let second_prefix = &data[first_archive_end as usize..second_archive.base_offset() as usize];
    assert_eq!(second_prefix, b"PREFIX_FOR_SECOND_ZIP\n");
}
