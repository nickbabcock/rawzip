pub mod criterion_benches {
    use crate::shared::{
        create_test_zip, deterministic_random_bytes, filled_bytes, setup_reader_fixture,
        sum_slice_entries, LARGE_ENTRY_COUNT, SIZE_CASES,
    };
    use criterion::{BenchmarkId, Criterion, Throughput};
    use rawzip::ZipLocator;
    use std::hint::black_box;

    fn locate_reader_and_sum_entries(data: &[u8], buffer: &mut [u8], end_offset: u64) -> u64 {
        let archive = ZipLocator::new()
            .locate_in_reader(data, buffer, end_offset)
            .unwrap();
        let mut total_size = 0u64;
        let mut entries = archive.entries(buffer);
        while let Ok(Some(entry)) = entries.next_entry() {
            total_size += entry.uncompressed_size_hint();
        }
        total_size
    }

    fn locator(c: &mut Criterion) {
        let mut group = c.benchmark_group("eocd-locator");

        type SetupFn = fn(usize) -> Vec<u8>;
        let scenarios: [(&str, SetupFn); 3] = [
            ("no-candidates", filled_bytes::<4>),
            ("random", deterministic_random_bytes),
            ("all-candidates", filled_bytes::<6>),
        ];
        for (scenario, setup) in scenarios {
            for size in SIZE_CASES {
                let data = setup(size);
                group.throughput(Throughput::Bytes(size as u64));
                group.bench_with_input(BenchmarkId::new(scenario, size), &data, |b, data| {
                    b.iter(|| black_box(rawzip::ZipArchive::from_slice(data).is_ok()));
                });
            }
        }

        group.finish();
    }

    fn locator_valid(c: &mut Criterion) {
        let zip_data = create_test_zip(LARGE_ENTRY_COUNT);
        let mut fixture = setup_reader_fixture(LARGE_ENTRY_COUNT);
        let mut group = c.benchmark_group("eocd-locator-valid");

        group.bench_function("slice", |b| {
            b.iter(|| {
                black_box(
                    ZipLocator::new()
                        .locate_in_slice(zip_data.as_slice())
                        .map(|archive| archive.entries_hint())
                        .unwrap(),
                )
            })
        });

        group.bench_function("reader", |b| {
            b.iter(|| {
                black_box(
                    ZipLocator::new()
                        .locate_in_reader(
                            fixture.zip_data.as_slice(),
                            &mut fixture.buffer,
                            fixture.end_offset,
                        )
                        .map(|archive| archive.entries_hint())
                        .unwrap(),
                )
            })
        });

        group.finish();
    }

    fn entries(c: &mut Criterion) {
        let zip_data = create_test_zip(LARGE_ENTRY_COUNT);
        let mut reader_fixture = setup_reader_fixture(LARGE_ENTRY_COUNT);
        let expected_total_size = LARGE_ENTRY_COUNT as u64;
        let mut group = c.benchmark_group("entries");

        group.bench_function("slice", |b| {
            b.iter(|| {
                let archive = rawzip::ZipArchive::from_slice(&zip_data).unwrap();
                let total_size = sum_slice_entries(&archive);
                assert_eq!(total_size, expected_total_size);
                black_box(total_size)
            })
        });

        group.bench_function("reader", |b| {
            b.iter(|| {
                let total_size = locate_reader_and_sum_entries(
                    &reader_fixture.zip_data,
                    &mut reader_fixture.buffer,
                    reader_fixture.end_offset,
                );
                assert_eq!(total_size, reader_fixture.expected_total_size);
                black_box(total_size)
            })
        });

        group.finish();
    }

    criterion::criterion_group!(locator_benches, locator, locator_valid);
    criterion::criterion_group!(entries_benches, entries);
}

#[cfg(not(target_family = "wasm"))]
pub mod gungraun_benches {
    use crate::shared::{
        create_test_zip, deterministic_random_bytes, filled_bytes, setup_reader_entries_fixture,
        setup_reader_fixture, setup_slice_entries_fixture, ReaderEntriesFixture, ReaderFixture,
        SliceEntriesFixture,
    };
    use gungraun::{library_benchmark, library_benchmark_group};
    use std::hint::black_box;

    #[inline(never)]
    fn measure_missing_eocd(data: &[u8]) -> bool {
        rawzip::ZipLocator::new().locate_in_slice(data).is_ok()
    }

    #[inline(never)]
    fn measure_valid_locator(data: Vec<u8>) -> u64 {
        let archive = rawzip::ZipLocator::new().locate_in_slice(data).unwrap();
        archive.entries_hint()
    }

    #[inline(never)]
    fn measure_valid_archive_reader(mut fixture: ReaderFixture) -> u64 {
        rawzip::ZipLocator::new()
            .locate_in_reader(
                fixture.zip_data.as_slice(),
                &mut fixture.buffer,
                fixture.end_offset,
            )
            .map(|archive| archive.entries_hint())
            .unwrap()
    }

    #[inline(never)]
    fn measure_slice_entries(fixture: &SliceEntriesFixture) -> u64 {
        crate::shared::sum_slice_entries(&fixture.archive)
    }

    #[inline(never)]
    fn measure_reader_entries(fixture: &mut ReaderEntriesFixture) -> u64 {
        crate::shared::sum_reader_entries(&fixture.archive, &mut fixture.buffer)
    }

    #[library_benchmark]
    #[benches::sizes(args = [1usize, 4, 16, 64, 256, 1024, 4096, 16384, 65536], setup = filled_bytes::<4>)]
    fn locate_missing_eocd(data: Vec<u8>) -> bool {
        black_box(measure_missing_eocd(&data))
    }

    #[library_benchmark]
    #[benches::sizes(args = [1usize, 4, 16, 64, 256, 1024, 4096, 16384, 65536], setup = deterministic_random_bytes)]
    fn locate_missing_eocd_random(data: Vec<u8>) -> bool {
        black_box(measure_missing_eocd(&data))
    }

    // Worst case for the candidate-byte filter: every byte equals the EOCD
    // signature's last byte (0x06), so every position is a candidate that must
    // be confirmed (and rejected) by the full comparison.
    #[library_benchmark]
    #[benches::sizes(args = [1usize, 4, 16, 64, 256, 1024, 4096, 16384, 65536], setup = filled_bytes::<6>)]
    fn locate_missing_eocd_all_candidates(data: Vec<u8>) -> bool {
        black_box(measure_missing_eocd(&data))
    }

    #[library_benchmark]
    #[bench::large_directory(args = (200_000usize), setup = create_test_zip)]
    fn locate_valid_archive(data: Vec<u8>) -> u64 {
        black_box(measure_valid_locator(data))
    }

    #[library_benchmark]
    #[bench::large_directory(args = (200_000usize), setup = setup_reader_fixture)]
    fn locate_valid_archive_reader(fixture: ReaderFixture) -> u64 {
        black_box(measure_valid_archive_reader(fixture))
    }

    #[library_benchmark]
    #[bench::large_directory(args = (200_000usize), setup = setup_slice_entries_fixture)]
    fn iterate_slice_entries(fixture: SliceEntriesFixture) -> u64 {
        let total_size = measure_slice_entries(&fixture);
        assert_eq!(total_size, fixture.expected_total_size);
        black_box(total_size)
    }

    #[library_benchmark]
    #[bench::large_directory(args = (200_000usize), setup = setup_reader_entries_fixture)]
    fn iterate_reader_entries(mut fixture: ReaderEntriesFixture) -> u64 {
        let total_size = measure_reader_entries(&mut fixture);
        assert_eq!(total_size, fixture.expected_total_size);
        black_box(total_size)
    }

    library_benchmark_group!(
        name = locator_benches,
        benchmarks = [
            locate_missing_eocd,
            locate_missing_eocd_random,
            locate_missing_eocd_all_candidates,
            locate_valid_archive,
            locate_valid_archive_reader
        ]
    );

    library_benchmark_group!(
        name = entries_benches,
        benchmarks = [iterate_slice_entries, iterate_reader_entries]
    );
}
