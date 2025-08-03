use rawzip::ZipLocator;

/// Test handling of false EOCD signatures using the slice API
#[test]
fn test_false_signature_in_slice() {
    let mut zip_data = std::fs::read("assets/test.zip").expect("Failed to read test.zip");
    zip_data.extend_from_slice(b"This some trailing data: ");
    zip_data.extend_from_slice(&0x06054b50u32.to_le_bytes());
    zip_data.extend_from_slice(b" oh my!\n");

    let locator = ZipLocator::new();
    let (_, e) = locator.locate_in_slice(&zip_data).unwrap_err();
    let offset = e.eocd_offset().unwrap();
    assert_eq!(offset, 1195);

    // Test that we can locate the real zip
    let archive = locator
        .locate_in_slice(&zip_data[..offset.saturating_sub(1) as usize])
        .unwrap();
    assert_eq!(archive.comment().as_bytes(), b"This is a zipfile comment.");
}

/// Test handling of false signatures using the reader API
#[test]
fn test_false_signature_in_reader() {
    let mut zip_data = std::fs::read("assets/test.zip").expect("Failed to read test.zip");
    zip_data.extend_from_slice(b"This some trailing data: ");
    zip_data.extend_from_slice(&0x06054b50u32.to_le_bytes());
    zip_data.extend_from_slice(b" oh my\n");

    let locator = ZipLocator::new();
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let (_, e) = locator
        .locate_in_reader(&zip_data, &mut buf, zip_data.len() as u64)
        .unwrap_err();
    let offset = e.eocd_offset().unwrap();
    assert_eq!(offset, 1195);

    // Test that we can locate the real zip
    let archive = locator
        .locate_in_reader(&zip_data, &mut buf, offset.saturating_sub(1))
        .unwrap();
    assert_eq!(archive.comment().as_bytes(), b"This is a zipfile comment.");
}
