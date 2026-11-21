use crate::extra_data_zip_tests::{digital_signature_record, zip_with_cd_trailer};
use crate::truncation_tests::overflow_offset_zip;
use rawzip::{
    Error, ErrorKind, MAX_CENTRAL_DIRECTORY_RECORD_SIZE, RECOMMENDED_BUFFER_SIZE, ReaderAt,
    ZipArchive, ZipArchiveEntryWayfinder, ZipArchiveWriter, ZipEntries, ZipLocator,
};
use std::cell::{Cell, RefCell};

/// Size of the fixed part of a central directory record.
const HEADER_SIZE: usize = 46;

/// The offset of the central directory record at `index`.
fn record_offset(data: &[u8], index: usize) -> usize {
    ZipArchive::from_slice(data)
        .unwrap()
        .entries()
        .nth(index)
        .unwrap()
        .unwrap()
        .central_directory_offset() as usize
}

/// Corrupt the signature of the central directory record at `index` in
/// `assets/test.zip`, which has two entries.
fn corrupt_central_directory_record(index: usize) -> Vec<u8> {
    let mut data = std::fs::read("assets/test.zip").unwrap();
    let offset = record_offset(&data, index);
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
    let original = std::fs::read("assets/test.zip").unwrap();
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
        // The failed record is where to resume.
        assert_eq!(
            entries.next_offset(),
            record_offset(&original, index) as u64
        );
    }
}

/// A reader that records each read and can fail the next read at one offset.
struct TestReader {
    data: Vec<u8>,
    fail_at: Cell<Option<u64>>,
    reads: RefCell<Vec<(u64, usize)>>,
}

impl ReaderAt for TestReader {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        if self.fail_at.get() == Some(offset) {
            self.fail_at.set(None);
            return Err(std::io::Error::other("injected read failure"));
        }
        let read = self.data.as_slice().read_at(buf, offset)?;
        self.reads.borrow_mut().push((offset, read));
        Ok(read)
    }
}

fn locate(data: Vec<u8>) -> ZipArchive<TestReader> {
    let len = data.len() as u64;
    let reader = TestReader {
        data,
        fail_at: Cell::new(None),
        reads: RefCell::new(Vec::new()),
    };
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(reader, &mut buffer, len)
        .map_err(|(_, e)| e)
        .unwrap();
    archive.get_ref().reads.take();
    archive
}

/// A record's offset, the `next_offset` after it, and its wayfinder.
type Seen = (u64, u64, ZipArchiveEntryWayfinder);

/// Up to `n` records from `entries`, or the first error.
fn take<R: ReaderAt>(entries: &mut ZipEntries<'_, '_, R>, n: usize) -> Result<Vec<Seen>, Error> {
    let mut seen = Vec::new();
    while seen.len() < n {
        let Some(record) = entries.next_entry()? else {
            break;
        };
        let (offset, wayfinder) = (record.central_directory_offset(), record.wayfinder());
        seen.push((offset, entries.next_offset(), wayfinder));
    }
    Ok(seen)
}

/// Iterate from `offset` to the end, with a new iterator from `next_offset`
/// after every `batch` records. Returns the records and the final
/// `next_offset`.
fn pass<R: ReaderAt>(
    archive: &ZipArchive<R>,
    mut offset: u64,
    batch: usize,
    buffer_size: usize,
) -> Result<(Vec<Seen>, u64), Error> {
    let mut buffer = vec![0u8; buffer_size];
    let mut seen = Vec::new();
    loop {
        let mut entries = archive.entries_from(&mut buffer, offset);
        let records = take(&mut entries, batch)?;
        offset = entries.next_offset();
        let ended = records.len() < batch;
        seen.extend(records);
        if ended {
            return Ok((seen, offset));
        }
    }
}

/// The first error iterating from `offset`, and `next_offset` after it.
fn first_error<R: ReaderAt>(archive: &ZipArchive<R>, offset: u64, size: usize) -> (Error, u64) {
    let mut buffer = vec![0u8; size];
    let mut entries = archive.entries_from(&mut buffer, offset);
    let err = take(&mut entries, usize::MAX).unwrap_err();
    assert!(entries.next_entry().unwrap().is_none());
    (err, entries.next_offset())
}

fn archive_with_names(names: &[String]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut writer = ZipArchiveWriter::new(&mut output);
    for name in names {
        let (entry, config) = writer.new_file(name).start().unwrap();
        let (entry, descriptor) = config.wrap(entry).finish().unwrap();
        entry.finish(descriptor).unwrap();
    }
    writer.finish().unwrap();
    output
}

fn many_entries() -> Vec<u8> {
    let names = (0..50)
        .map(|i| format!("dir/{}{i}.txt", "x".repeat(i * 3)))
        .collect::<Vec<_>>();
    archive_with_names(&names)
}

#[test]
fn entries_from_resumes_iteration() {
    for data in [
        std::fs::read("assets/test.zip").unwrap(),
        std::fs::read("assets/zip64.zip").unwrap(),
        std::fs::read("assets/test-prefix.zip").unwrap(),
        many_entries(),
        zip_with_cd_trailer("README", &digital_signature_record(b"signed")),
    ] {
        let archive = locate(data);
        let start = archive.directory_offset();
        let (expected, end) = pass(&archive, start, usize::MAX, RECOMMENDED_BUFFER_SIZE).unwrap();
        assert!(!expected.is_empty());

        // Each record starts at the `next_offset` before it.
        let previous = std::iter::once(start).chain(expected.iter().map(|x| x.1));
        assert!(
            expected
                .iter()
                .map(|x| x.0)
                .eq(previous.take(expected.len()))
        );

        // Resuming after every record, with the smallest buffer that holds
        // every record, gives the same records and end.
        let smallest = expected.iter().map(|x| (x.1 - x.0) as usize).max().unwrap();
        for batch in [1, usize::MAX] {
            let actual = pass(&archive, start, batch, smallest.max(HEADER_SIZE)).unwrap();
            assert_eq!(actual, (expected.clone(), end), "batch {batch}");
        }

        // Each record on demand from its offset, and nothing from the end.
        let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        for entry in &expected {
            let mut entries = archive.entries_from(&mut buffer, entry.0);
            assert_eq!(take(&mut entries, 1).unwrap(), [*entry]);
        }
        assert_eq!(pass(&archive, end, 1, 1024).unwrap(), (Vec::new(), end));
    }
}

#[test]
fn entries_from_rejects_bad_offsets() {
    let archive = locate(std::fs::read("assets/test.zip").unwrap());
    let start = archive.directory_offset();
    let (seen, end) = pass(&archive, start, usize::MAX, RECOMMENDED_BUFFER_SIZE).unwrap();
    archive.get_ref().reads.take();

    // Outside the directory: no reads at all.
    for offset in [0, start - 1, end + 1, u64::MAX] {
        let (err, next_offset) = first_error(&archive, offset, RECOMMENDED_BUFFER_SIZE);
        assert!(
            matches!(err.kind(), ErrorKind::InvalidInput { .. }),
            "{err:?}"
        );
        assert_eq!(next_offset, offset);
        assert_eq!(archive.get_ref().reads.take(), [], "offset {offset}");
    }

    // Inside a record but not at a signature: reads stay in the directory.
    for offset in [seen[0].0 + 1, seen[0].0 + HEADER_SIZE as u64, end - 1] {
        let (err, next_offset) = first_error(&archive, offset, RECOMMENDED_BUFFER_SIZE);
        let kind = err.kind();
        assert!(matches!(
            kind,
            ErrorKind::InvalidSignature { .. } | ErrorKind::Eof
        ));
        assert_eq!(next_offset, offset);
        for (read_offset, read) in archive.get_ref().reads.take() {
            assert!(read_offset >= start && read_offset + read as u64 <= end);
        }
    }
}

/// After an error, `next_offset` is the failed record, and a new iterator
/// from it tries that record again.
#[test]
fn entries_from_retries_failed_record() {
    // The variable section does not fit the buffer, then fits a larger one.
    let mut names = (0..10).map(|i| format!("short{i}")).collect::<Vec<_>>();
    names[4] = "l".repeat(600);
    let archive = locate(archive_with_names(&names));
    let start = archive.directory_offset();
    let (expected, end) = pass(&archive, start, usize::MAX, 1024).unwrap();
    let (err, failed) = first_error(&archive, start, 256);
    assert!(matches!(err.kind(), ErrorKind::BufferTooSmall { .. }));
    assert_eq!(failed, expected[4].0);
    assert_eq!(
        pass(&archive, failed, usize::MAX, 1024).unwrap(),
        (expected[4..].to_vec(), end)
    );

    // A transient read error in record 10: in its fixed header with none or
    // 20 bytes of it buffered, and in its variable section after the fixed
    // header was consumed. The same buffer size works on retry.
    let archive = locate(many_entries());
    let start = archive.directory_offset();
    let (expected, end) = pass(&archive, start, usize::MAX, RECOMMENDED_BUFFER_SIZE).unwrap();
    let k = expected[10].0;
    for into_k in [0, 20, 50] {
        let size = (k - start) as usize + into_k;
        archive.get_ref().fail_at.set(Some(k + into_k as u64));
        let (err, failed) = first_error(&archive, start, size);
        assert!(matches!(err.kind(), ErrorKind::IO(_)), "{err:?}");
        assert_eq!(failed, k, "{into_k} bytes into the record");
        assert_eq!(
            pass(&archive, k, usize::MAX, size).unwrap(),
            (expected[10..].to_vec(), end)
        );
    }

    // Corrupt directories fail at the same record again: a record whose file
    // name (length at byte 28) runs past the directory end, fewer than
    // `HEADER_SIZE` bytes that are not a digital signature at the directory
    // end, and a local header offset that overflows with the base offset.
    let mut overrun = std::fs::read("assets/test.zip").unwrap();
    let overrun_record = record_offset(&overrun, 1);
    overrun[overrun_record + 28..][..2].copy_from_slice(&1000u16.to_le_bytes());
    let trailer = zip_with_cd_trailer("a", b"0123456789");
    let trailer_offset = ZipArchive::from_slice(&trailer).unwrap().eocd_offset() - 10;
    let overflow = overflow_offset_zip();
    let overflow_record = ZipArchive::from_slice(&overflow)
        .unwrap()
        .directory_offset();
    for (data, failed) in [
        (overrun, overrun_record as u64),
        (trailer, trailer_offset),
        (overflow, overflow_record),
    ] {
        let archive = locate(data);
        let size = MAX_CENTRAL_DIRECTORY_RECORD_SIZE;
        let (err, next_offset) = first_error(&archive, archive.directory_offset(), size);
        assert!(matches!(err.kind(), ErrorKind::Eof), "{err:?}");
        assert_eq!(next_offset, failed);
        let (err, next_offset) = first_error(&archive, failed, size);
        assert!(matches!(err.kind(), ErrorKind::Eof), "{err:?}");
        assert_eq!(next_offset, failed);
    }
}
