//! Example demonstrating creation and validation of a large ZIP archive.
//!
//! This example creates a ZIP file with 100,001 entries:
//! - 100,000 small text files (using Store compression)
//! - 1 large file containing 5GB of zeros (using Zstd compression)
//!
//! After creation, it validates the archive by reading it back and verifying:
//! - Correct number of entries (100,001)
//! - The 5GB file contains only zeros
//!
//! Run in release mode.

use rawzip::ZipArchiveWriter;
use std::io::{Read, Write};

/// A reader that yields zeros without storing them in memory
struct ZeroReader {
    remaining: u64,
}

impl ZeroReader {
    fn new(size: u64) -> Self {
        ZeroReader { remaining: size }
    }
}

impl Read for ZeroReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }

        let len = std::cmp::min(buf.len() as u64, self.remaining) as usize;
        buf[..len].fill(0);
        self.remaining -= len as u64;
        Ok(len)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_file = std::fs::File::create("big.zip")?;
    let writer = std::io::BufWriter::new(output_file);
    let mut archive = ZipArchiveWriter::new(writer);

    // Add 100,000 small files with Store compression
    for i in 0..100_000 {
        let filename = format!("file_{:05}.txt", i);

        let (mut entry, config) = archive
            .new_file(&filename)
            .compression_method(rawzip::CompressionMethod::Store)
            .start()?;

        let mut data_writer = config.wrap(&mut entry);
        data_writer.write_all(b"x")?;
        let (_, output) = data_writer.finish()?;
        entry.finish(output)?;
    }

    println!("  Added 100,000 small files");
    println!("  Adding 5GB zero file with Zstd compression...");

    let (mut big_entry, config) = archive
        .new_file("big_zeros.dat")
        .compression_method(rawzip::CompressionMethod::Zstd)
        .start()?;

    let encoder = zstd::Encoder::new(&mut big_entry, 3)?; // Compression level 3
    let mut data_writer = config.wrap(encoder);
    let mut zero_reader = ZeroReader::new(5 * 1024 * 1024 * 1024); // 5GB
    std::io::copy(&mut zero_reader, &mut data_writer)?;

    let (encoder, output) = data_writer.finish()?;
    encoder.finish()?;
    big_entry.finish(output)?;
    archive.finish()?;
    println!("Successfully created big.zip with 100,001 entries");

    let zip_file = std::fs::File::open("big.zip")?;
    let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = rawzip::ZipArchive::from_file(zip_file, &mut buffer)?;

    assert_eq!(
        archive.entries_hint(),
        100_001,
        "Expected 100,001 entries in the archive"
    );

    let mut big_file_wayfinder = None;
    let mut big_file_compression = rawzip::CompressionMethod::Store;
    let mut entries = archive.entries(&mut buffer);
    let mut entry_count = 0;
    while let Some(entry) = entries.next_entry()? {
        entry_count += 1;
        if entry.file_path().as_ref() == b"big_zeros.dat" {
            big_file_wayfinder = Some(entry.wayfinder());
            big_file_compression = entry.compression_method();
            break;
        }
    }

    assert_eq!(
        entry_count, 100_001,
        "Expected 100,001 entries in the archive"
    );

    let wayfinder = big_file_wayfinder.expect("big_zeros.dat wayfinder not found");
    let big_file_size = wayfinder.uncompressed_size_hint();
    assert_eq!(
        big_file_size,
        5 * 1024 * 1024 * 1024,
        "Expected big_zeros.dat to be 5GB"
    );

    let zip_entry = archive.get_entry(wayfinder)?;
    let reader = zip_entry.reader();

    assert_eq!(
        big_file_compression,
        rawzip::CompressionMethod::Zstd,
        "Expected big_zeros.dat to use Zstd compression"
    );
    let decoder = zstd::Decoder::new(reader)?;
    let mut reader = zip_entry.verifying_reader(decoder);
    let total_read = std::io::copy(&mut reader, &mut std::io::sink())?;
    assert_eq!(
        total_read,
        5 * 1024 * 1024 * 1024,
        "Expected to read exactly 5GB from big_zeros.dat"
    );
    Ok(())
}
