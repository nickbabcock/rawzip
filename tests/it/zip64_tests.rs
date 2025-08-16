use rawzip::{ZipArchive, ZipArchiveWriter, ZipDataWriter, RECOMMENDED_BUFFER_SIZE};
use rstest::rstest;
use std::io::{Cursor, Write};

// ZIP64 signatures to check for
const ZIP64_EOCD_SIGNATURE: u32 = 0x06064b50;
const ZIP64_EOCD_LOCATOR_SIGNATURE: u32 = 0x07064b50;

/// Helper function to check if ZIP64 structures are present in the archive
fn contains_zip64_signatures(data: &[u8]) -> bool {
    let zip64_eocd_sig_bytes = ZIP64_EOCD_SIGNATURE.to_le_bytes();
    let zip64_locator_sig_bytes = ZIP64_EOCD_LOCATOR_SIGNATURE.to_le_bytes();

    let has_eocd = data.windows(4).any(|w| w == zip64_eocd_sig_bytes);
    let has_locator = data.windows(4).any(|w| w == zip64_locator_sig_bytes);

    has_eocd && has_locator
}

fn verify_expected_entries(data: &[u8], expected_count: u64) {
    // Verify with slice
    let read_archive = ZipArchive::from_slice(data).unwrap();
    assert_eq!(read_archive.entries_hint(), expected_count);
    let entries = read_archive.entries();
    let mut count = 0;
    for _ in entries {
        count += 1;
    }
    assert_eq!(count, expected_count as usize);

    // Verify with reader
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let read_archive = ZipArchive::from_seekable(Cursor::new(data), &mut buffer).unwrap();
    assert_eq!(read_archive.entries_hint(), expected_count);
    let mut entries = read_archive.entries(&mut buffer);
    let mut count = 0;
    while entries.next_entry().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, expected_count as usize);
}

/// Test ZIP64 threshold behavior with different entry counts
#[rstest]
#[case(65534, false)]
#[case(65535, true)]
#[case(65536, true)]
fn test_zip64_threshold_entries(#[case] entry_count: usize, #[case] should_be_zip64: bool) {
    let output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::builder()
        .with_capacity(entry_count)
        .build(output);

    for i in 0..entry_count {
        let filename = format!("file_{:05}.txt", i);
        let mut file = archive.new_file(&filename).create().unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"x").unwrap();
        let (_, descriptor_output) = writer.finish().unwrap();

        file.finish(descriptor_output).unwrap();
    }

    let writer = archive.finish().unwrap();
    let data = writer.into_inner();

    let archive_type = if should_be_zip64 {
        "ZIP64"
    } else {
        "standard ZIP"
    };
    println!(
        "Created {} archive with {} entries",
        archive_type, entry_count
    );

    // Verify ZIP64 signatures presence matches expectation
    let has_zip64 = contains_zip64_signatures(&data);
    assert_eq!(
        has_zip64, should_be_zip64,
        "{} entries expected zip64: {}",
        entry_count, should_be_zip64
    );

    verify_expected_entries(&data, entry_count as u64);
}
