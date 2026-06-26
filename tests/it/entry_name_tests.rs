use rawzip::{EntryName, ZipArchive, ZipArchiveWriter};
use std::io::Write;

/// Writes one file and returns the archive.
fn write_one<'n>(name: impl Into<EntryName<'n>>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut output);
    let (mut entry, config) = archive.new_file(name).start().unwrap();
    let mut writer = config.wrap(&mut entry);
    writer.write_all(b"content").unwrap();
    let (_, descriptor) = writer.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();
    output
}

#[test]
fn borrowed_str_reference_is_an_entry_name() {
    let name = "file.txt";
    let output = write_one(&name);
    let archive = ZipArchive::from_slice(&output).unwrap();
    let entry = archive.entries().next().unwrap().unwrap();

    assert_eq!(entry.file_path().as_ref(), b"file.txt");
}

/// Reads the first entry's raw name.
fn first_entry_name(zip_data: &[u8]) -> Vec<u8> {
    let archive = ZipArchive::from_slice(zip_data).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    entry.file_path().as_bytes().to_vec()
}

fn first_entry_is_utf8(zip_data: &[u8]) -> bool {
    let archive = ZipArchive::from_slice(zip_data).unwrap();
    let mut entries = archive.entries();
    entries.next_entry().unwrap().unwrap().flags().is_utf8()
}

/// Verbatim Shift-JIS remains byte-exact with the UTF-8 flag cleared.
#[test]
fn verbatim_shift_jis_roundtrip() {
    // Shift-JIS "ソ.txt"; build at runtime to avoid a literal lint.
    let mut raw = vec![0x83u8, 0x5c];
    raw.extend_from_slice(b".txt");
    assert!(std::str::from_utf8(&raw).is_err());

    let output = write_one(EntryName::verbatim(raw.as_slice()));

    assert_eq!(
        first_entry_name(&output),
        raw,
        "bytes must be preserved exactly"
    );
    assert!(!first_entry_is_utf8(&output));
}

/// Verbatim data does not infer UTF-8 from valid bytes.
#[test]
fn verbatim_valid_utf8_no_flag() {
    let raw = "日本.txt".as_bytes();
    let output = write_one(EntryName::verbatim(raw));

    assert_eq!(first_entry_name(&output), raw);
    assert!(!first_entry_is_utf8(&output));
}

/// Conformant non-ASCII names set the UTF-8 flag.
#[test]
fn conformant_non_ascii_sets_flag() {
    let output = write_one(EntryName::conformant("日本.txt"));
    assert_eq!(first_entry_name(&output), "日本.txt".as_bytes());
    assert!(first_entry_is_utf8(&output));
}

/// String names retain traversal normalization.
#[test]
fn conformant_traversal_collapses_to_root() {
    let output = write_one("../../etc/passwd");
    assert_eq!(first_entry_name(&output), b"etc/passwd");
}

/// Reader-normalized names can be written directly.
#[test]
fn normalized_skip_arm_roundtrip() {
    // Build an entry requiring normalization.
    let source = write_one(EntryName::verbatim(b"dir/../safe.txt".as_slice()));
    let src_archive = ZipArchive::from_slice(&source).unwrap();
    let mut entries = src_archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    let safe = entry.file_path().try_normalize().unwrap();
    assert_eq!(safe.as_str(), "safe.txt");

    // Write the normalized path directly.
    let output = write_one(safe);
    assert_eq!(first_entry_name(&output), b"safe.txt");
}

/// Files and directories accept owned names.
#[test]
fn owned_names_accepted() {
    let mut output = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut output);
    archive.new_dir(String::from("d/")).create().unwrap();
    let (mut entry, config) = archive.new_file(String::from("f.txt")).start().unwrap();
    let mut writer = config.wrap(&mut entry);
    writer.write_all(b"x").unwrap();
    let (_, descriptor) = writer.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();

    let archive = ZipArchive::from_slice(&output).unwrap();
    let mut entries = archive.entries();
    let names: Vec<_> = std::iter::from_fn(|| {
        entries
            .next_entry()
            .unwrap()
            .map(|e| e.file_path().as_bytes().to_vec())
    })
    .collect();
    assert_eq!(names, vec![b"d/".to_vec(), b"f.txt".to_vec()]);
}
