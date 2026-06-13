use aes::{Aes128, Aes192, Aes256};
use ctr::cipher::{KeyIvInit, StreamCipher};
use hmac::{Hmac, KeyInit, Mac};
use pbkdf2::pbkdf2;
use rawzip::{
    CompressionMethod, Crc32Option, Header, RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipArchiveWriter,
    ZipLocator, extra_fields::ExtraFieldId,
};
use sha1::Sha1;
use std::io::{self, Read, Write};

// WinZip AES always uses a 128-bit (AES block size) little-endian CTR counter,
// regardless of the key size.
type Aes128Ctr = ctr::Ctr128LE<Aes128>;
type Aes192Ctr = ctr::Ctr128LE<Aes192>;
type Aes256Ctr = ctr::Ctr128LE<Aes256>;
type HmacSha1 = Hmac<Sha1>;

const PASSWORD: &[u8] = b"rawzipiscool";
const AES_EXTRA_FIELD_LEN: usize = 7;
const PASSWORD_VERIFIER_LEN: usize = 2;
const AUTH_CODE_LEN: usize = 10;

/// Salt and key lengths for the AES strength byte (APPNOTE Appendix E). The
/// salt is always half the key length.
fn aes_key_lengths(strength: u8) -> (usize /* salt */, usize /* key */) {
    let key_len = match strength {
        1 => 16, // AES-128
        2 => 24, // AES-192
        3 => 32, // AES-256
        other => unreachable!("invalid AES strength: {other}"),
    };
    (key_len / 2, key_len)
}

/// An AES-CTR keystream cipher whose key size is selected at runtime from the
/// strength byte. Using an enum instead of `Box<dyn StreamCipher>` avoids the
/// heap allocation and per-read virtual dispatch.
enum AesCipher {
    Aes128(Aes128Ctr),
    Aes192(Aes192Ctr),
    Aes256(Aes256Ctr),
}

impl AesCipher {
    fn apply_keystream(&mut self, buf: &mut [u8]) {
        match self {
            AesCipher::Aes128(c) => c.apply_keystream(buf),
            AesCipher::Aes192(c) => c.apply_keystream(buf),
            AesCipher::Aes256(c) => c.apply_keystream(buf),
        }
    }
}

/// Constructs the appropriate AES-CTR keystream cipher for the given strength.
fn new_cipher(strength: u8, key: &[u8], counter: &[u8; 16]) -> AesCipher {
    match strength {
        1 => AesCipher::Aes128(Aes128Ctr::new_from_slices(key, counter).unwrap()),
        2 => AesCipher::Aes192(Aes192Ctr::new_from_slices(key, counter).unwrap()),
        3 => AesCipher::Aes256(Aes256Ctr::new_from_slices(key, counter).unwrap()),
        other => unreachable!("invalid AES strength: {other}"),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AesExtraField {
    vendor_version: u16,
    vendor_id: [u8; 2],
    strength: u8,
    compression_method: CompressionMethod,
}

struct AesReader<R> {
    reader: R,
    cipher: AesCipher,
    mac: HmacSha1,
}

impl<R: Read> Read for AesReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.reader.read(buf)?;
        self.mac.update(&buf[..read]);
        self.cipher.apply_keystream(&mut buf[..read]);
        Ok(read)
    }
}

impl<R> AesReader<R> {
    fn into_parts(self) -> (R, HmacSha1) {
        (self.reader, self.mac)
    }
}

fn parse_aes_extra_field(data: &[u8]) -> AesExtraField {
    assert_eq!(data.len(), AES_EXTRA_FIELD_LEN);
    AesExtraField {
        vendor_version: u16::from_le_bytes(data[0..2].try_into().unwrap()),
        vendor_id: data[2..4].try_into().unwrap(),
        strength: data[4],
        compression_method: u16::from_le_bytes(data[5..7].try_into().unwrap()).into(),
    }
}

fn find_aes_extra_field<'a>(
    fields: impl Iterator<Item = (ExtraFieldId, &'a [u8])>,
) -> AesExtraField {
    let fields = fields
        .filter(|(id, _)| *id == ExtraFieldId::AES)
        .map(|(_, data)| parse_aes_extra_field(data))
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 1, "expected exactly one AES extra field");
    fields.into_iter().next().unwrap()
}

/// The keys and password verifier WinZip AES derives from the password and salt
/// via PBKDF2-HMAC-SHA1 (1000 iterations), in their on-disk order (WinZip AES /
/// APPNOTE Appendix E).
struct AesKeys {
    encryption_key: Vec<u8>,
    authentication_key: Vec<u8>,
    password_verifier: [u8; PASSWORD_VERIFIER_LEN],
}

fn derive_aes_keys(salt: &[u8], key_len: usize) -> AesKeys {
    let mut derived = vec![0u8; key_len * 2 + PASSWORD_VERIFIER_LEN];
    pbkdf2::<HmacSha1>(PASSWORD, salt, 1_000, &mut derived).unwrap();
    let (encryption_key, rest) = derived.split_at(key_len);
    let (authentication_key, password_verifier) = rest.split_at(key_len);
    AesKeys {
        encryption_key: encryption_key.to_vec(),
        authentication_key: authentication_key.to_vec(),
        password_verifier: password_verifier.try_into().unwrap(),
    }
}

/// Reads a WinZip AES payload (salt, password verifier, ciphertext, auth code)
/// from `reader`, then decrypts, decompresses, and verifies it, returning the
/// recovered plaintext.
fn decrypt_winzip_aes_payload<R: Read>(
    mut reader: R,
    compressed_size: u64,
    strength: u8,
    compression_method: CompressionMethod,
) -> Vec<u8> {
    let (salt_len, key_len) = aes_key_lengths(strength);
    let ciphertext_len = compressed_size
        .checked_sub((salt_len + PASSWORD_VERIFIER_LEN + AUTH_CODE_LEN) as u64)
        .unwrap();

    let mut salt = vec![0u8; salt_len];
    let mut password_verifier = [0u8; PASSWORD_VERIFIER_LEN];
    reader.read_exact(&mut salt).unwrap();
    reader.read_exact(&mut password_verifier).unwrap();

    let keys = derive_aes_keys(&salt, key_len);
    assert_eq!(password_verifier, keys.password_verifier);

    let mac = HmacSha1::new_from_slice(&keys.authentication_key).unwrap();
    let mut counter = [0u8; 16];
    counter[0] = 1;
    let cipher = new_cipher(strength, &keys.encryption_key, &counter);
    let ciphertext = reader.by_ref().take(ciphertext_len);
    let aes_reader = AesReader {
        reader: ciphertext,
        cipher,
        mac,
    };

    let mut output = Vec::new();
    let mut decoder = match compression_method {
        CompressionMethod::Deflate => flate2::read::DeflateDecoder::new(aes_reader),
        method => panic!("unsupported AES compression method: {method:?}"),
    };
    decoder.read_to_end(&mut output).unwrap();

    let aes_reader = decoder.into_inner();
    let (ciphertext, mac) = aes_reader.into_parts();
    assert_eq!(ciphertext.limit(), 0);

    let mut authentication_code = [0u8; AUTH_CODE_LEN];
    reader.read_exact(&mut authentication_code).unwrap();
    assert_eq!(reader.read(&mut [0u8; 1]).unwrap(), 0);
    let computed_authentication_code = mac.finalize().into_bytes();
    assert_eq!(
        &authentication_code,
        &computed_authentication_code[..AUTH_CODE_LEN]
    );

    output
}

#[test]
fn decrypt_winzip_aes128_entry_using_rawzip_primitives() {
    decrypt_winzip_aes_entry("assets/aes128.zip", 1, 2);
}

#[test]
fn decrypt_winzip_aes192_entry_using_rawzip_primitives() {
    decrypt_winzip_aes_entry("assets/aes192.zip", 2, 2);
}

#[test]
fn decrypt_winzip_aes256_entry_using_rawzip_primitives() {
    decrypt_winzip_aes_entry("assets/aes256.zip", 3, 2);
}

#[test]
fn decrypt_winzip_aes256_ae1_entry_using_rawzip_primitives() {
    decrypt_winzip_aes_entry("assets/aes256-ae1.zip", 3, 1);
}

fn decrypt_winzip_aes_entry(path: &str, expected_strength: u8, expected_vendor_version: u16) {
    let file = std::fs::File::open(path).unwrap();
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buffer).unwrap();

    let expected_metadata = AesExtraField {
        vendor_version: expected_vendor_version,
        vendor_id: *b"AE",
        strength: expected_strength,
        compression_method: CompressionMethod::Deflate,
    };
    let mut entries = archive.entries(&mut buffer);
    let entry = entries.next_entry().unwrap().unwrap();

    assert_eq!(entry.file_path().as_ref(), b"test.txt");
    assert_eq!(entry.compression_method(), CompressionMethod::Aes);
    assert!(entry.flags().is_encrypted());
    assert!(!entry.flags().has_strong_encryption());

    let central_metadata = find_aes_extra_field(entry.extra_fields());
    assert_eq!(central_metadata, expected_metadata);

    let stored_crc = entry.crc32();
    match central_metadata.vendor_version {
        // APPNOTE Appendix E.6.2 requires AE-2 entries to store zero in the CRC
        // field.
        2 => assert_eq!(stored_crc, 0),
        // AE-1 entries retain the real CRC32 of the uncompressed data
        1 => assert_eq!(stored_crc, 2783462679),
        other => panic!("unexpected AES vendor version: {other}"),
    }

    let compressed_size = entry.compressed_size_hint();
    let uncompressed_size = entry.uncompressed_size_hint();
    let encrypted_entry = archive.get_entry(entry.wayfinder()).unwrap();
    let mut local_header_buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let local_header = encrypted_entry
        .local_header(&mut local_header_buffer)
        .unwrap();
    let local_metadata = find_aes_extra_field(local_header.extra_fields());
    assert_eq!(local_metadata, expected_metadata);
    assert!(local_header.flags().is_encrypted());

    let output = decrypt_winzip_aes_payload(
        encrypted_entry.reader(),
        compressed_size,
        central_metadata.strength,
        central_metadata.compression_method,
    );

    assert_eq!(output.len() as u64, uncompressed_size);
    assert_eq!(output, b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    let output_crc = rawzip::crc32(&output);
    assert_ne!(output_crc, 0);
    // AE-1 stores the real CRC32, so it must match the decrypted data.
    if central_metadata.vendor_version == 1 {
        assert_eq!(output_crc, stored_crc);
    }
}

/// A writer that encrypts compressed data with AES-CTR and accumulates the
/// WinZip authentication code over the produced ciphertext (encrypt-then-MAC).
///
/// This is the write-side counterpart to [`AesReader`]: deflate-compressed
/// bytes are fed in, encrypted, and forwarded to the underlying archive writer.
struct AesWriter<W> {
    writer: W,
    cipher: AesCipher,
    mac: HmacSha1,
    scratch: Vec<u8>,
}

impl<W: Write> Write for AesWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.scratch.clear();
        self.scratch.extend_from_slice(buf);
        self.cipher.apply_keystream(&mut self.scratch);
        self.mac.update(&self.scratch);
        self.writer.write_all(&self.scratch)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl<W> AesWriter<W> {
    fn into_mac(self) -> HmacSha1 {
        self.mac
    }
}

/// Builds the 7-byte WinZip AES extra field (APPNOTE Appendix E):
/// vendor version, the "AE" vendor id, the AES strength byte, and the
/// compression method actually applied before encryption.
fn aes_extra_field(vendor_version: u16, strength: u8, method: CompressionMethod) -> [u8; 7] {
    let mut field = [0u8; AES_EXTRA_FIELD_LEN];
    field[0..2].copy_from_slice(&vendor_version.to_le_bytes());
    field[2..4].copy_from_slice(b"AE");
    field[4] = strength;
    // The actual compression method id (8 for deflate) lives here; the entry's
    // own compression method is 99 (AES).
    let method_id = match method {
        CompressionMethod::Deflate => 8u16,
        CompressionMethod::Store => 0u16,
        other => panic!("unsupported AES compression method: {other:?}"),
    };
    field[5..7].copy_from_slice(&method_id.to_le_bytes());
    field
}

/// Creates a single-entry, WinZip AES-encrypted, deflate-compressed ZIP archive
/// using only rawzip's public writer primitives plus the RustCrypto stack, and
/// returns the raw archive bytes.
///
/// A deterministic salt is used so the output is reproducible; real archives
/// must use a cryptographically random salt.
fn create_winzip_aes_entry(strength: u8, vendor_version: u16, plaintext: &[u8]) -> Vec<u8> {
    let (salt_len, key_len) = aes_key_lengths(strength);
    let salt: Vec<u8> = (0..salt_len as u8).collect();
    let keys = derive_aes_keys(&salt, key_len);

    let mut counter = [0u8; 16];
    counter[0] = 1;
    let cipher = new_cipher(strength, &keys.encryption_key, &counter);
    let mac = HmacSha1::new_from_slice(&keys.authentication_key).unwrap();

    // AE-2 must store a zero CRC; AE-1 retains the real CRC32 of the plaintext.
    let crc32_option = match vendor_version {
        2 => Crc32Option::Skip,
        1 => Crc32Option::Calculate,
        other => panic!("unsupported AES vendor version: {other}"),
    };

    let mut output = std::io::Cursor::new(Vec::new());
    let mut archive = ZipArchiveWriter::new(&mut output);

    let extra = aes_extra_field(vendor_version, strength, CompressionMethod::Deflate);
    let (mut entry, config) = archive
        .new_file("test.txt")
        .compression_method(CompressionMethod::Aes)
        .encrypted(true)
        .extra_field(ExtraFieldId::AES, &extra, Header::default())
        .unwrap()
        .crc32(crc32_option)
        .start()
        .unwrap();

    // Payload layout: salt, password verifier, AES-CTR encrypted deflate stream,
    // then the 10-byte authentication code. Salt and verifier are unencrypted.
    entry.write_all(&salt).unwrap();
    entry.write_all(&keys.password_verifier).unwrap();

    let aes_writer = AesWriter {
        writer: &mut entry,
        cipher,
        mac,
        scratch: Vec::new(),
    };
    let deflater = flate2::write::DeflateEncoder::new(aes_writer, flate2::Compression::default());

    // The data writer tracks the plaintext CRC32/length for the data descriptor.
    let mut writer = config.wrap(deflater);
    writer.write_all(plaintext).unwrap();
    let (deflater, descriptor) = writer.finish().unwrap();
    let aes_writer = deflater.finish().unwrap();
    let mac = aes_writer.into_mac();

    // The authentication code is the first 10 bytes of the HMAC-SHA1 over the
    // ciphertext, written immediately after the encrypted data.
    let auth_code = mac.finalize().into_bytes();
    entry.write_all(&auth_code[..AUTH_CODE_LEN]).unwrap();

    entry.finish(descriptor).unwrap();
    archive.finish().unwrap();

    output.into_inner()
}

fn roundtrip_winzip_aes_entry(strength: u8, vendor_version: u16) {
    let plaintext = b"the quick brown fox jumps over the lazy dog".repeat(8);
    let zip = create_winzip_aes_entry(strength, vendor_version, &plaintext);

    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::new()
        .locate_in_reader(zip.as_slice(), &mut buffer, zip.len() as u64)
        .map_err(|(_, e)| e)
        .unwrap();

    let expected_metadata = AesExtraField {
        vendor_version,
        vendor_id: *b"AE",
        strength,
        compression_method: CompressionMethod::Deflate,
    };

    let mut entries = archive.entries(&mut buffer);
    let entry = entries.next_entry().unwrap().unwrap();
    assert_eq!(entry.file_path().as_ref(), b"test.txt");
    assert_eq!(entry.compression_method(), CompressionMethod::Aes);
    assert!(entry.flags().is_encrypted());
    assert!(!entry.flags().has_strong_encryption());

    // The AES extra field must be present in both the central directory and the
    // local file header.
    let central_metadata = find_aes_extra_field(entry.extra_fields());
    assert_eq!(central_metadata, expected_metadata);

    let stored_crc = entry.crc32();
    match vendor_version {
        2 => assert_eq!(stored_crc, 0),
        1 => assert_eq!(stored_crc, rawzip::crc32(&plaintext)),
        other => panic!("unexpected AES vendor version: {other}"),
    }

    let compressed_size = entry.compressed_size_hint();
    let uncompressed_size = entry.uncompressed_size_hint();
    assert_eq!(uncompressed_size, plaintext.len() as u64);

    let encrypted_entry = archive.get_entry(entry.wayfinder()).unwrap();
    let mut local_header_buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let local_header = encrypted_entry
        .local_header(&mut local_header_buffer)
        .unwrap();
    let local_metadata = find_aes_extra_field(local_header.extra_fields());
    assert_eq!(local_metadata, expected_metadata);
    assert!(local_header.flags().is_encrypted());

    // Decrypt using the same primitives the read-side tests rely on.
    let decoded = decrypt_winzip_aes_payload(
        encrypted_entry.reader(),
        compressed_size,
        strength,
        CompressionMethod::Deflate,
    );
    assert_eq!(decoded, plaintext);
}

#[test]
fn roundtrip_winzip_aes256_ae2() {
    roundtrip_winzip_aes_entry(3, 2);
}

#[test]
fn roundtrip_winzip_aes256_ae1() {
    roundtrip_winzip_aes_entry(3, 1);
}
