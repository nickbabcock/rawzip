use rawzip::{time::UtcDateTime, ZipArchiveWriter};
use std::io::{Cursor, Write};

fn write_archive(
    file_names: &[String],
    last_modified: Option<UtcDateTime>,
    output: &mut Vec<u8>,
) -> usize {
    output.clear();

    {
        let mut cursor = Cursor::new(&mut *output);
        let mut archive = ZipArchiveWriter::builder()
            .with_capacity(file_names.len())
            .build(&mut cursor);

        for file_name in file_names {
            let mut builder = archive
                .new_file(file_name)
                .compression_method(rawzip::CompressionMethod::STORE);

            if let Some(last_modified) = last_modified {
                builder = builder.last_modified(last_modified);
            }

            let (mut entry, config) = builder.start().unwrap();
            let mut writer = config.wrap(&mut entry);
            writer.write_all(crate::shared::STORED_FILE_DATA).unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            entry.finish(descriptor).unwrap();
        }

        archive.finish().unwrap();
    }

    output.len()
}

pub mod criterion_benches {
    use crate::shared::{
        setup_writer_fixture_minimal, setup_writer_fixture_with_timestamps, WRITER_ENTRY_COUNT,
    };
    use criterion::Criterion;
    use std::hint::black_box;

    fn create_zips(c: &mut Criterion) {
        let mut timestamps_fixture = setup_writer_fixture_with_timestamps(WRITER_ENTRY_COUNT);
        let mut minimal_fixture = setup_writer_fixture_minimal(WRITER_ENTRY_COUNT);
        let mut group = c.benchmark_group("write");

        group.bench_function("timestamps", |b| {
            b.iter(|| {
                black_box(super::write_archive(
                    &timestamps_fixture.file_names,
                    timestamps_fixture.last_modified,
                    &mut timestamps_fixture.output,
                ))
            })
        });

        group.bench_function("minimal", |b| {
            b.iter(|| {
                black_box(super::write_archive(
                    &minimal_fixture.file_names,
                    minimal_fixture.last_modified,
                    &mut minimal_fixture.output,
                ))
            })
        });

        group.finish();
    }

    criterion::criterion_group!(writer_benches, create_zips);
}

#[cfg(not(target_family = "wasm"))]
pub mod gungraun_benches {
    use crate::shared::{
        setup_writer_fixture_minimal, setup_writer_fixture_with_timestamps, WriterFixture,
    };
    use gungraun::{library_benchmark, library_benchmark_group};
    use std::hint::black_box;

    #[inline(never)]
    fn measure_write(fixture: &mut WriterFixture) -> usize {
        super::write_archive(
            &fixture.file_names,
            fixture.last_modified,
            &mut fixture.output,
        )
    }

    #[library_benchmark]
    #[bench::files_5k(args = (5_000usize), setup = setup_writer_fixture_with_timestamps)]
    fn write_with_timestamps(mut fixture: WriterFixture) -> usize {
        black_box(measure_write(&mut fixture))
    }

    #[library_benchmark]
    #[bench::files_5k(args = (5_000usize), setup = setup_writer_fixture_minimal)]
    fn write_minimal(mut fixture: WriterFixture) -> usize {
        black_box(measure_write(&mut fixture))
    }

    library_benchmark_group!(
        name = writer_benches,
        benchmarks = [write_with_timestamps, write_minimal]
    );
}
