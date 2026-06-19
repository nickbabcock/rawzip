use aes::{Aes128, Aes192, Aes256};
use ctr::cipher::{KeyIvInit, StreamCipher};
use hmac::{Hmac, KeyInit, Mac};
use pbkdf2::pbkdf2;
use rawzip::{CompressionMethod, RECOMMENDED_BUFFER_SIZE, ZipArchive, extra_fields::ExtraFieldId};
use sha1::Sha1;
use std::io::{self, Read};

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

/// WinZip AES salt and key lengths derived from the AES extra field strength
/// byte (WinZip AES spec / APPNOTE Appendix E). The salt is always half the
/// key length, while the password verifier (2 bytes) and authentication code
/// (10 bytes) are fixed regardless of strength.
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

    // The salt and key lengths vary with the AES strength; the rest of the
    // layout (password verifier, authentication code) is fixed.
    let (salt_len, key_len) = aes_key_lengths(central_metadata.strength);

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

    let ciphertext_len = compressed_size
        .checked_sub((salt_len + PASSWORD_VERIFIER_LEN + AUTH_CODE_LEN) as u64)
        .unwrap();
    let mut reader = encrypted_entry.reader();
    let mut salt = vec![0u8; salt_len];
    let mut password_verifier = [0u8; PASSWORD_VERIFIER_LEN];
    reader.read_exact(&mut salt).unwrap();
    reader.read_exact(&mut password_verifier).unwrap();

    let mut derived = vec![0u8; key_len * 2 + PASSWORD_VERIFIER_LEN];
    pbkdf2::<HmacSha1>(PASSWORD, &salt, 1_000, &mut derived).unwrap();
    let (encryption_key, rest) = derived.split_at(key_len);
    let (authentication_key, derived_password_verifier) = rest.split_at(key_len);
    assert_eq!(&password_verifier, derived_password_verifier);

    let mac = HmacSha1::new_from_slice(authentication_key).unwrap();
    let mut counter = [0u8; 16];
    counter[0] = 1;
    let cipher = new_cipher(central_metadata.strength, encryption_key, &counter);
    let ciphertext = reader.by_ref().take(ciphertext_len);
    let aes_reader = AesReader {
        reader: ciphertext,
        cipher,
        mac,
    };

    let mut output = Vec::new();
    let mut decoder = match central_metadata.compression_method {
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

    assert_eq!(output.len() as u64, uncompressed_size);
    assert_eq!(output, b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    let output_crc = rawzip::crc32(&output);
    assert_ne!(output_crc, 0);
    // AE-1 stores the real CRC32, so it must match the decrypted data.
    if central_metadata.vendor_version == 1 {
        assert_eq!(output_crc, stored_crc);
    }
}
