use criterion::{BenchmarkId, Criterion, Throughput};
use std::io::{Cursor, Write};

fn crc32(c: &mut Criterion) {
    let mut group = c.benchmark_group("crc32");
    for size in &[1, 4, 16, 64, 256, 1024, 4096, 16384, 65536] {
        let data = vec![0; *size];
        let input = data.as_slice();
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _size| {
            b.iter(|| rawzip::crc32(input));
        });
    }
    group.finish();
}

fn eocd(c: &mut Criterion) {
    let mut group = c.benchmark_group("eocd-locator");
    for size in &[1, 4, 16, 64, 256, 1024, 4096, 16384, 65536] {
        let data = vec![4; *size];
        let input = data.as_slice();
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _size| {
            b.iter(|| rawzip::ZipArchive::from_slice(&input));
        });
    }
    group.finish();
}

fn create_test_zip() -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let mut archive = rawzip::ZipArchiveWriter::builder()
        .with_capacity(200_000)
        .build(&mut output);

    for i in 0..200_000 {
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

fn entries(c: &mut Criterion) {
    let zip_data = create_test_zip();
    let mut group = c.benchmark_group("entries");

    group.bench_function("slice", |b| {
        b.iter(|| {
            let archive = rawzip::ZipArchive::from_slice(&zip_data).unwrap();
            let mut total_size = 0u64;
            let mut entries = archive.entries();
            while let Ok(Some(entry)) = entries.next_entry() {
                total_size += entry.uncompressed_size_hint();
            }
            assert_eq!(total_size, 200_000);
        })
    });

    group.bench_function("reader", |b| {
        let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
        b.iter(|| {
            let cursor = Cursor::new(&zip_data);
            let archive = rawzip::ZipLocator::new()
                .locate_in_reader(cursor, &mut buffer, zip_data.len() as u64)
                .unwrap();
            let mut total_size = 0u64;
            let mut entries = archive.entries(&mut buffer);
            while let Ok(Some(entry)) = entries.next_entry() {
                total_size += entry.uncompressed_size_hint();
            }
            assert_eq!(total_size, 200_000);
        })
    });
}

criterion::criterion_group!(benches, crc32, eocd, entries);
criterion::criterion_main!(benches);
