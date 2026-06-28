use rawzip::{ZipArchive, ZipArchiveWriter};
use std::io::Write;

fn central_external_attrs(data: &[u8], entry: &rawzip::ZipFileHeaderRecord<'_>) -> u32 {
    let offset = entry.central_directory_offset() as usize + 38;
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

#[test]
fn test_unix_permissions_roundtrip() {
    let test_cases = vec![
        (0o644, 0o100644, "Regular file (644)"),
        (0o755, 0o100755, "Executable file (755)"),
        (0o600, 0o100600, "Owner-only file (600)"),
        (0o777, 0o100777, "World-writable file (777)"),
        (0o040755, 0o040755, "Directory (040755)"),
        (0o100644, 0o100644, "Regular file with type (100644)"),
        (0o120777, 0o120777, "Symbolic link (120777)"),
    ];

    for (permissions, expected_mode, description) in test_cases {
        let mut output = Vec::new();

        // Write archive with permissions
        {
            let mut archive = ZipArchiveWriter::new(&mut output);

            let (mut entry, config) = archive
                .new_file("test_file.txt")
                .unix_permissions(permissions)
                .start()
                .unwrap();

            let mut writer = config.wrap(&mut entry);
            writer.write_all(b"test content").unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            entry.finish(descriptor).unwrap();

            archive.finish().unwrap();
        }

        // Read archive and verify permissions
        let archive = ZipArchive::from_slice(&output).unwrap();
        let mut entries = archive.entries();
        let entry = entries.next_entry().unwrap().unwrap();

        assert_eq!(
            entry.file_path().try_normalize().unwrap().as_ref(),
            "test_file.txt"
        );

        let actual_mode = entry.mode().value();
        assert_eq!(
            actual_mode, expected_mode,
            "{description}: expected permissions 0o{expected_mode:o}, got 0o{actual_mode:o}"
        );
    }
}

#[test]
fn test_directory_permissions_roundtrip() {
    let mut output = Vec::new();

    // Write archive with directory
    {
        let mut archive = ZipArchiveWriter::new(&mut output);

        archive
            .new_dir("test_dir/")
            .unix_permissions(0o040755)
            .create()
            .unwrap();
        archive.finish().unwrap();
    }

    // Read archive and verify directory permissions
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(
        entry.file_path().try_normalize().unwrap().as_ref(),
        "test_dir/"
    );
    assert!(entry.is_dir());

    let actual_mode = entry.mode().value();
    assert_eq!(
        actual_mode, 0o040755,
        "Directory permissions: expected 0o040755, got 0o{actual_mode:o}"
    );

    assert_eq!(central_external_attrs(&output, &entry) & 0x10, 0x10);
}

#[test]
fn test_directory_without_unix_permissions_has_msdos_directory_attribute() {
    let mut output = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut output);
    archive.new_dir("test_dir/").create().unwrap();
    archive.finish().unwrap();

    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    assert!(entry.is_dir());
    assert_eq!(central_external_attrs(&output, &entry) & 0x10, 0x10);
}

#[test]
fn test_permissions_without_unix_permissions() {
    let mut output = Vec::new();

    // Write archive without explicit permissions
    {
        let mut archive = ZipArchiveWriter::new(&mut output);

        let (mut entry, config) = archive.new_file("test_file.txt").start().unwrap(); // No unix_permissions set

        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"test content").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();

        archive.finish().unwrap();
    }

    // Read archive and verify default behavior
    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();

    // When no unix permissions are set, we should get default permissions
    let actual_mode = entry.mode().value();
    assert_eq!(
        actual_mode, 0o100666,
        "Default permissions: expected 0o100666, got 0o{actual_mode:o}"
    );
}
