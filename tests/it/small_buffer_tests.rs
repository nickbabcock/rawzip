use rawzip::{ErrorKind, RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipArchiveWriter, ZipLocator};
use std::io::Cursor;

fn empty_archive() -> Vec<u8> {
    let mut output = Vec::new();
    ZipArchiveWriter::new(&mut output).finish().unwrap();
    output
}

fn one_entry_archive() -> Vec<u8> {
    let mut output = Vec::new();
    let mut archive = ZipArchiveWriter::new(&mut output);
    archive.new_dir("dir/").create().unwrap();
    archive.finish().unwrap();
    output
}

#[rstest::rstest]
#[case(0)]
#[case(3)]
#[case(22)]
#[case(45)]
#[case(55)]
fn test_locator_small_buffer(#[case] size: usize) {
    let data = empty_archive();
    let mut buffer = vec![0u8; size];

    let (_, err) = ZipLocator::new()
        .locate_in_reader(data.as_slice(), &mut buffer, data.len() as u64)
        .unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::BufferTooSmall { required: 56 }),
        "locate_in_reader: {err:?}"
    );

    let err = ZipArchive::from_seekable(Cursor::new(&data), &mut buffer).unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::BufferTooSmall { required: 56 }),
        "from_seekable: {err:?}"
    );
}

#[test]
fn test_locator_minimum_buffer() {
    let data = one_entry_archive();
    let mut buffer = vec![0u8; 56];
    let archive = ZipLocator::new()
        .locate_in_reader(data.as_slice(), &mut buffer, data.len() as u64)
        .unwrap();
    assert_eq!(archive.entries_hint(), 1);
}

#[test]
fn test_entries_small_buffer() {
    let data = one_entry_archive();
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(data.as_slice(), &mut buffer, data.len() as u64)
        .unwrap();

    let mut small = vec![0u8; 45];
    let mut entries = archive.entries(&mut small);
    let err = entries.next_entry().unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::BufferTooSmall { required: 46 }),
        "{err:?}"
    );

    let mut minimum = vec![0u8; 46];
    let mut entries = archive.entries(&mut minimum);
    let entry = entries.next_entry().unwrap().unwrap();
    assert_eq!(entry.file_path().as_ref(), b"dir/");
    assert!(entries.next_entry().unwrap().is_none());
}

#[test]
fn test_entries_small_buffer_empty_archive() {
    let data = empty_archive();
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(data.as_slice(), &mut buffer, data.len() as u64)
        .unwrap();

    let mut entries = archive.entries(&mut []);
    assert!(entries.next_entry().unwrap().is_none());
}

#[rstest::rstest]
#[case("assets/zip64.zip")]
#[case("assets/zip64-2.zip")]
fn test_locator_minimum_buffer_zip64(#[case] path: &str) {
    // The minimum buffer must hold the Zip64 end of central directory record,
    // so that the locator does not fall back to the classic record.
    let data = std::fs::read(path).unwrap();
    let mut large = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let expected = ZipArchive::from_seekable(Cursor::new(&data), &mut large).unwrap();

    let mut buffer = vec![0u8; 56];
    let archive = ZipArchive::from_seekable(Cursor::new(&data), &mut buffer).unwrap();
    assert_eq!(archive.entries_hint(), expected.entries_hint());
    assert_eq!(archive.directory_offset(), expected.directory_offset());
}
