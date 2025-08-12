use rawzip::{
    extra_fields::ExtraFieldId, Header, ZipArchive, ZipArchiveWriter, ZipDataWriter, ZipLocator,
};
use std::io::{Cursor, Write};

#[test]
fn test_extra_fields_comprehensive() {
    let mut output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut output);

    let my_custom_field = ExtraFieldId::new(0x6666);

    // File with extra fields only in the local file header
    let mut local_file = archive
        .new_file("video.mp4")
        .extra_field(my_custom_field, b"field1", Header::LOCAL)
        .unwrap()
        .create()
        .unwrap();
    let mut writer = ZipDataWriter::new(&mut local_file);
    writer.write_all(b"video data").unwrap();
    let (_, desc) = writer.finish().unwrap();
    local_file.finish(desc).unwrap();

    // File with extra fields only in the central directory
    let mut central_file = archive
        .new_file("document.pdf")
        .extra_field(my_custom_field, b"field2", Header::CENTRAL)
        .unwrap()
        .create()
        .unwrap();
    let mut writer = ZipDataWriter::new(&mut central_file);
    writer.write_all(b"PDF content").unwrap();
    let (_, desc) = writer.finish().unwrap();
    central_file.finish(desc).unwrap();

    // File with extra fields in both headers for maximum compatibility
    let mut both_file = archive
        .new_file("important.dat")
        .extra_field(my_custom_field, b"field3", Header::default())
        .unwrap()
        .create()
        .unwrap();
    let mut writer = ZipDataWriter::new(&mut both_file);
    writer.write_all(b"important data").unwrap();
    let (_, desc) = writer.finish().unwrap();
    both_file.finish(desc).unwrap();

    archive.finish().unwrap();

    // Read it back and verify both central directory and local headers
    let zip_data = output.into_inner();
    let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(&zip_data, &mut buffer, zip_data.len() as u64)
        .unwrap();
    let mut entries = archive.entries(&mut buffer);
    while let Some(entry) = entries.next_entry().unwrap() {
        // Test central directory extra fields
        let central_field_data = entry
            .extra_fields()
            .find(|(id, _)| *id == my_custom_field)
            .map(|(_, data)| data);

        // Get wayfinder to access local header
        let wayfinder = entry.wayfinder();
        let zip_entry = archive.get_entry(wayfinder).unwrap();

        // Test local header extra fields
        let mut local_buffer = vec![0u8; 1024];
        let local_header = zip_entry.local_header(&mut local_buffer).unwrap();
        let local_field_data = local_header
            .extra_fields()
            .find(|(id, _)| *id == my_custom_field)
            .map(|(_, data)| data);

        match entry.file_path().as_ref() {
            b"video.mp4" => {
                // LOCAL: not in central directory, but in local header
                assert_eq!(
                    central_field_data, None,
                    "LOCAL field should not be in central directory"
                );
                assert_eq!(
                    local_field_data,
                    Some(b"field1".as_slice()),
                    "LOCAL field should be in local header"
                );
            }
            b"document.pdf" => {
                // CENTRAL: in central directory, but not in local header
                assert_eq!(
                    central_field_data,
                    Some(b"field2".as_slice()),
                    "CENTRAL field should be in central directory"
                );
                assert_eq!(
                    local_field_data, None,
                    "CENTRAL field should not be in local header"
                );
            }
            b"important.dat" => {
                // DEFAULT: in both central directory and local header
                assert_eq!(
                    central_field_data,
                    Some(b"field3".as_slice()),
                    "DEFAULT field should be in central directory"
                );
                assert_eq!(
                    local_field_data,
                    Some(b"field3".as_slice()),
                    "DEFAULT field should be in local header"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn test_extra_field_size_limit() {
    let mut output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut output);

    // Test individual field size limit
    let large_data = vec![0u8; 65536]; // Exactly 1 byte too large
    let result = archive.new_file("test1.txt").extra_field(
        ExtraFieldId::new(0x1111),
        &large_data,
        Header::default(),
    );
    assert!(
        result.is_err(),
        "Should fail with oversized individual field"
    );

    // Test total accumulated size limit
    // Each extra field has 4 bytes overhead (2 bytes ID + 2 bytes length)
    // So we need multiple fields that total > 65535 bytes including overhead
    let field_data = vec![0u8; 16380]; // 16380 + 4 = 16384 bytes per field

    let builder = archive
        .new_file("test2.txt")
        .extra_field(ExtraFieldId::new(0x2222), &field_data, Header::default())
        .unwrap()
        .extra_field(ExtraFieldId::new(0x3333), &field_data, Header::default())
        .unwrap()
        .extra_field(ExtraFieldId::new(0x4444), &field_data, Header::default())
        .unwrap()
        .extra_field(ExtraFieldId::new(0x5555), &field_data, Header::default());

    // The fourth field should cause us to exceed 65535 bytes total
    // 4 * (16380 + 4) = 4 * 16384 = 65536 bytes (1 byte over limit)
    assert!(
        builder.is_err(),
        "Should fail when total extra field size exceeds limit"
    );
}

#[test]
fn test_extra_field_deduplication_behavior() {
    // Test that duplicate field IDs are not deduplicated (append-only behavior)
    let mut output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut output);

    let custom_field = ExtraFieldId::new(0x7777);

    let mut file = archive
        .new_file("duplicate.txt")
        .extra_field(custom_field, b"first", Header::default())
        .unwrap()
        .extra_field(custom_field, b"second", Header::default())
        .unwrap()
        .extra_field(custom_field, b"third", Header::default())
        .unwrap()
        .create()
        .unwrap();

    let mut writer = ZipDataWriter::new(&mut file);
    writer.write_all(b"test content").unwrap();
    let (_, desc) = writer.finish().unwrap();
    file.finish(desc).unwrap();

    archive.finish().unwrap();

    // Verify all three instances are present in central directory
    let zip_data = output.into_inner();
    let archive = ZipArchive::from_slice(&zip_data).unwrap();
    let entry = archive.entries().next().unwrap().unwrap();

    // Count instances in central directory
    let central_instances: Vec<_> = entry
        .extra_fields()
        .filter(|(id, _)| *id == custom_field)
        .map(|(_, data)| data)
        .collect();

    assert_eq!(
        central_instances.len(),
        3,
        "Should have 3 instances in central directory"
    );

    // Verify the order is preserved
    assert_eq!(central_instances[0], b"first");
    assert_eq!(central_instances[1], b"second");
    assert_eq!(central_instances[2], b"third");
}
