use criterion::{criterion_group, criterion_main, Criterion};
use std::io::{Cursor, Write};

fn create_test_zip(entries: usize) -> Vec<u8> {
    let jan_1_2001 = rawzip::time::UtcDateTime::from_components(2001, 1, 1, 0, 0, 0, 0).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut archive = rawzip::ZipArchiveWriter::builder()
        .with_capacity(entries)
        .build(&mut output);

    let mut filename = String::new();
    for i in 0..entries {
        filename.clear();
        filename.push_str("file");
        let mut j = i;
        while j > 0 {
            let digit = (j % 10) as u8;
            filename.push((b'0' + digit) as char);
            j /= 10;
        }
        filename.push_str(".txt");

        let mut file = archive
            .new_file(&filename)
            .compression_method(rawzip::CompressionMethod::Store)
            .last_modified(jan_1_2001)
            .create()
            .unwrap();
        let mut writer = rawzip::ZipDataWriter::new(&mut file);
        writer.write_all(b"x").unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        file.finish(descriptor).unwrap();
    }

    archive.finish().unwrap();
    output.into_inner()
}

fn parse_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    let zip_data = create_test_zip(100_000);
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

    let zip = create_test_zip(5000);

    group.throughput(criterion::Throughput::Bytes(zip.len() as u64));
    group.bench_function("rawzip", |b| {
        b.iter(|| create_test_zip(5000));
    });

    group.bench_function("zip", |b| {
        b.iter(|| {
            let mut output = Cursor::new(Vec::new());
            let mut archive = zip::ZipWriter::new(&mut output);

            let time = zip::DateTime::from_date_and_time(2001, 1, 1, 0, 0, 0).unwrap();

            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .last_modified_time(time);

            let mut filename = String::new();
            for i in 0..5000 {
                filename.clear();
                filename.push_str("file");
                let mut j = i;
                while j > 0 {
                    let digit = (j % 10) as u8;
                    filename.push((b'0' + digit) as char);
                    j /= 10;
                }
                filename.push_str(".txt");

                archive.start_file(&filename, options).unwrap();
                archive.write_all(b"Hello, World!").unwrap();
            }

            archive.finish().unwrap();
            output.into_inner()
        });
    });

    group.finish();
}

criterion_group!(benches, parse_benchmarks, write_benchmarks);
criterion_main!(benches);
