use rstest::rstest;
use std::io::Write;

/// Test filename UTF-8 flag behavior with various filenames
#[rstest]
#[case("file.txt", false)]
#[case("MixedCase123.TXT", false)]
#[case("with-dashes_and_underscores.txt", false)]
#[case("🦀🔥_rust_file.txt", true)]
#[case("テストファイル.txt", true)]
#[case("café.txt", true)]
#[case("file~backup.txt", true)] // Tilde character - UTF-8 flag (EUC-KR conflict)
#[case("path\\file.txt", false)]
#[case("normal-file_123.txt", false)]
#[case("test|file.txt", false)]
#[case("test}file.txt", false)]
fn test_filename_utf8_flag(#[case] filename: &str, #[case] should_have_utf8_flag: bool) {
    let mut output = Vec::new();
    {
        let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
        let (mut entry, config) = archive.new_file(filename).start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"test content").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }

    // Parse the ZIP file to verify the UTF-8 flag is set correctly
    let flags = extract_flags_from_zip(&output);
    let utf8_flag_present = (flags & 0x800) != 0;

    assert_eq!(
        utf8_flag_present, should_have_utf8_flag,
        "UTF-8 flag mismatch for filename '{}': expected {}, got {}",
        filename, should_have_utf8_flag, utf8_flag_present
    );
}

/// Test directory UTF-8 flag behavior with various directory names
#[rstest]
#[case("ascii_dir/", false)]
#[case("🦀🔥/", true)]
#[case("フォルダ/", true)]
#[case("dossier/", false)]
#[case("café_folder/", true)]
#[case("file~backup/", true)]
fn test_directory_utf8_flag(#[case] dirname: &str, #[case] should_have_utf8_flag: bool) {
    let mut output = Vec::new();
    {
        let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
        archive.new_dir(dirname).create().unwrap();
        archive.finish().unwrap();
    }

    // Parse the ZIP file to verify the UTF-8 flag is set correctly
    let flags = extract_flags_from_zip(&output);
    let utf8_flag_present = (flags & 0x800) != 0;

    assert_eq!(
        utf8_flag_present, should_have_utf8_flag,
        "UTF-8 flag mismatch for directory '{}': expected {}, got {}",
        dirname, should_have_utf8_flag, utf8_flag_present
    );
}

/// Test the UTF-8
/// Helper function to extract the general purpose bit flags from the first local file header
/// This is a simplified parser just for testing purposes
fn extract_flags_from_zip(zip_data: &[u8]) -> u16 {
    // ZIP local file header structure:
    // 0-3: signature (0x04034b50)
    // 4-5: version needed
    // 6-7: general purpose bit flag <- this is what we want
    // 8-9: compression method
    // ...

    // Check for local file header signature
    let signature = u32::from_le_bytes([zip_data[0], zip_data[1], zip_data[2], zip_data[3]]);
    if signature != 0x04034b50 {
        panic!("Invalid local file header signature: 0x{:x}", signature);
    }

    // Extract general purpose bit flag (bytes 6-7)
    u16::from_le_bytes([zip_data[6], zip_data[7]])
}
