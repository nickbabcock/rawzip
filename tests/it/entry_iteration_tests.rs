use rawzip::{RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipLocator};

/// Corrupt the signature of the central directory record at `index` in
/// `assets/test.zip`, which has two entries.
fn corrupt_central_directory_record(index: usize) -> Vec<u8> {
    let mut data = std::fs::read("assets/test.zip").unwrap();
    let offset = ZipArchive::from_slice(data.as_slice())
        .unwrap()
        .entries()
        .nth(index)
        .unwrap()
        .unwrap()
        .central_directory_offset() as usize;
    data[offset] ^= 0xff;
    data
}

fn outcomes(mut next: impl FnMut() -> Option<Result<(), rawzip::Error>>) -> Vec<&'static str> {
    (0..5)
        .map(|_| match next() {
            Some(Ok(())) => "ok",
            Some(Err(_)) => "err",
            None => "none",
        })
        .collect()
}

#[test]
fn slice_entries_end_after_error() {
    for (index, expected) in [
        (0, vec!["err", "none", "none", "none", "none"]),
        (1, vec!["ok", "err", "none", "none", "none"]),
    ] {
        let data = corrupt_central_directory_record(index);
        let archive = ZipArchive::from_slice(data.as_slice()).unwrap();
        let mut entries = archive.entries();
        let actual = outcomes(|| entries.next().map(|x| x.map(|_| ())));
        assert_eq!(actual, expected, "corrupt record {index}");
    }
}

#[test]
fn slice_entries_skip_errors_and_end() {
    let data = corrupt_central_directory_record(1);
    let archive = ZipArchive::from_slice(data.as_slice()).unwrap();
    assert_eq!(archive.entries().filter_map(Result::ok).count(), 1);
}

#[test]
fn reader_entries_end_after_error() {
    for (index, expected) in [
        (0, vec!["err", "none", "none", "none", "none"]),
        (1, vec!["ok", "err", "none", "none", "none"]),
    ] {
        let data = corrupt_central_directory_record(index);
        let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let archive = ZipLocator::new()
            .locate_in_reader(data.as_slice(), &mut buffer, data.len() as u64)
            .map_err(|(_, e)| e)
            .unwrap();
        let mut entries = archive.entries(&mut buffer);
        let actual = outcomes(|| entries.next_entry().map(|x| x.map(|_| ())).transpose());
        assert_eq!(actual, expected, "corrupt record {index}");
    }
}
