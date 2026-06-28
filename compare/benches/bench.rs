use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rawzip::extra_fields::ExtraFieldId;
use std::hint::black_box;
use std::io::{Cursor, Write};

/// Number of entries in the archive walked by the read benchmarks.
const READ_ENTRIES: usize = 100_000;

/// Number of entries written by the write benchmarks.
const WRITE_ENTRIES: usize = 5_000;

fn create_test_zip<const TIMESTAMP: bool>(entries: usize) -> Vec<u8> {
    let jan_1_2001 = rawzip::time::UtcDateTime::from_components(2001, 1, 1, 0, 0, 0, 0).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut archive = rawzip::ZipArchiveWriter::builder()
        .with_capacity(entries)
        .build(&mut output);

    let mut names = NameIter::new();
    for i in 0..entries {
        let mut file_builder = archive
            .new_file(names.name_of(i))
            .compression_method(rawzip::CompressionMethod::STORE);
        if TIMESTAMP {
            file_builder = file_builder.last_modified(jan_1_2001);
        }
        let (mut entry, config) = file_builder.start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(b"x").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }

    archive.finish().unwrap();
    output.into_inner()
}

// Use case: compute an archive's overall compression ratio.
fn compression_ratio_benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("compression_ratio");

    let zip_data = create_test_zip::<true>(READ_ENTRIES);
    // These are per-entry workloads, so throughput is reported in entries/sec.
    group.throughput(Throughput::Elements(READ_ENTRIES as u64));

    // rawzip slice API: bytes already in memory, so the central directory is
    // walked zero-copy with no allocations.
    group.bench_function("rawzip_slice", |b| {
        b.iter(|| {
            let archive = rawzip::ZipArchive::from_slice(&zip_data).unwrap();
            let (mut compressed, mut uncompressed) = (0u64, 0u64);
            let mut entries = archive.entries();
            while let Some(entry) = entries.next_entry().unwrap() {
                compressed += entry.compressed_size_hint();
                uncompressed += entry.uncompressed_size_hint();
            }
            black_box((compressed, uncompressed))
        })
    });

    // rawzip reader API: streamed through one reused scratch buffer, backed by
    // an in-memory slice to keep the measurement free of disk/page-cache noise.
    group.bench_function("rawzip_reader", |b| {
        let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
        b.iter(|| {
            let archive = rawzip::ZipLocator::new()
                .locate_in_reader(zip_data.as_slice(), &mut buffer, zip_data.len() as u64)
                .unwrap();
            let (mut compressed, mut uncompressed) = (0u64, 0u64);
            let mut entries = archive.entries(&mut buffer);
            while let Some(entry) = entries.next_entry().unwrap() {
                compressed += entry.compressed_size_hint();
                uncompressed += entry.uncompressed_size_hint();
            }
            black_box((compressed, uncompressed))
        })
    });

    group.bench_function("zip", |b| {
        b.iter(|| {
            let cursor = Cursor::new(&zip_data);
            let mut archive = zip::ZipArchive::new(cursor).unwrap();
            let (mut compressed, mut uncompressed) = (0u64, 0u64);
            for i in 0..archive.len() {
                let entry = archive.by_index_raw(i).unwrap();
                compressed += entry.compressed_size();
                uncompressed += entry.size();
            }
            black_box((compressed, uncompressed))
        })
    });

    group.bench_function("rc_zip", |b| {
        b.iter(|| {
            use rc_zip_sync::ReadZip;
            let slice = zip_data.as_slice();
            let reader = slice.read_zip().unwrap();
            let totals = reader.entries().fold((0u64, 0u64), |(c, u), entry| {
                (c + entry.compressed_size, u + entry.uncompressed_size)
            });
            black_box(totals)
        })
    });

    group.bench_function("async_zip", |b| {
        b.to_async(&rt).iter(|| async {
            use async_zip::base::read::seek::ZipFileReader;
            use tokio_util::compat::TokioAsyncReadCompatExt;

            let cursor = Cursor::new(&zip_data);
            let reader = ZipFileReader::new(cursor.compat()).await.unwrap();
            let totals = reader
                .file()
                .entries()
                .iter()
                .fold((0u64, 0u64), |(c, u), entry| {
                    (c + entry.compressed_size(), u + entry.uncompressed_size())
                });
            black_box(totals)
        })
    });

    group.finish();
}

// Use case: extract entries from a large archive (0-byte store entries)
fn extract_benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("extract");

    let zip_data = create_test_zip::<true>(READ_ENTRIES);

    group.throughput(Throughput::Elements(1));
    for (label, take_all) in [("first", false), ("all", true)] {
        group.bench_function(BenchmarkId::new("rawzip_slice", label), |b| {
            b.iter(|| {
                let archive = rawzip::ZipArchive::from_slice(&zip_data).unwrap();
                let mut bytes = 0u64;
                let mut entries = archive.entries();
                while let Some(entry) = entries.next_entry().unwrap() {
                    let zip_entry = archive.get_entry(entry.wayfinder()).unwrap();
                    bytes += zip_entry.data().len() as u64;
                    if !take_all {
                        break;
                    }
                }
                black_box(bytes)
            })
        });

        group.bench_function(BenchmarkId::new("rawzip_reader", label), |b| {
            let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
            b.iter(|| {
                let archive = rawzip::ZipLocator::new()
                    .locate_in_reader(zip_data.as_slice(), &mut buffer, zip_data.len() as u64)
                    .unwrap();
                let mut bytes = 0u64;
                let mut entries = archive.entries(&mut buffer);
                while let Some(entry) = entries.next_entry().unwrap() {
                    let zip_entry = archive.get_entry(entry.wayfinder()).unwrap();
                    let reader = zip_entry.reader();
                    let mut verifier = zip_entry.verifying_reader(reader);
                    bytes += std::io::copy(&mut verifier, &mut std::io::sink()).unwrap();
                    if !take_all {
                        break;
                    }
                }
                black_box(bytes)
            })
        });

        // zip: eager open builds the whole index, then each entry is extracted
        // by position.
        group.bench_function(BenchmarkId::new("zip", label), |b| {
            b.iter(|| {
                let cursor = Cursor::new(&zip_data);
                let mut archive = zip::ZipArchive::new(cursor).unwrap();
                let n = if take_all { archive.len() } else { 1 };
                let mut bytes = 0u64;
                for i in 0..n {
                    let mut file = archive.by_index(i).unwrap();
                    bytes += std::io::copy(&mut file, &mut std::io::sink()).unwrap();
                }
                black_box(bytes)
            })
        });

        // rc_zip: eager open, then each entry is read from the parsed list.
        group.bench_function(BenchmarkId::new("rc_zip", label), |b| {
            b.iter(|| {
                use rc_zip_sync::ReadZip;
                let slice = zip_data.as_slice();
                let reader = slice.read_zip().unwrap();
                let n = if take_all { usize::MAX } else { 1 };
                let mut bytes = 0u64;
                for handle in reader.entries().take(n) {
                    bytes += handle.bytes().unwrap().len() as u64;
                }
                black_box(bytes)
            })
        });

        // async_zip: eager open, then each entry is read by index.
        group.bench_function(BenchmarkId::new("async_zip", label), |b| {
            b.to_async(&rt).iter(|| async {
                use async_zip::base::read::seek::ZipFileReader;
                use tokio_util::compat::TokioAsyncReadCompatExt;

                let cursor = Cursor::new(&zip_data);
                let mut reader = ZipFileReader::new(cursor.compat()).await.unwrap();
                let n = if take_all {
                    reader.file().entries().len()
                } else {
                    1
                };
                let mut bytes = 0u64;
                let mut buf = Vec::new();
                for i in 0..n {
                    buf.clear();
                    let mut entry_reader = reader.reader_with_entry(i).await.unwrap();
                    entry_reader.read_to_end_checked(&mut buf).await.unwrap();
                    bytes += buf.len() as u64;
                }
                black_box(bytes)
            })
        });
    }

    group.finish();
}

// Use case: package N files into an archive.
fn write_benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("write");

    // Extra fields (timestamps). Throughput is keyed to the archive size this
    // variant produces.
    let extra_fields_bytes = create_test_zip::<true>(WRITE_ENTRIES).len() as u64;
    group.throughput(Throughput::Bytes(extra_fields_bytes));

    group.bench_function("extra_fields/rawzip", |b| {
        b.iter(|| create_test_zip::<true>(WRITE_ENTRIES));
    });

    group.bench_function("extra_fields/zip", |b| {
        b.iter(|| {
            let mut output = Cursor::new(Vec::new());
            let mut archive = zip::ZipWriter::new(&mut output);

            // The zip crate does not emit the extended timestamp field, so add
            // it manually to keep the comparison apples-to-apples.
            let jan_1_2001 =
                rawzip::time::UtcDateTime::from_components(2001, 1, 1, 0, 0, 0, 0).unwrap();
            let mut data = [0u8; 5];
            data[0] = 1;
            data[1..5].copy_from_slice((jan_1_2001.to_unix() as u32).to_le_bytes().as_ref());
            let mut names = NameIter::new();

            let mut options: zip::write::FileOptions<zip::write::ExtendedFileOptions> =
                zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
            options
                .add_extra_data(ExtraFieldId::EXTENDED_TIMESTAMP.as_u16(), data, true)
                .unwrap();

            for i in 0..WRITE_ENTRIES {
                archive
                    .start_file(names.name_of(i), options.clone())
                    .unwrap();
                archive.write_all(b"x").unwrap();
            }

            archive.finish().unwrap();
            output.into_inner()
        });
    });

    // No extra fields. Throughput is keyed to the smaller minimal archive size.
    let minimal_bytes = create_test_zip::<false>(WRITE_ENTRIES).len() as u64;
    group.throughput(Throughput::Bytes(minimal_bytes));

    group.bench_function("minimal/rawzip", |b| {
        b.iter(|| create_test_zip::<false>(WRITE_ENTRIES));
    });

    group.bench_function("minimal/zip", |b| {
        b.iter(|| {
            let mut output = Cursor::new(Vec::new());
            let mut archive = zip::ZipWriter::new(&mut output);

            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);

            let mut names = NameIter::new();

            for i in 0..WRITE_ENTRIES {
                archive.start_file(names.name_of(i), options).unwrap();
                archive.write_all(b"x").unwrap();
            }

            archive.finish().unwrap();
            output.into_inner()
        });
    });

    group.bench_function("minimal/async_zip", |b| {
        b.to_async(&rt).iter(|| async {
            use async_zip::ZipEntryBuilder;
            use async_zip::base::write::ZipFileWriter;

            let mut output = Cursor::new(Vec::new());
            let mut archive = ZipFileWriter::with_tokio(&mut output);

            let mut names = NameIter::new();

            for i in 0..WRITE_ENTRIES {
                let entry =
                    ZipEntryBuilder::new(names.name_of(i).into(), async_zip::Compression::Stored);
                archive.write_entry_whole(entry, b"x").await.unwrap();
            }

            archive.close().await.unwrap();
            output.into_inner()
        });
    });

    group.finish();
}

struct NameIter {
    buf: String,
}

impl NameIter {
    fn new() -> Self {
        Self { buf: String::new() }
    }

    #[inline]
    fn name_of(&mut self, ind: usize) -> &str {
        self.buf.clear();
        self.buf.push_str("file");
        let mut j = ind;
        while j > 0 {
            let digit = (j % 10) as u8;
            self.buf.push((b'0' + digit) as char);
            j /= 10;
        }
        self.buf.push_str(".txt");
        &self.buf
    }
}

criterion_group!(
    benches,
    compression_ratio_benchmarks,
    extract_benchmarks,
    write_benchmarks
);
criterion_main!(benches);
