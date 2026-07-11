# WinZip AES

rawzip provides the hooks to support modern WinZip AES encryption.

See the [WinZip AES integration test][winzip-aes-test] for reading and writing WinZip AES archives. The test uses the RustCrypto ecosystem
(`aes`, `ctr`, `hmac`, `pbkdf2`, and `sha1`), but you can use OpenSSL too.

Detect an encrypted entry by checking for
[`CompressionMethod::AES`][crate::CompressionMethod::AES]. The actual compression
method is tucked into the final two bytes of the
[`ExtraFieldId::AES`][crate::extra_fields::ExtraFieldId::AES] extra field.

# ZipCrypto (legacy)

rawzip includes the legacy PKWARE ZipCrypto primitives in the
[`zipcrypto`][crate::zipcrypto] module. ZipCrypto is weak and should not be used
to protect new archives, but it remains useful for compatibility with older
files. [`Decryptor`][crate::zipcrypto::Decryptor] yields decrypted, still
compressed bytes, so decrypt before decompressing and verifying:

```rust
use rawzip::{ZipArchive, zipcrypto::Decryptor};
use std::io::Read;

let data = std::fs::read("assets/zipcrypto.zip")?;
let archive = ZipArchive::from_slice(&data)?;
let entry = archive.entries().next_entry()?.expect("one entry");
// ZipCrypto keeps the real compression method and uses general-purpose bit 0
// to signal encryption.
assert!(entry.flags().is_encrypted());
assert_eq!(entry.compression_method(), rawzip::CompressionMethod::DEFLATE);
let local = archive.get_entry(entry.wayfinder())?;

let decryptor = Decryptor::new(local.data(), b"rawzipiscool")?;
let header = local.local_header();

// Optionally, use the check byte to fail fast before decompression.
let expected_check_byte = if header.flags().has_data_descriptor() {
    (header.last_modified_dos().packed_time() >> 8) as u8
} else {
    (header.crc32() >> 24) as u8
};
assert_eq!(decryptor.check_byte(), expected_check_byte);

let inflater = flate2::read::DeflateDecoder::new(decryptor);
let mut reader = local.verifying_reader(inflater);

let mut output = Vec::new();
reader.read_to_end(&mut output)?;
assert_eq!(output, b"aaaaaaaaaaaaaaaa\n");
# Ok::<(), Box<dyn std::error::Error>>(())
```

The check byte rejects an incorrect password with probability 255/256; it is
not authentication. Always decompress through `verifying_reader` so the full
CRC and uncompressed size are checked too.

Writing uses the reverse stack: rawzip tracks the plaintext, the compressor
writes into the encryptor, and the encryptor writes into the entry:

```rust
use rawzip::{CompressionMethod, ZipArchiveWriter};
use rawzip::zipcrypto::Encryptor;
use std::io::Write;

let password = b"rawzipiscool";
let mut output = std::io::Cursor::new(Vec::new());
let mut archive = ZipArchiveWriter::new(&mut output);
let (mut entry, config) = archive
    .new_file("test.txt")
    .compression_method(CompressionMethod::DEFLATE)
    .encrypted(true)
    .start()?;

// Real applications must fill these 11 bytes with a cryptographic RNG.
let header_random = [0u8; 11];
// rawzip writes a data descriptor, so ZipCrypto uses the high byte of the DOS
// modification time rather than the CRC for its check byte.
let check_byte = (entry.last_modified_dos().packed_time() >> 8) as u8;
let encryptor = Encryptor::new(&mut entry, password, header_random, check_byte)?;
let deflater = flate2::write::DeflateEncoder::new(
    encryptor,
    flate2::Compression::default(),
);
let mut writer = config.wrap(deflater);
writer.write_all(b"legacy compatibility")?;

let (deflater, descriptor) = writer.finish()?;
deflater.finish()?; // finishes compression and releases the entry borrow
entry.finish(descriptor)?;
archive.finish()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

[winzip-aes-test]: https://github.com/nickbabcock/rawzip/blob/master/tests/it/encryption_tests.rs
