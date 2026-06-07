use rawzip_bench::{
    crc32::gungraun_benches::crc32_benches, reader::gungraun_benches::entries_benches,
    reader::gungraun_benches::locator_benches, writer::gungraun_benches::writer_benches,
};

gungraun::main!(
    library_benchmark_groups = crc32_benches,
    locator_benches,
    entries_benches,
    writer_benches
);
