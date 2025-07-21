use criterion::{criterion_group, criterion_main, Criterion};
use rawzip::extra_fields::ExtraFieldId;
use std::io::{Cursor, Write};

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
            .compression_method(rawzip::CompressionMethod::Store);
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

fn parse_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    let zip_data = create_test_zip::<true>(100_000);
    let setup_zip = rawzip::ZipArchive::from_slice(&zip_data).unwrap();
    let throughput = zip_data.len() as u64 - setup_zip.directory_offset();
    group.throughput(criterion::Throughput::Bytes(throughput));

    group.bench_function("rawzip", |b| {
        #[inline(never)]
        fn rawzip_bench(zip_data: &[u8]) {
            let archive = rawzip::ZipArchive::from_slice(&zip_data).unwrap();
            let mut total_size = 0u64;
            let mut entries = archive.entries();
            while let Ok(Some(entry)) = entries.next_entry() {
                total_size += entry.uncompressed_size_hint();
            }
            assert_eq!(total_size, 100_000);
        }

        b.iter(|| {
            rawzip_bench(&zip_data);
        });
    });

    group.bench_function("rc_zip", |b| {
        b.iter(|| {
            use rc_zip_sync::ReadZip;

            let slice = &zip_data[..];
            let reader = slice.read_zip().unwrap();
            let total_size = reader.entries().map(|x| x.uncompressed_size).sum::<u64>();
            assert_eq!(total_size, 100_000);
        })
    });

    group.bench_function("zip", |b| {
        b.iter(|| {
            use zip::ZipArchive;

            let cursor = Cursor::new(&zip_data);
            let mut archive = ZipArchive::new(cursor).unwrap();
            let entries = archive.len();
            let mut total_size = 0u64;
            for ind in 0..entries {
                let entry = archive.by_index_raw(ind).unwrap();
                total_size += entry.size();
            }
            assert_eq!(total_size, 100_000);
        })
    });

    group.bench_function("async_zip", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                use async_zip::base::read::seek::ZipFileReader;
                use tokio_util::compat::TokioAsyncReadCompatExt;

                let cursor = Cursor::new(&zip_data);
                let reader = ZipFileReader::new(cursor.compat()).await.unwrap();
                let sum = reader
                    .file()
                    .entries()
                    .iter()
                    .map(|x| x.uncompressed_size())
                    .sum::<u64>();
                assert_eq!(sum, 100_000);
            })
    });

    group.finish();
}

fn write_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("write");

    // Calculate throughput for the with-extra-fields case (larger due to extra data)
    let zip_with_extra_fields = create_test_zip::<true>(5000);
    group.throughput(criterion::Throughput::Bytes(
        zip_with_extra_fields.len() as u64
    ));

    // Benchmarks with extra fields (timestamps)
    group.bench_function("extra_fields/rawzip", |b| {
        b.iter(|| create_test_zip::<true>(5000));
    });

    group.bench_function("extra_fields/zip", |b| {
        b.iter(|| {
            let mut output = Cursor::new(Vec::new());
            let mut archive = zip::ZipWriter::new(&mut output);

            // It doesn't look like the rust zip implementation writes out
            // the extended timestamp field so we manually do it to make it
            // a more apples to apples comparison.
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
                .add_extra_data(
                    ExtraFieldId::EXTENDED_TIMESTAMP.as_u16(),
                    Box::new(data),
                    true,
                )
                .unwrap();

            for i in 0..5000 {
                archive
                    .start_file(names.name_of(i), options.clone())
                    .unwrap();
                archive.write_all(b"x").unwrap();
            }

            archive.finish().unwrap();
            output.into_inner()
        });
    });

    // Benchmarks without extra fields (no timestamps)
    group.bench_function("minimal/rawzip", |b| {
        b.iter(|| create_test_zip::<false>(5000));
    });

    group.bench_function("minimal/zip", |b| {
        b.iter(|| {
            let mut output = Cursor::new(Vec::new());
            let mut archive = zip::ZipWriter::new(&mut output);

            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);

            let mut names = NameIter::new();

            for i in 0..5000 {
                archive.start_file(names.name_of(i), options).unwrap();
                archive.write_all(b"x").unwrap();
            }

            archive.finish().unwrap();
            output.into_inner()
        });
    });

    group.bench_function("minimal/async_zip", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                use async_zip::base::write::ZipFileWriter;
                use async_zip::ZipEntryBuilder;

                let mut output = Cursor::new(Vec::new());
                let mut archive = ZipFileWriter::with_tokio(&mut output);

                let mut names = NameIter::new();

                for i in 0..5000 {
                    let entry = ZipEntryBuilder::new(
                        names.name_of(i).into(),
                        async_zip::Compression::Stored,
                    );
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

criterion_group!(benches, parse_benchmarks, write_benchmarks);
criterion_main!(benches);
