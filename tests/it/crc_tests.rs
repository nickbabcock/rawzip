//! "Bring your own CRC" verification.
//!
//! rawzip exposes four ways for a caller to supply their own CRC
//! implementation instead of rawzip's built-in one. They form a 2x2 matrix:
//! two entry types (slice- vs reader-backed) times two styles (imperative
//! `claim_verifier().valid(..)` vs the wrapping `verifying_reader(..)`). Each
//! cell gets a focused test below, all driven from the same [`sample_zip`]
//! fixture so the setup ceremony doesn't drown out the part that matters.

use rawzip::{ErrorKind, ZipArchive, ZipVerification, RECOMMENDED_BUFFER_SIZE};
use std::io::{Cursor, Read, Write};

const SAMPLE_DATA: &[u8] = b"Bring your own CRC: hardware intrinsics welcome!";

/// Builds an in-memory Zip with a single deflated entry containing
/// [`SAMPLE_DATA`]. The writer always emits a data descriptor, so the
/// authoritative CRC lives *after* the body and the lazy-descriptor path is
/// exercised.
fn sample_zip() -> Vec<u8> {
    let mut output = Vec::new();
    let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
    let (mut entry, config) = archive
        .new_file("file.txt")
        .compression_method(rawzip::CompressionMethod::Deflate)
        .start()
        .unwrap();
    let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
    let mut writer = config.wrap(encoder);
    std::io::copy(&mut &SAMPLE_DATA[..], &mut writer).unwrap();
    let (_, descriptor) = writer.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();
    output
}

/// Locates the (single) entry of a reader-backed archive built from `bytes`.
fn reader_entry(bytes: Vec<u8>, buf: &mut [u8]) -> rawzip::ZipArchive<Vec<u8>> {
    let end = bytes.len() as u64;
    rawzip::ZipLocator::new()
        .locate_in_reader(bytes, buf, end)
        .map_err(|(_, e)| e)
        .unwrap()
}

/// Slice + imperative: own the CRC loop, then call `claim_verifier().valid(..)`.
#[test]
fn slice_imperative() {
    let bytes = sample_zip();
    let archive = ZipArchive::from_slice(&bytes).unwrap();
    let mut entries = archive.entries();
    let header = entries.next_entry().unwrap().unwrap();
    let entry = archive.get_entry(header.wayfinder()).unwrap();

    // Compute the checksum with a "foreign" CRC (flate2's CrcReader).
    let decompressor = flate2::read::DeflateDecoder::new(entry.data());
    let mut reader = flate2::CrcReader::new(decompressor);
    std::io::copy(&mut reader, &mut std::io::sink()).unwrap();

    let actual = ZipVerification {
        crc: reader.crc().sum(),
        uncompressed_size: reader.get_ref().total_out(),
    };

    let verification = entry.claim_verifier();
    assert_ne!(actual.crc(), 0, "CRC should not be zero");
    assert_eq!(verification.expected().unwrap().crc(), actual.crc());
    verification.valid(actual).unwrap();
}

/// Slice + wrapping: let rawzip drive the CRC via `verifying_reader`.
#[test]
fn slice_wrapping() {
    let bytes = sample_zip();
    let archive = ZipArchive::from_slice(&bytes).unwrap();
    let mut entries = archive.entries();
    let header = entries.next_entry().unwrap().unwrap();
    let entry = archive.get_entry(header.wayfinder()).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.data());
    let mut verifier = entry.verifying_reader(decompressor);
    let mut actual = Vec::new();
    std::io::copy(&mut verifier, &mut actual).unwrap();
    assert_eq!(actual, SAMPLE_DATA);
}

/// Reader + imperative: own the CRC loop against a reader-backed archive.
#[test]
fn reader_imperative() {
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = reader_entry(sample_zip(), &mut buf);
    let wayfinder = {
        let mut entries = archive.entries(&mut buf);
        entries.next_entry().unwrap().unwrap().wayfinder()
    };
    let entry = archive.get_entry(wayfinder).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.reader());
    let mut reader = flate2::CrcReader::new(decompressor);
    std::io::copy(&mut reader, &mut std::io::sink()).unwrap();

    let actual = ZipVerification {
        crc: reader.crc().sum(),
        uncompressed_size: reader.get_ref().total_out(),
    };

    let verification = entry.claim_verifier();
    assert_ne!(actual.crc(), 0, "CRC should not be zero");
    assert_eq!(verification.expected().unwrap().crc(), actual.crc());
    verification.valid(actual).unwrap();
}

/// Reader + wrapping: let rawzip drive the CRC via `verifying_reader`.
#[test]
fn reader_wrapping() {
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = reader_entry(sample_zip(), &mut buf);
    let wayfinder = {
        let mut entries = archive.entries(&mut buf);
        entries.next_entry().unwrap().unwrap().wayfinder()
    };
    let entry = archive.get_entry(wayfinder).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.reader());
    let mut verifier = entry.verifying_reader(decompressor);
    let mut actual = Vec::new();
    std::io::copy(&mut verifier, &mut actual).unwrap();
    assert_eq!(actual, SAMPLE_DATA);
}

/// Slice + `finish()`: read exactly the known number of bytes (never hitting
/// EOF). Verification auto-triggers on the read that reaches the declared
/// size — lazily parsing the descriptor — and `finish()` then short-circuits,
/// handing back the inner reader.
#[test]
fn slice_finish_success() {
    let bytes = sample_zip();
    let archive = ZipArchive::from_slice(&bytes).unwrap();
    let mut entries = archive.entries();
    let header = entries.next_entry().unwrap().unwrap();
    let entry = archive.get_entry(header.wayfinder()).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.data());
    let mut verifier = entry.verifying_reader(decompressor);

    // Read exactly the known length; read_exact stops once the buffer is full
    // (never returning Ok(0)), and verification auto-triggers at the size
    // threshold.
    let mut out = vec![0u8; SAMPLE_DATA.len()];
    verifier.read_exact(&mut out).unwrap();
    assert_eq!(out, SAMPLE_DATA);

    // finish() short-circuits (already verified) and hands back the inner reader.
    verifier.finish().unwrap();
}

/// Slice + `finish()` under-read: stopping short of the known length must
/// surface as an `InvalidSize` error.
#[test]
fn slice_finish_under_read() {
    let bytes = sample_zip();
    let archive = ZipArchive::from_slice(&bytes).unwrap();
    let mut entries = archive.entries();
    let header = entries.next_entry().unwrap().unwrap();
    let entry = archive.get_entry(header.wayfinder()).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.data());
    let mut verifier = entry.verifying_reader(decompressor);

    let mut out = vec![0u8; SAMPLE_DATA.len() - 1];
    verifier.read_exact(&mut out).unwrap();

    let err = verifier.finish().unwrap_err();
    match err.kind() {
        ErrorKind::InvalidSize { expected, actual } => {
            assert_eq!(*expected, SAMPLE_DATA.len() as u64);
            assert_eq!(*actual, (SAMPLE_DATA.len() - 1) as u64);
        }
        other => panic!("expected InvalidSize error, got {:?}", other),
    }
}

/// Reader + `finish()`: same as `slice_finish_success` but against a
/// reader-backed archive, whose descriptor CRC is read lazily (positioned
/// read) when verification auto-triggers at the size threshold.
#[test]
fn reader_finish_success() {
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = reader_entry(sample_zip(), &mut buf);
    let wayfinder = {
        let mut entries = archive.entries(&mut buf);
        entries.next_entry().unwrap().unwrap().wayfinder()
    };
    let entry = archive.get_entry(wayfinder).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.reader());
    let mut verifier = entry.verifying_reader(decompressor);

    let mut out = vec![0u8; SAMPLE_DATA.len()];
    verifier.read_exact(&mut out).unwrap();
    assert_eq!(out, SAMPLE_DATA);

    verifier.finish().unwrap();
}

/// Reader + `finish()` under-read: stopping short must surface as `InvalidSize`.
#[test]
fn reader_finish_under_read() {
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = reader_entry(sample_zip(), &mut buf);
    let wayfinder = {
        let mut entries = archive.entries(&mut buf);
        entries.next_entry().unwrap().unwrap().wayfinder()
    };
    let entry = archive.get_entry(wayfinder).unwrap();

    let decompressor = flate2::read::DeflateDecoder::new(entry.reader());
    let mut verifier = entry.verifying_reader(decompressor);

    let mut out = vec![0u8; SAMPLE_DATA.len() - 1];
    verifier.read_exact(&mut out).unwrap();

    let err = verifier.finish().unwrap_err();
    match err.kind() {
        ErrorKind::InvalidSize { expected, actual } => {
            assert_eq!(*expected, SAMPLE_DATA.len() as u64);
            assert_eq!(*actual, (SAMPLE_DATA.len() - 1) as u64);
        }
        other => panic!("expected InvalidSize error, got {:?}", other),
    }
}

/// Reading exactly the known length must auto-verify at the size threshold:
/// a corrupted CRC surfaces from `read_exact` itself, with no EOF read and no
/// `finish()` call.
#[test]
fn read_exact_auto_verifies_at_known_length() {
    let mut data = std::fs::read("assets/crc32-not-streamed.zip").unwrap();

    let archive = ZipArchive::from_slice(data.as_slice()).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    assert!(!entry.has_data_descriptor());
    let size = entry.uncompressed_size_hint() as usize;

    // Mutate the central directory CRC to be incorrect
    let crc_offset = entry.central_directory_offset() as usize + 16;
    let original_crc = u32::from_le_bytes(data[crc_offset..crc_offset + 4].try_into().unwrap());
    let corrupted_crc = original_crc ^ 0xffff_ffff;
    data[crc_offset..crc_offset + 4].copy_from_slice(&corrupted_crc.to_le_bytes());

    // Slice verifier: the read that reaches the declared size must fail.
    let archive = ZipArchive::from_slice(data.as_slice()).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let mut verifier = ent.verifying_reader(ent.data());
    let mut out = vec![0u8; size];
    let err = verifier.read_exact(&mut out).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let source = err.into_inner().unwrap();
    let zip_error = source.downcast::<rawzip::Error>().unwrap();
    assert!(matches!(
        zip_error.kind(),
        ErrorKind::InvalidChecksum { .. }
    ));

    // Reader verifier: same expectation.
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_seekable(Cursor::new(data), &mut buffer).unwrap();
    let mut entries = archive.entries(&mut buffer);
    let entry = entries.next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let mut verifier = ent.verifying_reader(ent.reader());
    let mut out = vec![0u8; size];
    let err = verifier.read_exact(&mut out).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let source = err.into_inner().unwrap();
    let zip_error = source.downcast::<rawzip::Error>().unwrap();
    assert!(matches!(
        zip_error.kind(),
        ErrorKind::InvalidChecksum { .. }
    ));
}

/// A user-defined verifying reader composed from the `claim_verifier()`
/// building block, for when a self-verifying `Read` must be handed to code
/// you don't control (e.g. a tar parser). When you own the read loop, prefer
/// `verifying_reader` or the capped imperative pattern from the
/// `ZipEntry::claim_verifier` docs instead.
///
/// This mirrors the policy of `ZipVerifier::read`, the canonical
/// implementation: empty-buffer guard, fail-fast size bound, and verification
/// once the declared size is reached or EOF is hit, whichever comes first.
/// Policy changes there should be reflected here.
struct CustomZipVerifier<R> {
    reader: flate2::CrcReader<rawzip::ZipReader<R>>,
    size: u64,
    verified: bool,
}

impl<R> Read for CustomZipVerifier<R>
where
    R: rawzip::ReaderAt,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Guard against an empty buffer being misinterpreted as EOF.
        if buf.is_empty() {
            return Ok(0);
        }

        let read = self.reader.read(buf)?;
        self.size += read as u64;

        // Constructing the handle is a cheap field copy (no I/O). It borrows
        // the `ZipReader` owned by this struct, so it is constructed
        // transiently rather than stored.
        let verifier = self.reader.get_ref().claim_verifier();

        // Fail fast on an oversized stream
        if self.size > verifier.uncompressed_size_hint() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                rawzip::Error::from(rawzip::ErrorKind::InvalidSize {
                    expected: verifier.uncompressed_size_hint(),
                    actual: self.size,
                }),
            ));
        }

        // Verify once the declared size is reached or at EOF, whichever
        // comes first.
        if (read == 0 || self.size >= verifier.uncompressed_size_hint()) && !self.verified {
            let actual = ZipVerification {
                crc: self.reader.crc().sum(),
                uncompressed_size: self.size,
            };
            verifier
                .valid(actual)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            self.verified = true;
        }

        Ok(read)
    }
}

#[test]
fn custom_verifying_reader() {
    // Build a Stored entry (compressed == uncompressed); the writer always emits
    // a data descriptor, so the authoritative CRC lives *after* the body. The
    // custom reader CRCs the raw bytes directly, which only matches the stored
    // value because the entry is uncompressed.
    let data = b"deferred descriptor verification body bytes";
    let mut output = Cursor::new(Vec::new());
    let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
    let (mut entry, config) = archive.new_file("deferred.bin").start().unwrap();
    let mut writer = config.wrap(&mut entry);
    writer.write_all(data).unwrap();
    let (_, descriptor) = writer.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();
    let bytes = output.into_inner();

    // Use a reader-backed archive so the custom wrapper drives the `ZipReader`
    // verification path (whose CRC comes from the data descriptor).
    let mut buf = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_seekable(Cursor::new(bytes), &mut buf).unwrap();
    let wayfinder = {
        let mut entries = archive.entries(&mut buf);
        let header = entries.next_entry().unwrap().unwrap();
        assert!(header.has_data_descriptor());
        header.wayfinder()
    };
    let entry = archive.get_entry(wayfinder).unwrap();

    let mut verifier = CustomZipVerifier {
        reader: flate2::CrcReader::new(entry.reader()),
        size: 0,
        verified: false,
    };

    // An empty buffer is not EOF and must not trigger (premature) verification.
    assert_eq!(verifier.read(&mut []).unwrap(), 0);
    assert!(!verifier.verified);

    let n = std::io::copy(&mut verifier, &mut std::io::sink()).unwrap();
    assert_eq!(n, data.len() as u64);
    assert_eq!(verifier.size, data.len() as u64);
    assert!(verifier.verified);

    // A redundant read after EOF stays at EOF without re-verifying.
    assert_eq!(verifier.read(&mut [0u8; 8]).unwrap(), 0);
}

#[test]
fn catch_incorrect_crc_without_data_descriptor() {
    let mut data = std::fs::read("assets/crc32-not-streamed.zip").unwrap();

    let archive = ZipArchive::from_slice(data.as_slice()).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    assert!(!entry.has_data_descriptor());

    // Mutate the central directory CRC to be incorrect
    let crc_offset = entry.central_directory_offset() as usize + 16;
    let original_crc = u32::from_le_bytes(data[crc_offset..crc_offset + 4].try_into().unwrap());
    let corrupted_crc = original_crc ^ 0xffff_ffff;
    data[crc_offset..crc_offset + 4].copy_from_slice(&corrupted_crc.to_le_bytes());

    // Ensure that the slice verifier rejects the bad CRC
    let archive = ZipArchive::from_slice(data.as_slice()).unwrap();
    let mut entries = archive.entries();
    let entry = entries.next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let mut verifier = ent.verifying_reader(ent.data());
    let slice_result = std::io::copy(&mut verifier, &mut std::io::sink());

    let err = slice_result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let source = err.into_inner().unwrap();
    let zip_error = source.downcast::<rawzip::Error>().unwrap();
    match zip_error.kind() {
        ErrorKind::InvalidChecksum { expected, actual } => {
            assert_eq!(*expected, corrupted_crc);
            assert_eq!(*actual, original_crc);
        }
        other => panic!("expected InvalidChecksum error, got {:?}", other),
    }

    // Ensure that the reader verifier rejects the bad CRC
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_seekable(Cursor::new(data), &mut buffer).unwrap();
    let mut entries = archive.entries(&mut buffer);
    let entry = entries.next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let mut verifier = ent.verifying_reader(ent.reader());
    let reader_result = std::io::copy(&mut verifier, &mut std::io::sink());

    let err = reader_result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let source = err.into_inner().unwrap();
    let zip_error = source.downcast::<rawzip::Error>().unwrap();
    match zip_error.kind() {
        ErrorKind::InvalidChecksum { expected, actual } => {
            assert_eq!(*expected, corrupted_crc);
            assert_eq!(*actual, original_crc);
        }
        other => panic!("expected InvalidChecksum error, got {:?}", other),
    }
}
