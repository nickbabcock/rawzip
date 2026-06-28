use flate2::read::DeflateDecoder;
use rawzip::{CompressionMethod, ZipArchive, ZipArchiveWriter};
use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Barrier};

const FIRST_CONTENT: &[u8] = b"first entry: the quick brown fox jumps over the lazy dog\n";
const SECOND_CONTENT: &[u8] = b"second entry: sphinx of black quartz, judge my vow\nsecond line\n";

struct GateReader<R> {
    inner: R,
    barrier: Arc<Barrier>,
    entered: bool,
}

impl<R> GateReader<R> {
    fn new(inner: R, barrier: Arc<Barrier>) -> Self {
        Self {
            inner,
            barrier,
            entered: false,
        }
    }
}

impl<R: Read> Read for GateReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.entered {
            self.entered = true;
            self.barrier.wait();
        }

        self.inner.read(buf)
    }
}

fn build_two_entry_deflate_zip() -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut output);

    for (name, content) in [("first.txt", FIRST_CONTENT), ("second.txt", SECOND_CONTENT)] {
        let (mut entry, config) = archive
            .new_file(name)
            .compression_method(CompressionMethod::DEFLATE)
            .start()
            .unwrap();
        let encoder =
            flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
        let mut writer = config.wrap(encoder);
        writer.write_all(content).unwrap();
        let (encoder, descriptor) = writer.finish().unwrap();
        encoder.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }

    archive.finish().unwrap();
    output.into_inner()
}

fn read_all_verified(mut reader: impl Read) -> Vec<u8> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    output
}

#[test]
fn reader_api_decompresses_entries_in_parallel() {
    let data = build_two_entry_deflate_zip();
    let archive = ZipArchive::from_slice(data.as_slice())
        .unwrap()
        .into_reader();

    let barrier = Arc::new(Barrier::new(2));
    let expected = [FIRST_CONTENT, SECOND_CONTENT];
    let mut buffer = [0u8; 1024];
    let mut entries = archive.entries(&mut buffer);

    std::thread::scope(|scope| {
        let mut handles = Vec::new();

        while let Some(entry) = entries.next_entry().unwrap() {
            let wayfinder = entry.wayfinder();
            let barrier = Arc::clone(&barrier);
            let archive = &archive;
            handles.push(scope.spawn(move || {
                let entry = archive.get_entry(wayfinder).unwrap();
                let compressed = GateReader::new(entry.reader(), barrier);
                let inflater = DeflateDecoder::new(compressed);
                read_all_verified(entry.verifying_reader(inflater))
            }));
        }

        assert_eq!(handles.len(), expected.len());
        for (handle, expected) in handles.into_iter().zip(expected) {
            assert_eq!(handle.join().unwrap(), expected);
        }
    });
}

#[test]
fn slice_api_decompresses_entries_in_parallel() {
    let data = build_two_entry_deflate_zip();
    let archive = ZipArchive::from_slice(data.as_slice()).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let expected = [FIRST_CONTENT, SECOND_CONTENT];
    let mut entries = archive.entries();

    std::thread::scope(|scope| {
        let mut handles = Vec::new();

        while let Some(entry) = entries.next_entry().unwrap() {
            let wayfinder = entry.wayfinder();
            let barrier = Arc::clone(&barrier);
            let archive = &archive;
            handles.push(scope.spawn(move || {
                let entry = archive.get_entry(wayfinder).unwrap();
                let compressed = GateReader::new(entry.data(), barrier);
                let inflater = DeflateDecoder::new(compressed);
                read_all_verified(entry.verifying_reader(inflater))
            }));
        }

        assert_eq!(handles.len(), expected.len());
        for (handle, expected) in handles.into_iter().zip(expected) {
            assert_eq!(handle.join().unwrap(), expected);
        }
    });
}
