## v0.4.0 - August 22nd, 2025

### Breaking Changes

#### Removed `base_offset()` method

The base offset method has been removed as it was documented as giving the zip archive start offset. The documentation was misleading, as the calculation represents the difference between expected and actual EOCD location, which, under some circumstances could return the starting offset of the zip archive.

To find the true start of the zip archive, iterate through central directory entries and find the minimum `local_header_offset()`. In the vast majority of cases the first entry should signal the start of the zip file.

```rust
// Before (v0.3.x)
let start = archive.base_offset();

// After (v0.4.0)
let start = archive
    .entries()
    .map(|x| x.expect("<error handling>").local_header_offset())
    .min()
    .unwrap_or(archive.directory_offset())
```

#### `ZipLocator::locate_in_reader`

To allow starting a zip search from an arbitrary offset in a stream, the `ZipLocator::locate_in_reader` has been updated to take a starting offset instead of requiring a `Seek` implementation to calculate the reader length.

```rust
// Before (v0.3.x)
let locator = ZipLocator::new();
let archive = locator.locate_in_reader(reader, &mut buffer)
    .map_err(|(_, e)| e)?;

// After (v0.4.0)
let end_offset = reader.seek(std::io::SeekFrom::End(0))?;
let archive = locator.locate_in_reader(reader, &mut buffer, end_offset)
    .map_err(|(_, e)| e)?;
```

#### `ZipFileBuilder::create` deprecation

`ZipFileBuilder::create()` has been deprecated in favor of `ZipFileBuilder::start()`, which allows propagation of the newly introduced crc32 customization option into the uncompressed data writer, `ZipDataWriter`.

```rust
// Before (v0.3.x)
let file = archive.new_file("file.txt").create()?;
let mut encoder = flate2::write::DeflateEncoder::new(&mut file, flate2::Compression::default());
let mut writer = rawzip::ZipDataWriter::new(encoder);
std::io::copy(&mut &data[..], &mut writer)?;
let _ = file.finish(descriptor)?;

// After (v0.4.0)
let (mut entry, config) = archive.new_file("file.txt").start()?;
let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
let mut writer = config.wrap(encoder);
std::io::copy(&mut &data[..], &mut writer)?;
let _ = entry.finish(descriptor)?;
```

#### `ZipArchive::comment`

`ZipArchive::comment` returns a `RangeReader` instead of `ZipString` in order to achieve zero heap allocation parsing of zip files.

Zip slice archives are unaffected.

Both slice archives and reader archives will validate enough data is present for the comment to be read prior to returning a parsed archive.

```rust
// Before (v0.3.x)
assert_eq!(archive.comment().as_bytes(), b"This is a zipfile comment.");

// After (v0.4.0)
let mut comment_reader = archive.comment();
let comment_len = comment_reader.remaining() as usize;
let mut comment_buffer = vec![0u8; comment_len];
comment_reader.read_exact(&mut comment_buffer).unwrap();
assert_eq!(comment_buffer.as_slice(), b"This is a zipfile comment.");
```

#### Lifetimes

`ZipReader` and `ZipVerifier` no longer have an explicit lifetime parameter that is tied to the reference of the underlying Zip archive.

```rust
// Before (v0.3.x)
rawzip::ZipReader<'_, R>

// After (v0.4.0)
rawzip::ZipReader<&R>
```

### Features and fixes

- Added write API to customize crc32 calculation
- Added extra field API on local and central directory entries
- Added file name API for local file entries
- Added crc32 value on central directory entries
- Added central directory offset on central directory entries
- Added `ZipArchive::into_inner()` to recover underlying reader
- Added method to get current stream offset when creating a zip file
- Added ReaderAt implementations for smart pointers
- Added mechanisms to recover from a false EOCD signature by adding `eocd_offset` on errors and zip archives.
- Added inherent `as_bytes` method for zip file paths to allow easier extraction of underlying byte data
- Added end of central directory offset on zip archives for facilitating false EOCD recovery
- Fixed offset overflow issues in zip slice archives
- Performance optimizations
  - Amortized file name allocations on the write path
  - Added support for creating archive writers with initial capacity
  - Improved Windows file handling by avoiding a mutex

## v0.3.1 - July 13th, 2025

- Fix file name lifetime tied to the archive writer

## v0.3.0 - July 12th, 2025

### Breaking Changes

As an author of several libraries and applications that rely rawzip, breaking changes pain me. Especially as one mantra of rawzip is to be a dependable foundation where one expects long term stability and infrequent releases. I think with this release, the goal is close to being realized.

There are three breaking changes.

The following methods have been replaced:

```rust
ZipFileHeaderRecord::file_safe_path()
ZipFileHeaderRecord::file_raw_path()
```

and consolidated with a single entrypoint:

```rust
ZipFileHeaderRecord::file_path()
```

Getting the raw bytes of the file path becomes:

```rust
let raw = entry.file_path().as_ref();
```

And retrieving the UTF-8 string becomes:

```rust
let s = entry.file_path().try_normalize()?.as_ref();
let owned = String::from(entry.file_path().try_normalize()?);
```

Another breaking change is the writer API. `ZipEntryOptions` has been removed:

```rust
archive.new_dir("my_dir/")?;
archive.new_file(
    "my_dir/file.txt",
    rawzip::ZipEntryOptions::default().compression_method(CompressionMethod::Deflate)
)?;
```

And is replaced with a builder API.

```rust
archive.new_dir("my_dir/").create()?;
archive.new_file("file.txt")
    .compression_method(CompressionMethod::Deflate)
    .create()?;
```

The last breaking change is how the central directory iteration is terminated. Previously, iteration would terminate when an entry would fail to parse and the anticipated number of entries had been encountered. Now, iteration is terminated once the end of central directory marker is reached. 

## Additional changes

- Added zip64 write capabilities
- Read and write last modified timestamps and permissions on zip and zip64 files
- Added `ZipEntry::compressed_data_range()` methods for offsets of compressed data to guard against overlapping zip entries
- Improved zip writer's UTF-8 flag handling
- Improved performance of central directory iteration for slice and readers

## v0.2.0 - May 26th, 2025

- Expose `ErrorKind` as part of public API
- Impl Iterator for ZipSliceEntries
- Remove accidental types as part of public API
- Derive `Debug` for `MutexReader`
- Fix slice archive panic on truncated eocd entry
- Fix inconsistent behavior for truncated eocd64 between reader and slice archives
- Fix inconsistent behavior for truncated zip comments between reader and slice archives
- Fix zip archive reader reading zeros instead of EOF
- Fix zip archive reader comment detection

## v0.1.0 - March 1th, 2025

The v0.1.0 release signifies that I'm satisfied with overall APIs for reading and writing. There are still plenty of missing aspects that would be useful for a general purpose zip library reader (like timestamps, permissions, etc) as well as writer (zip64), but these can be incorporated onto the current foundations as time and use cases permits.

- Add `ZipSliceArchive::as_bytes` to get access to the underlying input byte stream
- Add `ZipSliceEntry::claim_verifier`
- Change `ZipLocator::locate_in_slice` to return input ownership when there is an error
- Change `ZipSliceArchive` to be generic over any type that implements `AsRef<&[u8]>`
- Rename `RawZipWriter` to `ZipDataWriter`
- Rename `ZipSliceArchive::into_owned` to `into_reader`

## v0.0.7 - February 18th, 2025

- Update `ZipSliceArchive` to pull compressed data size from central directory instead of local file header

## v0.0.6 - February 14th, 2025

- Add the ability to create Zip files

## v0.0.5 - February 11th, 2025

- Improved support for zips with arbitrary leading data
- Expose base offset of where the zip file begins proper
- Expose inner ReaderAt with `get_ref`

## v0.0.4 - February 8th, 2025

- Add exposure of file local header offset
- Add `Debug` and `Clone` implementations to most structs
- Add `ReaderError` to `ZipLocator` to return file ownership back to caller on failure

## v0.0.3 - February 6th, 2025

- Add an `into_owned` to transform a `ZipSliceArchive` into a `ZipArchive`
- Standardize on "verifying_reader" nomenclature

## v0.0.2 - February 1st, 2025

- Update zip verification API
- Update safe file path to remove drive letters
- ZipVerifier to automatically verify at end of stream
- Expose additional types

## v0.0.1 - January 30th, 2025

- Initial workable, pre-alpha release
