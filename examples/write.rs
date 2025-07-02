use std::io::Write;

fn main() {
    let mut writer = rawzip::ZipArchiveWriter::new(std::io::stdout());
    writer.new_dir("dir/", rawzip::ZipEntryOptions::default()).unwrap();

    let options =
        rawzip::ZipEntryOptions::default().compression_method(rawzip::CompressionMethod::Deflate);

    let mut file = writer.new_file("dir/test.txt", options).unwrap();
    let output = {
        let mut encoder =
            flate2::write::DeflateEncoder::new(&mut file, flate2::Compression::default());
        let mut writer = rawzip::ZipDataWriter::new(&mut encoder);
        writer.write_all(b"Hello, world!").unwrap();
        let (_, output) = writer.finish().unwrap();
        output
    };
    file.finish(output).unwrap();
    writer.finish().unwrap();
}
