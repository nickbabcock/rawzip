use rawzip::{ErrorKind, ZipArchive, ZipArchiveWriter, ZipLocator};
use std::io::{Read, Write};

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
    let mut comment_reader = archive.comment();
    let comment_len = comment_reader.remaining() as usize;
    let mut comment_buffer = vec![0u8; comment_len];
    comment_reader.read_exact(&mut comment_buffer).unwrap();
    assert_eq!(comment_buffer.as_slice(), b"This is a zipfile comment.");
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
        let recovered_slice = locator.locate_in_slice_with_end(&zip_data, offset).unwrap();
        assert_eq!(recovered_slice.entries_hint(), *entries);

        let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
        let recovered_archive = locator
            .locate_in_reader(&zip_data, &mut buf, offset)
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
            "eocd_offset should point to EOCD signature (0x06054b50), got 0x{signature:08x}"
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
            "eocd_offset should point to EOCD signature (0x06054b50), got 0x{signature:08x}"
        );
    }
}

/// Writes an archive with one entry and the given comment
fn archive_with_comment(comment: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut data);
    archive.set_comment(comment);
    let (mut entry, config) = archive.new_file("a.txt").start().unwrap();
    let mut writer = config.wrap(&mut entry);
    writer.write_all(b"hi").unwrap();
    let (_, descriptor) = writer.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();
    data
}

/// Comments with a false signature. The locator sees too few bytes to parse, a
/// comment length past the end of the data, or a record that parses.
const FALSE_SIGNATURE_COMMENTS: &[&[u8]] = &[
    b"note PK\x05\x06 end",
    b"note PK\x05\x06zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    b"note PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
];

/// Searches before each false signature. A record with no entries also counts
/// as false, so this helper cannot recover an empty archive.
fn recover_slice<'a>(
    locator: &ZipLocator,
    data: &'a [u8],
) -> Result<rawzip::ZipSliceArchive<&'a [u8]>, rawzip::Error> {
    let mut end = data.len() as u64;
    loop {
        let false_signature = match locator.locate_in_slice_with_end(data, end) {
            Ok(archive) if archive.entries_hint() > 0 => return Ok(archive),
            Ok(archive) => archive.eocd_offset(),
            Err((_, e)) => match e.eocd_offset() {
                Some(offset) => offset,
                None => return Err(e),
            },
        };
        assert!(false_signature < end, "the search must move backwards");
        end = false_signature;
    }
}

#[test]
fn test_false_signature_in_comment_slice() {
    for comment in FALSE_SIGNATURE_COMMENTS {
        let data = archive_with_comment(comment);
        let archive = recover_slice(&ZipLocator::new(), &data).unwrap();
        assert_eq!(archive.comment().as_bytes(), *comment);

        let entry = archive.entries().next_entry().unwrap().unwrap();
        assert_eq!(entry.file_path().as_bytes(), b"a.txt");
        let entry = archive.get_entry(entry.wayfinder()).unwrap();
        assert_eq!(entry.data(), b"hi");
    }
}

#[test]
fn test_false_signature_in_comment_reader() {
    let locator = ZipLocator::new();
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    for comment in FALSE_SIGNATURE_COMMENTS {
        let data = archive_with_comment(comment);
        let mut end = data.len() as u64;
        let archive = loop {
            let false_signature = match locator.locate_in_reader(&data, &mut buf, end) {
                Ok(archive) if archive.entries_hint() > 0 => break archive,
                Ok(archive) => archive.eocd_offset(),
                Err((_, e)) => e.eocd_offset().unwrap(),
            };
            assert!(false_signature < end, "the search must move backwards");
            end = false_signature;
        };
        assert_eq!(archive.end_offset(), data.len() as u64);

        let mut actual = Vec::new();
        archive.comment().read_to_end(&mut actual).unwrap();
        assert_eq!(actual, *comment);
    }
}

#[test]
fn test_locate_in_slice_with_end_past_end_clamps() {
    let data = std::fs::read("assets/test.zip").unwrap();
    let locator = ZipLocator::new();
    let expected = locator.locate_in_slice(&data).unwrap();
    let past_u32 = (1u64 << 32) + 5;
    for end in [data.len() as u64 + 1, past_u32, u64::MAX] {
        let archive = locator.locate_in_slice_with_end(&data, end).unwrap();
        assert_eq!(archive.eocd_offset(), expected.eocd_offset());
    }
}

#[test]
fn test_locate_in_slice_with_end_keeps_truncated_comment_rejected() {
    let data = archive_with_comment(b"a comment");
    let eocd = ZipArchive::from_slice(&data).unwrap().eocd_offset();
    let truncated = &data[..data.len() - 1];

    let (_, e) = ZipLocator::new().locate_in_slice(truncated).unwrap_err();
    assert!(matches!(e.kind(), ErrorKind::Eof));
    assert_eq!(e.eocd_offset(), Some(eocd));

    let e = recover_slice(&ZipLocator::new(), truncated).err().unwrap();
    assert!(matches!(e.kind(), ErrorKind::MissingEndOfCentralDirectory));

    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let (_, e) = ZipLocator::new()
        .locate_in_reader(truncated, &mut buf, truncated.len() as u64)
        .unwrap_err();
    assert_eq!(e.eocd_offset(), Some(eocd));
}

#[test]
fn test_locate_in_slice_with_end_excludes_signature_that_crosses_end() {
    let data = archive_with_comment(b"a comment");
    let eocd = ZipArchive::from_slice(&data).unwrap().eocd_offset();

    let locator = ZipLocator::new();
    assert!(locator.locate_in_slice_with_end(&data, eocd + 4).is_ok());
    let (_, e) = locator
        .locate_in_slice_with_end(&data, eocd + 3)
        .unwrap_err();
    assert!(matches!(e.kind(), ErrorKind::MissingEndOfCentralDirectory));
}

#[test]
fn test_locate_in_slice_with_end_search_space_starts_at_end_offset() {
    let comment = [b'z'; 64];
    let data = archive_with_comment(&comment);
    let eocd = ZipArchive::from_slice(&data).unwrap().eocd_offset();
    let end = eocd + 8;

    let found = ZipLocator::new().max_search_space(8);
    assert!(found.locate_in_slice_with_end(&data, end).is_ok());

    let missed = ZipLocator::new().max_search_space(7);
    let (_, e) = missed.locate_in_slice_with_end(&data, end).unwrap_err();
    assert!(matches!(e.kind(), ErrorKind::MissingEndOfCentralDirectory));
}

#[test]
fn test_locate_in_slice_with_end_corrects_prelude_offsets() {
    let mut data = vec![b'#'; 37];
    data.extend_from_slice(&archive_with_comment(b"x PK\x05\x06 y"));

    let archive = recover_slice(&ZipLocator::new(), &data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    let entry = archive.get_entry(entry.wayfinder()).unwrap();
    assert_eq!(entry.data(), b"hi");
}

#[test]
fn test_false_signature_at_end_of_data_is_reported() {
    let data = archive_with_comment(b"PK\x05\x06");
    let false_signature = Some(data.len() as u64 - 4);
    let locator = ZipLocator::new();

    let (_, e) = locator.locate_in_slice(&data).unwrap_err();
    assert_eq!(e.eocd_offset(), false_signature);
}

#[test]
fn test_locate_in_slice_with_end_of_zero_finds_nothing() {
    let data = archive_with_comment(b"a comment");
    let (_, e) = ZipLocator::new()
        .locate_in_slice_with_end(&data, 0)
        .unwrap_err();
    assert!(matches!(e.kind(), ErrorKind::MissingEndOfCentralDirectory));
    assert_eq!(e.eocd_offset(), None);
}
