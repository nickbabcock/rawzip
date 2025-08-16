use criterion::{criterion_group, criterion_main, Criterion};
use std::io::{Cursor, Write};

fn create_test_zip() -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let mut archive = rawzip::ZipArchiveWriter::builder()
        .with_capacity(100_000)
        .build(&mut output);

    for i in 0..100_000 {
        let filename = format!("file{:06}.txt", i);
        let mut file = archive
            .new_file(&filename)
            .compression_method(rawzip::CompressionMethod::Store)
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
    let zip_data = create_test_zip();
    group.throughput(criterion::Throughput::Bytes(zip_data.len() as u64));

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

criterion_group!(benches, parse_benchmarks);
criterion_main!(benches);
