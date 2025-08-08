use rawzip::{ZipArchive, ZipLocator};

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

#[test]
fn test_false_eocd_recovery_slice() {
    for (asset, entries) in &[("assets/test.zip", 2u64), ("assets/zip64.zip", 1u64)] {
        let mut zip_data = std::fs::read(asset).expect("Failed to read asset");

        zip_data.extend_from_slice(b"This some trailing data: ");
        zip_data.extend_from_slice(&0x06054b50u32.to_le_bytes());
        zip_data.extend_from_slice(&[0u8; 18]);

        let locator = ZipLocator::new();
        let archive = locator.locate_in_slice(&zip_data).unwrap();
        assert_eq!(archive.entries_hint(), 0);

        // Use the archive's EOCD offset to restart search and find real archive
        let offset = archive.eocd_offset();
        let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
        let recovered_archive = locator
            .locate_in_reader(zip_data, &mut buf, offset)
            .unwrap();
        assert_eq!(recovered_archive.entries_hint(), *entries);
    }
}

#[test]
fn test_eocd_offset_points_to_signature() {
    for asset in &["assets/test.zip", "assets/zip64.zip"] {
        let data = std::fs::read(asset).expect("Failed to read asset");
        let archive = ZipArchive::from_slice(&data).unwrap();
        let eocd_offset = archive.eocd_offset();
        let signature = u32::from_le_bytes([
            data[eocd_offset as usize],
            data[eocd_offset as usize + 1],
            data[eocd_offset as usize + 2],
            data[eocd_offset as usize + 3],
        ]);
        assert_eq!(
            signature, 0x06054b50,
            "eocd_offset should point to EOCD signature (0x06054b50), got 0x{:08x}",
            signature
        );
    }
}

#[test]
fn test_eocd_offset_points_to_signature_reader() {
    for asset in &["assets/test.zip", "assets/zip64.zip"] {
        let data = std::fs::read(asset).expect("Failed to read asset");
        let archive = ZipArchive::from_file(
            std::fs::File::open(asset).unwrap(),
            &mut vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE],
        )
        .unwrap();
        let eocd_offset = archive.eocd_offset();
        let signature = u32::from_le_bytes([
            data[eocd_offset as usize],
            data[eocd_offset as usize + 1],
            data[eocd_offset as usize + 2],
            data[eocd_offset as usize + 3],
        ]);
        assert_eq!(
            signature, 0x06054b50,
            "eocd_offset should point to EOCD signature (0x06054b50), got 0x{:08x}",
            signature
        );
    }
}
