use criterion::criterion_main;
use rawzip_bench::{crc32, reader, writer};

criterion_main!(
    crc32::criterion_benches::crc32_benches,
    reader::criterion_benches::locator_benches,
    reader::criterion_benches::entries_benches,
    writer::criterion_benches::writer_benches,
);
