There are a few ways one can measure an entity's integrity.

NOTE: For a more holistic view of what goes into safely extracting files, see the README's [Security section][security] and the [extractor example][extract-example].

The quickstart's [`verifying_reader`][crate::ZipSliceEntry::verifying_reader]
is the normal read path. It checks the decompressed CRC-32 and byte count against
the central directory. This is where most zip libraries stop.

A stricter policy can be created that requires the local header or trailing data descriptor
to repeat the central directory's CRC and sizes.

```rust
# use std::io::Write;
# fn deflate_zip(body: &[u8]) -> Vec<u8> {
#     let mut output = Vec::new();
#     let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
#     let (mut entry, config) = archive.new_file("data.txt")
#         .compression_method(rawzip::CompressionMethod::DEFLATE).start().unwrap();
#     let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
#     let mut writer = config.wrap(encoder);
#     writer.write_all(body).unwrap();
#     let (encoder, descriptor) = writer.finish().unwrap();
#     encoder.finish().unwrap();
#     entry.finish(descriptor).unwrap();
#     archive.finish().unwrap();
#     output
# }
# let bytes = deflate_zip(b"trust, but verify");
use std::io::{self, Read};
use rawzip::{Crc32, ZipDataDescriptor, ZipVerification};

struct VerifiedReader<R> {
    inner: R,
    crc: Crc32,
    size: u64,
    central: ZipVerification,
    central_compressed_size: u64,
    descriptor: ZipDataDescriptor,
    verified: bool,
}

impl<R: Read> Read for VerifiedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let read = self.inner.read(buf)?;
        self.crc.update(&buf[..read]);
        self.size += read as u64;

        if !self.verified && (read == 0 || self.size >= self.central.uncompressed_size) {
            self.central.valid(ZipVerification {
                crc: self.descriptor.crc32(),
                uncompressed_size: self.descriptor.uncompressed_size(),
            }).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

            if self.descriptor.compressed_size() != self.central_compressed_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "descriptor compressed size differs from central directory",
                ));
            }

            self.central.valid(ZipVerification {
                crc: self.crc.checksum(),
                uncompressed_size: self.size,
            }).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.verified = true;
        }

        Ok(read)
    }
}

let archive = rawzip::ZipArchive::from_slice(&bytes)?;
let entry = archive.entries().next_entry()?.expect("one entry");
let central_compressed_size = entry.compressed_size_hint();
let local = archive.get_entry(entry.wayfinder())?;
let central = local.claim_verifier();
let descriptor = local.data_descriptor()?.expect("data descriptor");
let inflater = flate2::bufread::DeflateDecoder::new(local.data());
let verifier = VerifiedReader {
    inner: inflater,
    crc: Crc32::new(),
    size: 0,
    central,
    central_compressed_size,
    descriptor,
    verified: false,
};

let mut output = Vec::new();
io::copy(&mut verifier.take(central.uncompressed_size + 1), &mut output)?;
assert_eq!(output, b"trust, but verify");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Next: [Encryption](crate::guide::encryption)

[crc-tests]: https://github.com/nickbabcock/rawzip/blob/master/tests/it/crc_tests.rs
[extract-example]: https://github.com/nickbabcock/rawzip/blob/master/examples/extract.rs
[security]: https://github.com/nickbabcock/rawzip#security
