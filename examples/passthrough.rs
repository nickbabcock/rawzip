//! Copy compressed file's data **verbatim** from one archive to another — no
//! decompression, no re-compression. Each entry's already-compressed bytes are
//! read from the source and written straight to the destination, preserving the
//! original compression method, using `DataDescriptorOutput::new` to supply the
//! known CRC and uncompressed size.
//!
//! This is the fast path for tools that rewrite an archive while leaving most
//! entries untouched (adding, removing, or replacing a few files): the bulk of
//! the data is copied through without ever being inflated.
//!
//! Run: `cargo run --example passthrough`

use std::io::Write;

use rawzip::{CompressionMethod, DataDescriptorOutput, ZipArchive, ZipArchiveWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build a small source archive with a Deflate-compressed entry and a
    //    Stored entry, so the copy exercises both.
    let source = build_source()?;

    // 2. Copy every entry raw into a fresh archive.
    let copied = copy_verbatim(&source)?;

    // 3. Verify: the copy decodes to the same content, entry for entry.
    verify(&source, &copied)?;

    println!(
        "passthrough copy verified: {} bytes -> {} bytes",
        source.len(),
        copied.len()
    );
    Ok(())
}

fn build_source() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut out);

    // Deflate entry.
    let (mut entry, config) = archive
        .new_file("deflated.txt")
        .compression_method(CompressionMethod::DEFLATE)
        .start()?;
    let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
    let mut writer = config.wrap(encoder);
    writer.write_all(
        b"the quick brown fox jumps over the lazy dog\n"
            .repeat(64)
            .as_slice(),
    )?;
    let (encoder, desc) = writer.finish()?;
    encoder.finish()?;
    entry.finish(desc)?;

    // Stored entry.
    let (mut entry, config) = archive
        .new_file("stored.bin")
        .compression_method(CompressionMethod::STORE)
        .start()?;
    let mut writer = config.wrap(&mut entry);
    writer.write_all(b"raw stored bytes")?;
    let (_, desc) = writer.finish()?;
    entry.finish(desc)?;

    archive.finish()?;
    Ok(out.into_inner())
}

fn copy_verbatim(source: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let archive = ZipArchive::from_slice(source)?;
    let mut out = std::io::Cursor::new(Vec::new());
    let mut writer = ZipArchiveWriter::new(&mut out);

    let mut entries = archive.entries();
    while let Some(dir) = entries.next_entry()? {
        if dir.is_dir() {
            continue;
        }
        // Read the entry's metadata + raw (still-compressed) bytes.
        let name = dir.file_path().try_normalize()?.as_ref().to_string();
        let crc = dir.crc32();
        let uncompressed_size = dir.uncompressed_size_hint();
        let method = dir.compression_method();
        let local = archive.get_entry(dir.wayfinder())?;
        let raw = local.data();

        // Write those bytes straight through — no compressor — declaring the
        // original method and the known CRC + uncompressed size.
        let (mut entry, _config) = writer
            .new_file(name.as_str())
            .compression_method(method)
            .start()?;
        entry.write_all(raw)?;
        entry.finish(DataDescriptorOutput::new(crc, uncompressed_size))?;
    }

    writer.finish()?;
    Ok(out.into_inner())
}

/// A decoded archive: (entry name, uncompressed content) per file entry.
type Decoded = Vec<(String, Vec<u8>)>;

fn verify(source: &[u8], copied: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let a = decode_all(source)?;
    let b = decode_all(copied)?;
    assert_eq!(a, b, "copied archive content differs from source");
    Ok(())
}

/// Decompress every file entry of `bytes` to (name, content), verifying each
/// entry's CRC via `verifying_reader`.
fn decode_all(bytes: &[u8]) -> Result<Decoded, Box<dyn std::error::Error>> {
    let archive = ZipArchive::from_slice(bytes)?;
    let mut entries = archive.entries();
    let mut result = Vec::new();
    while let Some(dir) = entries.next_entry()? {
        if dir.is_dir() {
            continue;
        }
        let name = dir.file_path().try_normalize()?.as_ref().to_string();
        let entry = archive.get_entry(dir.wayfinder())?;
        let decoder: Box<dyn std::io::Read> = match dir.compression_method() {
            CompressionMethod::STORE => Box::new(entry.data()),
            CompressionMethod::DEFLATE => {
                Box::new(flate2::bufread::DeflateDecoder::new(entry.data()))
            }
            other => return Err(format!("unexpected method {other:?}").into()),
        };
        let mut verifier = entry.verifying_reader(decoder);
        let mut content = Vec::new();
        std::io::copy(&mut verifier, &mut content)?;
        result.push((name, content));
    }
    Ok(result)
}
