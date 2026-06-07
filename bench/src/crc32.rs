pub mod criterion_benches {
    use crate::shared::SIZE_CASES;
    use criterion::{BenchmarkId, Criterion, Throughput};
    use std::hint::black_box;

    fn crc32(c: &mut Criterion) {
        let mut group = c.benchmark_group("crc32");

        for size in SIZE_CASES {
            let data = vec![0; size];
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
                b.iter(|| black_box(rawzip::crc32(data)));
            });
        }

        group.finish();
    }

    criterion::criterion_group!(crc32_benches, crc32);
}

#[cfg(not(target_family = "wasm"))]
pub mod gungraun_benches {
    use crate::shared::filled_bytes;
    use gungraun::{library_benchmark, library_benchmark_group};
    use std::hint::black_box;

    #[inline(never)]
    fn measure_crc32(data: &[u8]) -> u32 {
        rawzip::crc32(data)
    }

    #[library_benchmark]
    #[benches::sizes(args = [1usize, 4, 16, 64, 256, 1024, 4096, 16384, 65536], setup = filled_bytes::<0>)]
    fn crc32(data: Vec<u8>) -> u32 {
        black_box(measure_crc32(&data))
    }

    library_benchmark_group!(name = crc32_benches, benchmarks = [crc32]);
}
