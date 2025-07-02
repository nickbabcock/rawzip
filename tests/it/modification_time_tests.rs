use rawzip::{
    TimeZone, ZipArchive, ZipArchiveWriter, ZipDataWriter, ZipDateTime, ZipEntryOptions,
};
use std::io::Write;

/// Test that modification times are preserved in a round-trip for files
#[test]
fn test_modification_time_roundtrip_file() {
    let datetime = ZipDateTime::from_components(2023, 6, 15, 14, 30, 45, 0, TimeZone::Utc);
    let mut output = Vec::new();

    // Create archive with modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime.clone());
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
    
    assert_eq!(entry.file_safe_path().unwrap(), "test.txt");
    let actual_datetime = entry.last_modified();
    assert_eq!(actual_datetime, datetime);
}

/// Test that modification times are preserved in a round-trip for directories
#[test]
fn test_modification_time_roundtrip_directory() {
    let datetime = ZipDateTime::from_components(2023, 8, 20, 9, 15, 30, 0, TimeZone::Local);
    let mut output = Vec::new();

    // Create archive with directory modification time
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime.clone());
        archive.new_dir("test_dir/", options).unwrap();
        archive.finish().unwrap();
    }

    // Read back and verify modification time
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    
    assert_eq!(entry.file_safe_path().unwrap(), "test_dir/");
    let actual_datetime = entry.last_modified();
    
    // When using extended timestamp format, timestamps are converted to UTC
    // So we expect the same date/time but with UTC timezone
    let expected_datetime = ZipDateTime::from_components(2023, 8, 20, 9, 15, 30, 0, TimeZone::Utc);
    assert_eq!(actual_datetime, expected_datetime);
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
    
    assert_eq!(entry.file_safe_path().unwrap(), "test.txt");
    let actual_datetime = entry.last_modified();
    
    // Should be the DOS timestamp 0 normalized to 1980-01-01 00:00:00
    let expected = ZipDateTime::from_components(1980, 1, 1, 0, 0, 0, 0, TimeZone::Local);
    assert_eq!(actual_datetime, expected);
}

/// Test that extended timestamp format is used when modification time is provided
#[test]
fn test_extended_timestamp_format_present() {
    let datetime = ZipDateTime::from_components(2023, 6, 15, 14, 30, 45, 0, TimeZone::Utc);
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
    let contains_extended_timestamp = output
        .windows(2)
        .any(|w| w == extended_timestamp_id_bytes);
    
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
    let contains_extended_timestamp = output
        .windows(2)
        .any(|w| w == extended_timestamp_id_bytes);
    
    assert!(
        !contains_extended_timestamp,
        "Extended timestamp extra field should NOT be present when no modification time is provided"
    );
}

/// Test that we can handle timestamps outside DOS range (before 1980)
#[test]
fn test_timestamp_before_dos_range() {
    let datetime = ZipDateTime::from_components(1970, 1, 1, 0, 0, 0, 0, TimeZone::Utc);
    let mut output = Vec::new();

    // Create archive with pre-1980 timestamp
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime.clone());
        let mut file = archive.new_file("test.txt", options).unwrap();
        let mut writer = ZipDataWriter::new(&mut file);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    // Read back - should still have the extended timestamp since DOS conversion will fail
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    
    assert_eq!(entry.file_safe_path().unwrap(), "test.txt");
    let actual_datetime = entry.last_modified();
    
    // Should preserve the original timestamp via extended timestamp format
    assert_eq!(actual_datetime, datetime);
}

/// Test multiple files with different modification times
#[test]
fn test_multiple_files_different_timestamps() {
    let datetime1 = ZipDateTime::from_components(2023, 1, 15, 10, 0, 0, 0, TimeZone::Utc);
    let datetime2 = ZipDateTime::from_components(2023, 6, 20, 15, 30, 45, 0, TimeZone::Local);
    let mut output = Vec::new();

    // Create archive with multiple files having different timestamps
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        
        // First file
        let options1 = ZipEntryOptions::default().modification_time(datetime1.clone());
        let mut file1 = archive.new_file("file1.txt", options1).unwrap();
        let mut writer1 = ZipDataWriter::new(&mut file1);
        writer1.write_all(b"File 1").unwrap();
        let (_, descriptor1) = writer1.finish().unwrap();
        file1.finish(descriptor1).unwrap();
        
        // Second file - expect this to be converted to UTC when using extended timestamp
        let options2 = ZipEntryOptions::default().modification_time(datetime2.clone());
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
        let filename = entry.file_safe_path().unwrap();
        match filename.as_ref() {
            "file1.txt" => {
                assert_eq!(entry.last_modified(), datetime1);
            }
            "file2.txt" => {
                // Local timestamp gets converted to UTC when using extended timestamp format
                let expected_datetime2 = ZipDateTime::from_components(2023, 6, 20, 15, 30, 45, 0, TimeZone::Utc);
                assert_eq!(entry.last_modified(), expected_datetime2);
            }
            name => panic!("Unexpected file: {}", name),
        }
    }
}

/// Test that the breaking change to new_dir works correctly
#[test]
fn test_new_dir_with_options() {
    let datetime = ZipDateTime::from_components(2023, 12, 25, 12, 0, 0, 0, TimeZone::Utc);
    let mut output = Vec::new();

    // Create archive with directory using options
    {
        let mut archive = ZipArchiveWriter::new(&mut output);
        let options = ZipEntryOptions::default().modification_time(datetime.clone());
        
        // This should compile and work (breaking change)
        archive.new_dir("christmas/", options).unwrap();
        
        archive.finish().unwrap();
    }

    // Verify the directory was created with the correct timestamp
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    
    assert_eq!(entry.file_safe_path().unwrap(), "christmas/");
    assert!(entry.is_dir());
    assert_eq!(entry.last_modified(), datetime);
}