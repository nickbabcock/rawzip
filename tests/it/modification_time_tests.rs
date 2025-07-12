use rawzip::{
    time::{LocalDateTime, UtcDateTime, ZipDateTimeKind},
    ZipArchive, ZipArchiveWriter, ZipDataWriter, ZipEntryOptions,
};
use std::io::Write;

/// Test that modification times are preserved in a round-trip for files
#[test]
fn test_modification_time_roundtrip_file() {
    let datetime = UtcDateTime::from_components(2023, 6, 15, 14, 30, 45, 0).unwrap();
    let mut output = Vec::new();

    // Create archive with modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime);
        let mut file = archive.new_file("test.txt", options).unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    // Read back and verify modification time
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "test.txt"
    );
    let actual_datetime = entry.last_modified();
    assert_eq!(actual_datetime, ZipDateTimeKind::Utc(datetime));
}

/// Test that modification times are preserved in a round-trip for directories
#[test]
fn test_modification_time_roundtrip_directory() {
    let datetime = UtcDateTime::from_components(2023, 8, 20, 9, 15, 30, 0).unwrap();
    let mut output = Vec::new();

    // Create archive with directory modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime);
        archive.new_dir("test_dir/", options).unwrap();
        archive.finish().unwrap();
    }

    // Read back and verify modification time
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "test_dir/"
    );
    let actual_datetime = entry.last_modified();

    assert_eq!(actual_datetime, ZipDateTimeKind::Utc(datetime));
}

/// Test that files without modification time use DOS timestamp 0
#[test]
fn test_no_modification_time_defaults_to_zero() {
    let mut output = Vec::new();

    // Create archive without modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default();
        let mut file = archive.new_file("test.txt", options).unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    // Read back and verify it uses the "zero" timestamp (1980-01-01 00:00:00)
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "test.txt"
    );
    let actual_datetime = entry.last_modified();

    // Should be the DOS timestamp 0 normalized to 1980-01-01 00:00:00
    let expected =
        ZipDateTimeKind::Local(LocalDateTime::from_components(1980, 1, 1, 0, 0, 0, 0).unwrap());
    assert_eq!(actual_datetime, expected);
}

/// Test that extended timestamp format is used when modification time is provided
#[test]
fn test_extended_timestamp_format_present() {
    let datetime = UtcDateTime::from_components(2023, 6, 15, 14, 30, 45, 0).unwrap();
    let mut output = Vec::new();

    // Create archive with modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime);
        let mut file = archive.new_file("test.txt", options).unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    // Check that the extended timestamp extra field is present
    // Extended timestamp field ID is 0x5455
    let extended_timestamp_id_bytes = 0x5455u16.to_le_bytes();
    let contains_extended_timestamp = output.windows(2).any(|w| w == extended_timestamp_id_bytes);

    assert!(
        contains_extended_timestamp,
        "Extended timestamp extra field should be present when modification time is provided"
    );
}

/// Test that no extended timestamp format is used when no modification time is provided
#[test]
fn test_no_extended_timestamp_without_modification_time() {
    let mut output = Vec::new();

    // Create archive without modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default();
        let mut file = archive.new_file("test.txt", options).unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    // Check that the extended timestamp extra field is NOT present
    let extended_timestamp_id_bytes = 0x5455u16.to_le_bytes();
    let contains_extended_timestamp = output.windows(2).any(|w| w == extended_timestamp_id_bytes);

    assert!(
        !contains_extended_timestamp,
        "Extended timestamp extra field should NOT be present when no modification time is provided"
    );
}

/// Test that we can handle timestamps outside DOS range (before 1980)
#[test]
fn test_timestamp_before_dos_range() {
    let datetime = UtcDateTime::from_components(1970, 1, 1, 0, 0, 0, 0).unwrap();
    let mut output = Vec::new();

    // Create archive with pre-1980 timestamp
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime);
        let mut file = archive.new_file("test.txt", options).unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "test.txt"
    );
    let actual_datetime = entry.last_modified();

    assert_eq!(actual_datetime, ZipDateTimeKind::Utc(datetime));
}

/// Test multiple files with different modification times
#[test]
fn test_multiple_files_different_timestamps() {
    let datetime1 = UtcDateTime::from_components(2023, 1, 15, 10, 0, 0, 0).unwrap();
    let datetime2 = UtcDateTime::from_components(2023, 6, 20, 15, 30, 45, 0).unwrap();
    let mut output = Vec::new();

    // Create archive with multiple files having different timestamps
    {
        let mut archive = ZipArchiveWriter::new(&mut output);

        // First file
        let options1 = ZipEntryOptions::default().modification_time(datetime1);
        let mut file1 = archive.new_file("file1.txt", options1).unwrap();
        let mut writer1 = ZipDataWriter::new(&mut file1);
        writer1.write_all(b"File 1").unwrap();
        let (_, descriptor1) = writer1.finish().unwrap();
        file1.finish(descriptor1).unwrap();

        // Second file
        let options2 = ZipEntryOptions::default().modification_time(datetime2);
        let mut file2 = archive.new_file("file2.txt", options2).unwrap();
        let mut writer2 = ZipDataWriter::new(&mut file2);
        writer2.write_all(b"File 2").unwrap();
        let (_, descriptor2) = writer2.finish().unwrap();
        file2.finish(descriptor2).unwrap();

        archive.finish().unwrap();
    }

    // Read back and verify timestamps
    let archive = ZipArchive::from_slice(&output).unwrap();
    let entries: Vec<_> = archive.entries().collect();

    assert_eq!(entries.len(), 2);

    // Find entries by name and check timestamps
    for entry in entries {
        let entry = entry.unwrap();
        let file_path = entry.file_path();
        let filename = file_path.try_normalize().unwrap();
        match filename.as_ref() {
            "file1.txt" => {
                assert_eq!(entry.last_modified(), ZipDateTimeKind::Utc(datetime1));
            }
            "file2.txt" => {
                // Since we now require UTC timestamps, the result should be identical
                assert_eq!(entry.last_modified(), ZipDateTimeKind::Utc(datetime2));
            }
            name => panic!("Unexpected file: {}", name),
        }
    }
}

#[test]
fn test_new_dir_with_options() {
    let datetime = UtcDateTime::from_components(2023, 12, 25, 12, 0, 0, 0).unwrap();
    let mut output = Vec::new();

    // Create archive with directory using options
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime);

        // This should compile and work (breaking change)
        archive.new_dir("christmas/", options).unwrap();

        archive.finish().unwrap();
    }

    // Verify the directory was created with the correct timestamp
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "christmas/"
    );
    assert!(entry.is_dir());
    assert_eq!(entry.last_modified(), ZipDateTimeKind::Utc(datetime));
}

/// Test compile-time timezone API and date validation
#[test]
fn test_timezone_api_and_validation() {
    // Create UTC timestamp with validation
    let utc_time = UtcDateTime::from_components(2023, 6, 15, 14, 30, 45, 0).unwrap();
    let local_time = LocalDateTime::from_components(2023, 6, 15, 14, 30, 45, 0).unwrap();

    // Verify timestamp properties
    assert_eq!(utc_time.year(), 2023);
    assert_eq!(utc_time.month(), 6);
    assert_eq!(utc_time.day(), 15);
    assert_eq!(utc_time.hour(), 14);
    assert_eq!(utc_time.minute(), 30);
    assert_eq!(utc_time.second(), 45);
    assert_eq!(utc_time.nanosecond(), 0);

    // Verify timezone types work
    assert_eq!(utc_time.timezone(), rawzip::time::TimeZone::Utc);
    assert_eq!(local_time.timezone(), rawzip::time::TimeZone::Local);

    // Test that only UTC timestamps can be used for modification_time
    let _options = ZipEntryOptions::default().modification_time(utc_time);

    // Test date validation
    assert!(UtcDateTime::from_components(2023, 2, 30, 0, 0, 0, 0).is_none()); // Feb 30th
    assert!(LocalDateTime::from_components(2023, 13, 1, 0, 0, 0, 0).is_none()); // 13th month
    assert!(UtcDateTime::from_components(2023, 4, 31, 0, 0, 0, 0).is_none()); // April 31st

    // Test leap year validation
    assert!(UtcDateTime::from_components(2020, 2, 29, 0, 0, 0, 0).is_some()); // 2020 is leap year
    assert!(UtcDateTime::from_components(2021, 2, 29, 0, 0, 0, 0).is_none()); // 2021 is not leap year
}

/// Test ZipDateTimeKind functionality and timezone handling
#[test]
fn test_parsed_datetime_functionality() {
    // UTC timestamps can be used for Extended Timestamp writing
    let utc_dt = UtcDateTime::from_components(2023, 6, 15, 14, 30, 45, 0).unwrap();

    // Local timestamps are for reading legacy ZIP files
    let local_dt = LocalDateTime::from_components(1995, 1, 1, 12, 0, 0, 0).unwrap();

    // ZipDateTimeKind can represent either
    let parsed_utc = ZipDateTimeKind::Utc(utc_dt);
    let parsed_local = ZipDateTimeKind::Local(local_dt);

    // Both can be queried uniformly
    assert_eq!(parsed_utc.year(), 2023);
    assert_eq!(parsed_local.year(), 1995);
    assert_eq!(parsed_utc.timezone(), rawzip::time::TimeZone::Utc);
    assert_eq!(parsed_local.timezone(), rawzip::time::TimeZone::Local);
}
