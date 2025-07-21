use rawzip::{
    extra_fields::{ExtraFieldId, ExtraFields},
    time::{LocalDateTime, UtcDateTime, ZipDateTimeKind},
    ZipArchive, ZipArchiveWriter,
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
        let (mut entry, config) = archive
            .new_file("test.txt")
            .last_modified(datetime)
            .start()
            .unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
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
        archive
            .new_dir("test_dir/")
            .last_modified(datetime)
            .create()
            .unwrap();
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
        let (mut entry, config) = archive.new_file("test.txt").start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
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
        let (mut entry, config) = archive
            .new_file("test.txt")
            .last_modified(datetime)
            .start()
            .unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
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
        let (mut entry, config) = archive.new_file("test.txt").start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
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
        let (mut entry, config) = archive
            .new_file("test.txt")
            .last_modified(datetime)
            .start()
            .unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
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
        let (mut entry1, config1) = archive
            .new_file("file1.txt")
            .last_modified(datetime1)
            .start()
            .unwrap();
        let mut writer1 = config1.wrap(&mut entry1);
        writer1.write_all(b"File 1").unwrap();
        let (_, descriptor1) = writer1.finish().unwrap();
        entry1.finish(descriptor1).unwrap();

        // Second file
        let (mut entry2, config2) = archive
            .new_file("file2.txt")
            .last_modified(datetime2)
            .start()
            .unwrap();
        let mut writer2 = config2.wrap(&mut entry2);
        writer2.write_all(b"File 2").unwrap();
        let (_, descriptor2) = writer2.finish().unwrap();
        entry2.finish(descriptor2).unwrap();

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

        // This should compile and work (breaking change)
        archive
            .new_dir("christmas/")
            .last_modified(datetime)
            .create()
            .unwrap();

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

    // Test that only UTC timestamps can be used for last_modified
    let mut output = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut output);
    let _builder = archive.new_file("test.txt").last_modified(utc_time);

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

/// Test demonstrating when the local header contains richer timestamp data
#[test]
fn test_infozip_extended_timestamps() {
    let file =
        std::fs::File::open("assets/time-infozip.zip").expect("Failed to open time-infozip.zip");
    let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buffer).expect("Failed to create ZipArchive");

    let mut entries = archive.entries(&mut buffer);
    let entry = entries.next_entry().unwrap().unwrap();

    // Get local header extra fields
    let wayfinder = entry.wayfinder();
    let zip_entry = archive.get_entry(wayfinder).unwrap();
    let mut local_buffer = vec![0u8; 256];
    let local_header = zip_entry.local_header(&mut local_buffer).unwrap();
    assert_extend_time_extra_field_difference(entry.extra_fields(), local_header.extra_fields());
}

/// Test demonstrating when the local header contains richer timestamp data using ZipSliceArchive
#[test]
fn test_infozip_extended_timestamps_slice() {
    let archive_data =
        std::fs::read("assets/time-infozip.zip").expect("Failed to read time-infozip.zip");
    let archive =
        rawzip::ZipArchive::from_slice(&archive_data).expect("Failed to create ZipSliceArchive");

    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    // Get local header extra fields
    let wayfinder = entry.wayfinder();
    let zip_entry = archive.get_entry(wayfinder).unwrap();
    let local_fields = zip_entry.extra_fields();
    assert_extend_time_extra_field_difference(entry.extra_fields(), local_fields);
}

fn assert_extend_time_extra_field_difference(mut central: ExtraFields, mut local: ExtraFields) {
    let central_et = central
        .find(|(id, _)| *id == ExtraFieldId::EXTENDED_TIMESTAMP)
        .expect("Central directory should have Extended Timestamp field");
    let local_et = local
        .find(|(id, _)| *id == ExtraFieldId::EXTENDED_TIMESTAMP)
        .expect("Local header should have Extended Timestamp field");

    let (_, central_data) = central_et;
    let (_, local_data) = local_et;

    assert_eq!(
        central_data.len(),
        5,
        "Central directory should have 5 bytes (mod time only)"
    );
    assert_eq!(local_data.len(), 9, "Local header should have 9 bytes (mod + access times) and have richer timestamp data than central directory");
}
