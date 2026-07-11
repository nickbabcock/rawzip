# Reading

The convenience wrappers:

- [`ZipArchive::from_file`][crate::ZipArchive::from_file]
- [`ZipArchive::from_slice`][crate::ZipArchive::from_slice]

They are built upon a [`ReaderAt`][crate::ReaderAt] trait which only requires `&self` and not `&mut self` to satisfy reads, which allows for easy parallel decompression as shown in the [Performance guide](crate::guide::performance). Custom [`ReaderAt`][crate::ReaderAt] implementations can instantiate their archive with:

- [`ZipLocator::locate_in_reader`][crate::ZipLocator::locate_in_reader]

When dealing with an implementation that is only `Read + Seek`:

- [`ZipArchive::from_seekable`][crate::ZipArchive::from_seekable] (Wrap the reads and seeks in a mutex)

rawzip's synchronous read interface may seem limiting in an asynchronous world (e.g., with io_uring), but parallel decompression can still maximize drive throughput. Even networked reads work well with synchronous I/O. For example, [crater][crater-reader] has a custom [`ReaderAt`][crate::ReaderAt] for byte-offset reads of the crates.io zip database dump directly from S3.

## Zip Archive Mental Model

- Open a [`ZipArchive`][crate::ZipArchive]. This locates and
  parses the end of the central directory.
- Iterate the central directory with
  [`ZipArchive::entries`][crate::ZipArchive::entries]. Each record exposes
  metadata such as its path, sizes, flags, and compression method without
  opening the file data.
- Keep the record's lightweight
  [`wayfinder`][crate::ZipFileHeaderRecord::wayfinder]. It contains the offsets
  needed to find that entry again and can be sent to another thread.
- Pass the wayfinder to
  [`ZipArchive::get_entry`][crate::ZipArchive::get_entry] to parse the local
  header and reach the compressed body.
- Stream the compressed body through
  [`ZipEntry::reader`][crate::ZipEntry::reader], then select a decompressor from
  the central record's
  [`compression_method`][crate::ZipFileHeaderRecord::compression_method].
- Wrap the decompressor with
  [`ZipEntry::verifying_reader`][crate::ZipEntry::verifying_reader] so the final
  CRC and uncompressed size are checked while the entry is consumed.

## Prefixed data (self-extracting archives)

A self-extracting archive is an executable stub with a zip archive appended to it.
The entry offsets in such a file are written relative to the zip archive, not the whole
file, so a naive parser reads garbage. rawzip validates the first central
directory entry and, when it does not line up, corrects the base offset for you —
so [`from_slice`][crate::ZipArchive::from_slice] just works even with junk in
front:

```rust
# use std::io::{Read, Write};
# fn one_file_zip(name: &str, body: &[u8]) -> Vec<u8> {
#     let mut output = Vec::new();
#     let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
#     let (mut entry, config) = archive
#         .new_file(name)
#         .compression_method(rawzip::CompressionMethod::STORE)
#         .start().unwrap();
#     let mut writer = config.wrap(&mut entry);
#     writer.write_all(body).unwrap();
#     let (_, descriptor) = writer.finish().unwrap();
#     entry.finish(descriptor).unwrap();
#     archive.finish().unwrap();
#     output
# }
// Pretend this is a 2 KB extractor stub sitting in front of the real archive.
let mut blob = vec![0xEeu8; 2048];
blob.extend_from_slice(&one_file_zip("payload.txt", b"inside the stub"));

// Look ma, we can still extract entries!
let archive = rawzip::ZipArchive::from_slice(&blob)?;
let entry = archive.entries().next_entry()?.expect("one entry");
let local = archive.get_entry(entry.wayfinder())?;
assert_eq!(local.data(), b"inside the stub");
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Concatenated archives

Because the locator can start its backward search from any offset, you can walk a
run of concatenated archives from the last to the first. Locate the tail archive,
find where it begins (the smallest
[`local_header_offset`][crate::ZipFileHeaderRecord::local_header_offset] among
its entries), then locate again in the bytes *before* that point:

```rust
# use std::io::Write;
# fn one_file_zip(name: &str, body: &[u8]) -> Vec<u8> {
#     let mut output = Vec::new();
#     let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
#     let (mut entry, config) = archive
#         .new_file(name)
#         .compression_method(rawzip::CompressionMethod::STORE)
#         .start().unwrap();
#     let mut writer = config.wrap(&mut entry);
#     writer.write_all(body).unwrap();
#     let (_, descriptor) = writer.finish().unwrap();
#     entry.finish(descriptor).unwrap();
#     archive.finish().unwrap();
#     output
# }
use rawzip::ZipLocator;

// Two archives, back to back.
let mut blob = one_file_zip("first.txt", b"from archive one");
blob.extend_from_slice(&one_file_zip("second.txt", b"from archive two"));

// A plain locate finds the *last* archive.
let last = ZipLocator::new().locate_in_slice(&blob).map_err(|(_, e)| e)?;
let entry = last.entries().next_entry()?.expect("one entry");
assert_eq!(entry.file_path().try_normalize()?.as_ref(), "second.txt");

// Where does the last archive begin?
let zip_start = last
    .entries()
    .into_iter()
    .filter_map(Result::ok)
    .map(|e| e.local_header_offset())
    .min()
    .unwrap_or(0);

// Re-scan everything before it to recover the previous archive.
let prev = ZipLocator::new().locate_in_slice(&blob[..zip_start as usize]).map_err(|(_, e)| e)?;
let entry = prev.entries().next_entry()?.expect("one entry");
assert_eq!(entry.file_path().try_normalize()?.as_ref(), "first.txt");
# Ok::<(), Box<dyn std::error::Error>>(())
```

## False EOCD signatures

A zip comment or trailing data can contain the EOCD byte sequence by chance. If
the locator tries that false signature first, use the offset on the resulting
[`Error`][crate::Error] to resume searching before it:

```rust
use rawzip::ZipLocator;

// Simulate trailing data that happens to contain an EOCD signature.
let mut data = std::fs::read("assets/test.zip")?;
data.extend_from_slice(b"trailing data: ");
data.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
data.extend_from_slice(b" not an EOCD");

let locator = ZipLocator::new();
let mut end = data.len();
let mut false_signatures = 0;

let archive = loop {
    match locator.locate_in_slice(&data[..end]) {
        Ok(archive) => break archive,
        Err((_, error)) => {
            // eocd_offset points at the false signature. Exclude it from the
            // next search so the locator continues farther back.
            let Some(false_eocd) = error.eocd_offset() else {
                return Err(error.into());
            };

            // Define your own policy on false signatures
            false_signatures += 1;
            if false_signatures == 1 {
                println!("suspicious zip ...")
            }

            end = false_eocd as usize;
        }
    }
};

assert_eq!(archive.entries_hint(), 2);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The same
[`eocd_offset`][crate::ZipArchive::eocd_offset],
[`directory_offset`][crate::ZipSliceArchive::directory_offset], and
[`end_offset`][crate::ZipSliceArchive::end_offset] accessors on a successfully
opened archive give you the anchor points to reason about (or re-scan) messy
inputs.

# Out of Scope: Sequential Archive Processing

While individual Zip archive entries can be decompressed in a streaming fashion, opening a Zip archive requires a seekable source because the central directory, typically located at the end of the file, is the source of truth. Although it is technically possible to extract entries from a stream, I don't recommend it, even for networked applications. Discrepancies between the central directory file headers and local file headers can otherwise rear their ugly heads. It is better to accept this cost and write to the file system temporarily as necessary, or follow crater's approach and use byte-offset reads from S3.

Next: [Performance](crate::guide::performance)

[crater-reader]: https://github.com/rust-lang/crater/blob/0b18731ff70bf874ec76096447a899497be730bd/src/crates/sources/registry.rs#L21
