[Benchmarks](https://github.com/nickbabcock/rawzip#benchmarks) attest that rawzip is fast.

rawzip is at least one to two orders of magnitude faster than other zip libraries. A zip archive's structural parsing should melt away in the face of the actual heavy lifting: decompression. But in the wake of other zip libraries materializing the central directory with an avalanche of allocations on files with many entries, rawzip was born.

Since rawzip is not tied to any compression library, users can opt into specialized implementations like [libdeflater](https://docs.rs/libdeflater/latest/libdeflater/) which has unbeatable performance but requires all the input up front and output pre-allocated.

Outside of decompression, rawzip has tricks up its sleeve to turn performance up to 11 with custom CRC implementations and parallel processing.

# Custom CRC Reading

rawzip's built-in CRC implementation is no slouch, reaching 5 GB/s and claiming to be the fastest CRC implementation on Wasm. However, because rawzip has zero dependencies and no unsafe code, it is not privy to certain hardware intrinsics and will lose to libraries such as `crc32fast` on those platforms.

The great news is that swapping CRC implementations is relatively painless in rawzip. We'll drop `verifying_reader` for a plain `reader`
and still leverage the built-in entry integrity check with
[`claim_verifier`][crate::ZipReader::claim_verifier].

The examples use [`flate2::CrcReader`] which internally uses `crc32fast`.

```rust
# use std::io::{Read, Write};
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
# let bytes = deflate_zip(b"hash me while streaming");
# let archive = rawzip::ZipArchive::from_slice(bytes)?;
# let wayfinder = archive.entries().next_entry()?.expect("one entry").wayfinder();
# let archive = archive.into_reader_archive();
let local = archive.get_entry(wayfinder)?;
let zip_reader = local.reader();
let expected = zip_reader.claim_verifier();
let inflater = flate2::read::DeflateDecoder::new(zip_reader);

// n + 1 detects oversized output while bounding work on invalid input.
let mut reader = flate2::CrcReader::new(inflater).take(expected.uncompressed_size + 1);
let mut plaintext = Vec::new();
let uncompressed_size = std::io::copy(&mut reader, &mut plaintext)?;

expected.valid(rawzip::ZipVerification {
    crc: reader.into_inner().crc().sum(),
    uncompressed_size,
})?;
# assert_eq!(plaintext, b"hash me while streaming");
# Ok::<(), Box<dyn std::error::Error>>(())
```


See [validation](crate::guide::validation) for an example driving CRC with a custom `Read` implementation.

# Custom CRC Writing

Stream input through the same custom reader and compressor, then supply its result with
[`DataDescriptorOutput::new`][crate::DataDescriptorOutput::new]. The entry
writer measures the compressed size itself:

```rust
use std::io::{Read, Write};
use flate2::{Compression, CrcReader, write::DeflateEncoder};
use rawzip::{CompressionMethod, DataDescriptorOutput, ZipArchiveWriter};

let payload = b"streamed custom integrity";
let mut output = Vec::new();
let mut archive = ZipArchiveWriter::new(&mut output);
let (mut entry, _config) = archive.new_file("data.bin")
    .compression_method(CompressionMethod::DEFLATE).start()?;

let mut input = CrcReader::new(payload.as_slice());
let uncompressed_size = {
    let mut compressor = DeflateEncoder::new(&mut entry, Compression::default());
    let n = payload.len() as u64;
    let size = std::io::copy(&mut input.by_ref().take(n + 1), &mut compressor)?;
    compressor.finish()?;
    size
};
entry.finish(DataDescriptorOutput::new(
    input.crc().sum(),
    uncompressed_size,
))?;
archive.finish()?;

let archive = rawzip::ZipArchive::from_slice(&output)?;
let entry = archive.entries().next_entry()?.expect("one entry");
assert_eq!(entry.crc32(), input.crc().sum());
assert_eq!(entry.uncompressed_size_hint(), payload.len() as u64);
# Ok::<(), Box<dyn std::error::Error>>(())
```

# Reading in Parallel

Decompression is CPU-bound, so we should take advantage of multicore systems to read data in parallel. This is done by sending the wayfinder yielded from a central directory entry to another thread.

```rust
# use std::io::{Read, Write};
# fn two_entry_zip() -> Vec<u8> {
#     let mut output = Vec::new();
#     let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
#     for (name, body) in [("a.txt", &b"first entry"[..]), ("b.txt", &b"second entry"[..])] {
#         let (mut entry, config) = archive
#             .new_file(name)
#             .compression_method(rawzip::CompressionMethod::DEFLATE)
#             .start().unwrap();
#         let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
#         let mut writer = config.wrap(encoder);
#         writer.write_all(body).unwrap();
#         let (encoder, descriptor) = writer.finish().unwrap();
#         encoder.finish().unwrap();
#         entry.finish(descriptor).unwrap();
#     }
#     archive.finish().unwrap();
#     output
# }
# let bytes = two_entry_zip();
let archive = rawzip::ZipArchive::from_slice(bytes.as_slice())?.into_reader_archive();

let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
let mut entries = archive.entries(&mut buffer);

let results = std::thread::scope(|scope| {
    let mut handles = Vec::new();
    while let Some(entry) = entries.next_entry().unwrap() {
        let wayfinder = entry.wayfinder(); // Copy + Send
        let archive = &archive;            // shared immutably across workers
        handles.push(scope.spawn(move || {
            let entry = archive.get_entry(wayfinder).unwrap();
            let inflater = flate2::read::DeflateDecoder::new(entry.reader());
            let mut out = Vec::new();
            entry.verifying_reader(inflater).read_to_end(&mut out).unwrap();
            out
        }));
    }
    handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
});

assert_eq!(results, vec![b"first entry".to_vec(), b"second entry".to_vec()]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

File names are often required for further processing, but the lending iterator through the central directory doesn't provide ownership of that data. All is not lost: instead of cloning each file name, the names can be collected into an amortized string buffer and referenced by index.

# Writing in Parallel

Rawzip can "write" in parallel but it is a little nuanced as a single thread still needs to sequentially write the central directory.

Since [`ZipEntryWriter`][crate::ZipEntryWriter] accepts *already-compressed* bytes, you
can compress every entry on a worker thread and hand the finished buffers to one
archive writer via [`DataDescriptorOutput::new`][crate::DataDescriptorOutput::new]. We bypass `config.wrap`, which would otherwise hash the data itself.

```rust
use std::io::{Read, Write};
use rawzip::{CompressionMethod, DataDescriptorOutput};

let files: [(&str, &[u8]); 2] = [("a.txt", b"first payload"), ("b.txt", b"second payload")];

struct Compressed { name: &'static str, data: Vec<u8>, crc: u32, size: u64 }

// Fan the compression out. Each worker returns the compressed bytes plus the
// CRC and uncompressed size — everything the entry will need.
let jobs: Vec<Compressed> = std::thread::scope(|scope| {
    let handles: Vec<_> = files.iter().map(|pair| {
        let (name, body) = *pair;
        scope.spawn(move || {
            let mut data = Vec::new();
            let mut enc = flate2::write::DeflateEncoder::new(&mut data, flate2::Compression::default());
            enc.write_all(body).unwrap();
            enc.finish().unwrap();
            Compressed { name, data, crc: rawzip::crc32(body), size: body.len() as u64 }
        })
    }).collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
});

// One thread writes the archive, copying each precompressed buffer verbatim.
let mut output = Vec::new();
let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
for job in jobs {
    let (mut entry, _config) = archive
        .new_file(job.name)
        .compression_method(CompressionMethod::DEFLATE)
        .start()?;
    entry.write_all(&job.data)?; // already deflate-compressed; no re-compression
    entry.finish(DataDescriptorOutput::new(job.crc, job.size))?;
}
archive.finish()?;

// Reads back like any other archive; the verifying reader confirms the off-thread CRC.
let archive = rawzip::ZipArchive::from_slice(&output)?;
let first = archive.entries().next_entry()?.expect("an entry");
let local = archive.get_entry(first.wayfinder())?;
let mut out = Vec::new();
local.verifying_reader(flate2::bufread::DeflateDecoder::new(local.data()))
    .read_to_end(&mut out)?;
assert_eq!(out, b"first payload");
# Ok::<(), Box<dyn std::error::Error>>(())
```


Next: [Validation](crate::guide::validation)

[`flate2::CrcReader`]: https://docs.rs/flate2/latest/flate2/struct.CrcReader.html
