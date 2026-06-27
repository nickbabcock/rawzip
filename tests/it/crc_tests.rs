use flate2::read::DeflateDecoder;
use rawzip::{
    CompressionMethod, Error, ErrorKind, RECOMMENDED_BUFFER_SIZE, ReaderAt, ZipArchive,
    ZipArchiveWriter, ZipReader, ZipVerification,
};
use std::io::{self, Cursor, Read, Write};

const CONTENT: &[u8] =
    b"the quick brown fox jumps over the lazy dog. the quick brown fox jumps over the lazy dog.";

/// Builds an in-memory deflate-compressed zip. The writer always emits a data
/// descriptor, so this exercises the descriptor-present path.
fn build_deflate_zip(content: &[u8]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut output);
    let (mut entry, config) = archive
        .new_file("file.txt")
        .compression_method(CompressionMethod::Deflate)
        .start()
        .unwrap();
    let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
    let mut writer = config.wrap(encoder);
    writer.write_all(content).unwrap();
    let (encoder, descriptor) = writer.finish().unwrap();
    encoder.finish().unwrap();
    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();
    output.into_inner()
}

/// Offset of the 4-byte CRC field within an entry's central directory record.
fn central_crc_offset(data: &[u8]) -> usize {
    let archive = ZipArchive::from_slice(data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    entry.central_directory_offset() as usize + 16
}

/// Returns the wayfinder for the first entry of a reader-based archive.
fn first_wayfinder<R: ReaderAt>(archive: &ZipArchive<R>) -> rawzip::ZipArchiveEntryWayfinder {
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let mut entries = archive.entries(&mut buffer);
    entries.next_entry().unwrap().unwrap().wayfinder()
}

fn corrupt_central_crc(data: &mut [u8]) -> (u32, u32) {
    let offset = central_crc_offset(data);
    let original = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let corrupted = original ^ 0xffff_ffff;
    data[offset..offset + 4].copy_from_slice(&corrupted.to_le_bytes());
    (original, corrupted)
}

fn corrupt_descriptor_crc(data: &mut [u8]) -> (u32, u32) {
    let archive = ZipArchive::from_slice(&*data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();
    assert_eq!(
        ent.data_descriptor().unwrap().unwrap().crc32(),
        entry.crc32()
    );

    let (_, compressed_data_end) = ent.compressed_data_range();
    let descriptor_crc_offset = compressed_data_end as usize + 4;
    let original = u32::from_le_bytes(
        data[descriptor_crc_offset..descriptor_crc_offset + 4]
            .try_into()
            .unwrap(),
    );
    let corrupted = original ^ 0xffff_ffff;
    data[descriptor_crc_offset..descriptor_crc_offset + 4]
        .copy_from_slice(&corrupted.to_le_bytes());
    (original, corrupted)
}

#[test]
fn slice_imperative_claim_verifier() {
    let data = build_deflate_zip(CONTENT);
    let archive = ZipArchive::from_slice(&data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let decoder = DeflateDecoder::new(ent.data());
    let mut crc_reader = flate2::CrcReader::new(decoder);
    let mut out = Vec::new();
    io::copy(&mut crc_reader, &mut out).unwrap();

    assert_eq!(out, CONTENT);
    ent.claim_verifier()
        .valid(ZipVerification {
            crc: crc_reader.crc().sum(),
            uncompressed_size: out.len() as u64,
        })
        .unwrap();
}

#[test]
fn slice_verifying_reader() {
    let data = build_deflate_zip(CONTENT);
    let archive = ZipArchive::from_slice(&data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let inflater = DeflateDecoder::new(ent.data());
    let mut verifier = ent.verifying_reader(inflater);
    let mut out = Vec::new();
    io::copy(&mut verifier, &mut out).unwrap();
    assert_eq!(out, CONTENT);
}

#[test]
fn reader_imperative_claim_verifier() {
    let data = build_deflate_zip(CONTENT);
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();

    let decoder = DeflateDecoder::new(ent.reader());
    let mut crc_reader = flate2::CrcReader::new(decoder);
    let mut out = Vec::new();
    io::copy(&mut crc_reader, &mut out).unwrap();
    assert_eq!(out, CONTENT);

    let actual = ZipVerification {
        crc: crc_reader.crc().sum(),
        uncompressed_size: out.len() as u64,
    };
    let zip_reader = crc_reader.into_inner().into_inner();
    zip_reader.claim_verifier().valid(actual).unwrap();
}

#[test]
fn reader_verifying_reader() {
    let data = build_deflate_zip(CONTENT);
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();

    let inflater = DeflateDecoder::new(ent.reader());
    let mut verifier = ent.verifying_reader(inflater);
    let mut out = Vec::new();
    io::copy(&mut verifier, &mut out).unwrap();
    assert_eq!(out, CONTENT);
}

#[test]
fn full_read_verifies_without_finish() {
    let mut data = build_deflate_zip(CONTENT);
    let (original, corrupted) = corrupt_central_crc(&mut data);

    let archive = ZipArchive::from_slice(&data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let mut verifier = ent.verifying_reader(DeflateDecoder::new(ent.data()));
    let mut buf = vec![0u8; CONTENT.len()];
    // read_exact of the full size triggers verification on the final read,
    // surfacing the corrupt central CRC without requiring a terminal method.
    let err = verifier.read_exact(&mut buf).unwrap_err();
    assert_invalid_checksum(err, corrupted, original);
}

#[test]
fn reader_verifying_reader_custom_crc32() {
    fn custom_crc32(buf: &[u8], initial: u32) -> u32 {
        let mut h = crc32fast::Hasher::new_with_initial(initial);
        h.update(buf);
        h.finalize()
    }

    let data = build_deflate_zip(CONTENT);
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();

    let inflater = DeflateDecoder::new(ent.reader());
    let mut verifier = ent.verifying_reader_crc32(inflater, custom_crc32);
    let mut out = Vec::new();
    io::copy(&mut verifier, &mut out).unwrap();
    assert_eq!(out, CONTENT);
}

struct CustomCrcVerifier<R> {
    reader: flate2::CrcReader<DeflateDecoder<ZipReader<R>>>,
    size: u64,
    verifications: usize,
}

impl<R: ReaderAt> CustomCrcVerifier<R> {
    fn new(reader: ZipReader<R>) -> Self {
        Self {
            reader: flate2::CrcReader::new(DeflateDecoder::new(reader)),
            size: 0,
            verifications: 0,
        }
    }
}

impl<R: ReaderAt> Read for CustomCrcVerifier<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let read = self.reader.read(buf)?;
        self.size += read as u64;

        let expected = self.reader.get_ref().get_ref().claim_verifier();
        if read == 0 || self.size >= expected.uncompressed_size {
            expected
                .valid(ZipVerification {
                    crc: self.reader.crc().sum(),
                    uncompressed_size: self.size,
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.verifications += 1;
        }

        Ok(read)
    }
}

#[test]
fn custom_crc_verifier_empty_buffer_and_eof_verification() {
    let data = build_deflate_zip(CONTENT);
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();
    let mut verifier = CustomCrcVerifier::new(ent.reader());

    // An empty target buffer must not be treated as EOF.
    assert_eq!(verifier.read(&mut []).unwrap(), 0);
    assert_eq!(verifier.verifications, 0);

    let mut out = Vec::new();
    io::copy(&mut verifier, &mut out).unwrap();
    assert_eq!(out, CONTENT);
    assert_eq!(verifier.verifications, 2);

    // Redundant reads past EOF stay at EOF and run the same verification path.
    assert_eq!(verifier.read(&mut [0u8; 16]).unwrap(), 0);
    assert_eq!(verifier.verifications, 3);
}

#[test]
fn corrupt_central_crc_rejected() {
    let mut data = build_deflate_zip(CONTENT);
    let (original, corrupted) = corrupt_central_crc(&mut data);
    let read_limit = CONTENT.len() as u64 + 1;

    // Slice verifier.
    let archive = ZipArchive::from_slice(&data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();
    let verifier = ent.verifying_reader(DeflateDecoder::new(ent.data()));
    let err = io::copy(&mut verifier.take(read_limit), &mut io::sink()).unwrap_err();
    assert_invalid_checksum(err, corrupted, original);

    // Reader verifier.
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();
    let verifier = ent.verifying_reader(DeflateDecoder::new(ent.reader()));
    let err = io::copy(&mut verifier.take(read_limit), &mut io::sink()).unwrap_err();
    assert_invalid_checksum(err, corrupted, original);
}

fn assert_invalid_checksum(err: io::Error, expected: u32, actual: u32) {
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let zip_error = err.into_inner().unwrap().downcast::<Error>().unwrap();
    match zip_error.kind() {
        ErrorKind::InvalidChecksum {
            expected: e,
            actual: a,
        } => {
            assert_eq!(*e, expected);
            assert_eq!(*a, actual);
        }
        other => panic!("expected InvalidChecksum, got {other:?}"),
    }
}

#[test]
fn data_descriptor_present_for_streamed_entry() {
    let data = build_deflate_zip(CONTENT);

    // Slice path.
    let archive = ZipArchive::from_slice(&data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    assert!(entry.flags().has_data_descriptor());
    let central_crc = entry.crc32();
    let central_compressed = entry.compressed_size_hint();
    let central_uncompressed = entry.uncompressed_size_hint();
    let ent = archive.get_entry(entry.wayfinder()).unwrap();
    let dd = ent.data_descriptor().unwrap().expect("descriptor present");
    assert_eq!(dd.crc32(), central_crc);
    assert_eq!(dd.compressed_size(), central_compressed);
    assert_eq!(dd.uncompressed_size(), central_uncompressed);

    // Reader path.
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();
    let reader = ent.reader();
    let dd = reader
        .data_descriptor()
        .unwrap()
        .expect("descriptor present");
    assert_eq!(dd.crc32(), central_crc);
    assert_eq!(dd.compressed_size(), central_compressed);
    assert_eq!(dd.uncompressed_size(), central_uncompressed);
}

#[test]
fn data_descriptor_absent_for_non_streamed_entry() {
    let data = std::fs::read("assets/crc32-not-streamed.zip").unwrap();

    // Slice path.
    let archive = ZipArchive::from_slice(&data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    assert!(!entry.flags().has_data_descriptor());
    let ent = archive.get_entry(entry.wayfinder()).unwrap();
    assert!(ent.data_descriptor().unwrap().is_none());

    // Reader path.
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();
    let reader = ent.reader();
    assert!(reader.data_descriptor().unwrap().is_none());
}

/// Mirrors libziparchive's data-descriptor consistency check: it cross-checks
/// all three descriptor fields (crc, compressed size, uncompressed size)
/// against the central directory:
///
/// ref: https://android.googlesource.com/platform/system/libziparchive/+/refs/tags/android-17.0.0_r1/zip_archive.cc#775
struct DescriptorVerifier<R> {
    reader: flate2::CrcReader<DeflateDecoder<ZipReader<R>>>,
    expected_compressed_size: u64,
    size: u64,
    verified: bool,
}

impl<R: ReaderAt> DescriptorVerifier<R> {
    fn new(reader: ZipReader<R>, expected_compressed_size: u64) -> Self {
        Self {
            reader: flate2::CrcReader::new(DeflateDecoder::new(reader)),
            expected_compressed_size,
            size: 0,
            verified: false,
        }
    }
}

impl<R: ReaderAt> Read for DescriptorVerifier<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let read = self.reader.read(buf)?;
        self.size += read as u64;

        let zr = self.reader.get_ref().get_ref();
        let expected = zr.claim_verifier();
        if !self.verified && (read == 0 || self.size >= expected.uncompressed_size) {
            if let Some(descriptor) = zr
                .data_descriptor()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            {
                // Cross-check the descriptor's crc and uncompressed size against
                // the central directory.
                expected
                    .valid(ZipVerification {
                        crc: descriptor.crc32(),
                        uncompressed_size: descriptor.uncompressed_size(),
                    })
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

                // and the compressed size, which `ZipVerification` omits.
                if descriptor.compressed_size() != self.expected_compressed_size {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "compressed size mismatch: descriptor {} vs central {}",
                            descriptor.compressed_size(),
                            self.expected_compressed_size
                        ),
                    ));
                }
            }

            expected
                .valid(ZipVerification {
                    crc: self.reader.crc().sum(),
                    uncompressed_size: self.size,
                })
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.verified = true;
        }

        Ok(read)
    }
}

#[test]
fn descriptor_cross_check_rejects_descriptor_divergence() {
    let mut data = build_deflate_zip(CONTENT);
    // Corrupt only the descriptor CRC; the central directory and data stay correct.
    let (original, corrupted) = corrupt_descriptor_crc(&mut data);
    let read_limit = CONTENT.len() as u64 + 1;

    // The default verifying_reader keys off the central directory and succeeds.
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();
    let verifier = ent.verifying_reader(DeflateDecoder::new(ent.reader()));
    io::copy(&mut verifier.take(read_limit), &mut io::sink()).unwrap();

    // The cross-checking verifier rejects the descriptor/central mismatch.
    let ent = archive.get_entry(wayfinder).unwrap();
    let mut verifier = DescriptorVerifier::new(ent.reader(), wayfinder.compressed_size_hint());
    let err = io::copy(&mut verifier, &mut io::sink()).unwrap_err();
    assert_invalid_checksum(err, original, corrupted);
}

/// Corrupts only the compressed size field within an entry's data descriptor.
fn corrupt_descriptor_compressed_size(data: &mut [u8]) -> (u64, u64) {
    let archive = ZipArchive::from_slice(&*data).unwrap();
    let entry = archive.entries().next_entry().unwrap().unwrap();
    assert!(entry.flags().has_data_descriptor());
    let ent = archive.get_entry(entry.wayfinder()).unwrap();

    let (_, compressed_data_end) = ent.compressed_data_range();
    let offset = compressed_data_end as usize + 8;
    let original = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let corrupted = original ^ 0xffff_ffff;
    data[offset..offset + 4].copy_from_slice(&corrupted.to_le_bytes());
    (u64::from(original), u64::from(corrupted))
}

#[test]
fn descriptor_cross_check_rejects_size_divergence() {
    let mut data = build_deflate_zip(CONTENT);
    // Corrupt only the descriptor's compressed size; the central directory and
    // data stay correct.
    let (original, corrupted) = corrupt_descriptor_compressed_size(&mut data);
    let read_limit = CONTENT.len() as u64 + 1;

    // The default verifying_reader keys off the central directory (and never
    // consults the descriptor sizes), so it succeeds despite the corruption.
    let archive = ZipArchive::from_slice(&data).unwrap().into_reader();
    let wayfinder = first_wayfinder(&archive);
    let ent = archive.get_entry(wayfinder).unwrap();
    let verifier = ent.verifying_reader(DeflateDecoder::new(ent.reader()));
    io::copy(&mut verifier.take(read_limit), &mut io::sink()).unwrap();

    // The descriptor still decodes both sizes; only the compressed size is wrong.
    let ent = archive.get_entry(wayfinder).unwrap();
    let descriptor = ent
        .reader()
        .data_descriptor()
        .unwrap()
        .expect("descriptor present");
    assert_eq!(descriptor.compressed_size(), corrupted);
    assert_ne!(descriptor.compressed_size(), original);
    assert_eq!(
        descriptor.uncompressed_size(),
        wayfinder.uncompressed_size_hint()
    );

    // The cross-checking verifier rejects the descriptor/central size mismatch.
    let ent = archive.get_entry(wayfinder).unwrap();
    let mut verifier = DescriptorVerifier::new(ent.reader(), wayfinder.compressed_size_hint());
    let err = io::copy(&mut verifier, &mut io::sink()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}
