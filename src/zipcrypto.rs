//! Traditional PKWARE ("ZipCrypto") encryption and decryption.
//!
//! This is the legacy stream cipher from the PKWARE APPNOTE (§6), kept for
//! interoperating with older archives. It is cryptographically weak and should
//! not be relied on for confidentiality; prefer WinZip AES for new data.
//!
//! [`Decryptor`] wraps an entry's encrypted reader and yields the decrypted
//! (still compressed) body, so layer a decompressor and a CRC check on top.
//! [`Encryptor`] is the write-side counterpart: it emits the encryption header
//! and encrypts the (already compressed) body on its way to the archive.

use crate::crc::crc32_byte;
use std::io::{self, Read, Write};

/// Size of the encryption header prefixing every ZipCrypto entry: 11 random
/// bytes followed by a single check byte (see [`Decryptor::check_byte`]).
pub(crate) const HEADER_LEN: usize = 12;

/// The traditional PKWARE ("ZipCrypto") stream cipher (PKWARE APPNOTE §6).
#[derive(Debug, Clone)]
pub(crate) struct Cipher {
    keys: [u32; 3],
}

impl Cipher {
    /// Initializes the cipher state from a password.
    pub(crate) fn new(password: &[u8]) -> Self {
        let mut cipher = Cipher {
            keys: [0x1234_5678, 0x2345_6789, 0x3456_7890],
        };
        for &byte in password {
            cipher.update(byte);
        }
        cipher
    }

    /// Folds a single plaintext byte into the key schedule.
    #[inline]
    fn update(&mut self, byte: u8) {
        self.keys[0] = crc32_byte(self.keys[0], byte);
        self.keys[1] = self.keys[1]
            .wrapping_add(self.keys[0] & 0xFF)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        self.keys[2] = crc32_byte(self.keys[2], (self.keys[1] >> 24) as u8);
    }

    /// Returns the next byte of the keystream.
    #[inline]
    fn keystream_byte(&self) -> u8 {
        let temp = (self.keys[2] | 2) as u16;
        (temp.wrapping_mul(temp ^ 1) >> 8) as u8
    }

    /// Decrypts a buffer of ciphertext in place, advancing the cipher state.
    #[inline]
    pub(crate) fn decrypt(&mut self, buf: &mut [u8]) {
        for byte in buf {
            let plain = *byte ^ self.keystream_byte();
            self.update(plain);
            *byte = plain;
        }
    }

    /// Encrypts a buffer of plaintext in place, advancing the cipher state.
    ///
    /// Like [`decrypt`](Cipher::decrypt), the key schedule is advanced with the
    /// *plaintext* byte, so the two are exact inverses.
    #[inline]
    pub(crate) fn encrypt(&mut self, buf: &mut [u8]) {
        for byte in buf {
            let cipher = *byte ^ self.keystream_byte();
            self.update(*byte);
            *byte = cipher;
        }
    }
}

/// A [`Read`] adapter that decrypts a ZipCrypto entry on the fly.
///
/// On construction it consumes the 12-byte encryption header; reads
/// then yield the decrypted (but still compressed) body, so wrap it in a
/// decompressor to recover the original data.
///
/// ZipCrypto's single check byte is weak verification, so still validate the
/// output against the entry's CRC32 (e.g. via [`ZipSliceEntry::verifying_reader`]).
///
/// [`ZipSliceEntry::verifying_reader`]: crate::ZipSliceEntry::verifying_reader
///
/// # Examples
///
/// ```rust
/// # use rawzip::ZipArchive;
/// # use rawzip::zipcrypto::Decryptor;
/// # use std::io::Read;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let data = std::fs::read("assets/zipcrypto.zip")?;
/// let archive = ZipArchive::from_slice(&data)?;
/// let entry = archive.entries().next().unwrap()?;
/// let zip_entry = archive.get_entry(entry.wayfinder())?;
///
/// let decryptor = Decryptor::new(zip_entry.data(), b"rawzipiscool")?;
///
/// // Optionally, for a cheap up-front password check, use `check_byte`. Omitted from
/// // the example for brevity.
///
/// let inflater = flate2::read::DeflateDecoder::new(decryptor);
///
/// // The verifying reader checks the decrypted data against the entry's CRC32.
/// let mut output = Vec::new();
/// zip_entry.verifying_reader(inflater).read_to_end(&mut output)?;
/// assert_eq!(output, b"aaaaaaaaaaaaaaaa\n");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Decryptor<R> {
    reader: R,
    cipher: Cipher,
    check_byte: u8,
}

impl<R: Read> Decryptor<R> {
    /// Wraps a reader positioned at the start of an entry's encrypted data,
    /// reading and decrypting the encryption header.
    ///
    /// Check [`check_byte`](Decryptor::check_byte) afterwards for a cheap
    /// password sanity test.
    pub fn new(mut reader: R, password: &[u8]) -> io::Result<Self> {
        let mut cipher = Cipher::new(password);
        let mut header = [0u8; HEADER_LEN];
        reader.read_exact(&mut header)?;
        cipher.decrypt(&mut header);
        Ok(Decryptor {
            reader,
            cipher,
            check_byte: header[HEADER_LEN - 1],
        })
    }

    /// The final byte of the decrypted header, a 1-in-256 password check.
    ///
    /// Equals the high byte of the entry's CRC32, or of its DOS mod time when
    /// the entry carries a data descriptor
    pub fn check_byte(&self) -> u8 {
        self.check_byte
    }

    /// Consumes the reader, returning the wrapped reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: Read> Read for Decryptor<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let read = self.reader.read(buf)?;
        self.cipher.decrypt(&mut buf[..read]);
        Ok(read)
    }
}

const CHUNK: usize = 8 * 1024;

/// A [`Write`] adapter that encrypts a ZipCrypto entry on the fly.
///
/// On construction it emits the 12-byte encryption header; writes
/// then encrypt the (already compressed) body.
///
/// The encryption header includes a single check byte for a cheap reader-side
/// password test. rawzip always emits a data descriptor, so it is the high byte
/// of the entry's DOS mod time (see [`Encryptor::new`]).
///
/// # Examples
///
/// ```rust
/// # use rawzip::{ZipArchive, ZipArchiveWriter, CompressionMethod};
/// # use rawzip::zipcrypto::{Encryptor, Decryptor};
/// # use std::io::{Read, Write};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let password = b"rawzipiscool";
/// let mut output = std::io::Cursor::new(Vec::new());
/// let mut archive = ZipArchiveWriter::new(&mut output);
///
/// let (mut entry, config) = archive
///     .new_file("test.txt")
///     .compression_method(CompressionMethod::Deflate)
///     .encrypted(true)
///     .start()?;
///
/// // In production these 11 bytes must come from a cryptographic RNG.
/// let header_random = [0u8; 11];
/// // rawzip always writes a data descriptor, so the check byte is the high
/// // byte of the DOS mod time.
/// let check_byte = (entry.last_modified_dos().packed_time() >> 8) as u8;
/// let encryptor = Encryptor::new(&mut entry, password, header_random, check_byte)?;
///
/// let deflater = flate2::write::DeflateEncoder::new(encryptor, flate2::Compression::default());
/// let mut writer = config.wrap(deflater);
/// writer.write_all(b"aaaaaaaaaaaaaaaa\n")?;
/// let (deflater, descriptor) = writer.finish()?;
/// deflater.finish()?; // drops the encryptor, releasing the borrow on `entry`
/// entry.finish(descriptor)?;
/// archive.finish()?;
///
/// // Round-trips back through the reader.
/// let zip = output.into_inner();
/// let archive = ZipArchive::from_slice(&zip)?;
/// let entry = archive.entries().next().unwrap()?;
/// let zip_entry = archive.get_entry(entry.wayfinder())?;
/// let decryptor = Decryptor::new(zip_entry.data(), password)?;
/// let inflater = flate2::read::DeflateDecoder::new(decryptor);
/// let mut decoded = Vec::new();
/// zip_entry.verifying_reader(inflater).read_to_end(&mut decoded)?;
/// assert_eq!(decoded, b"aaaaaaaaaaaaaaaa\n");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Encryptor<W> {
    writer: W,
    cipher: Cipher,
    scratch: Box<[u8; CHUNK]>,
}

impl<W: Write> Encryptor<W> {
    /// Wraps a writer positioned at the start of an entry's body, emitting the
    /// encrypted 12-byte encryption header.
    ///
    /// `header_random` are the 11 leading header bytes. They are security-only:
    /// any values produce a valid, round-trippable archive, but reusing them
    /// across entries under the same password leaks information, so draw them
    /// from a cryptographic RNG in production.
    ///
    /// `check_byte` is the 1-in-256 password verifier the reader checks: the
    /// high byte of the entry's CRC32, or of its DOS mod time when the entry
    /// carries a data descriptor (general purpose bit 3). rawzip's writer always
    /// sets that bit, so for its entries compute it as
    /// `(entry.last_modified_dos().packed_time() >> 8) as u8`.
    pub fn new(
        mut writer: W,
        password: &[u8],
        header_random: [u8; 11],
        check_byte: u8,
    ) -> io::Result<Self> {
        let mut cipher = Cipher::new(password);
        let mut header = [0u8; HEADER_LEN];
        header[..HEADER_LEN - 1].copy_from_slice(&header_random);
        header[HEADER_LEN - 1] = check_byte;
        cipher.encrypt(&mut header);
        writer.write_all(&header)?;
        Ok(Encryptor {
            writer,
            cipher,
            scratch: Box::new([0u8; CHUNK]),
        })
    }

    /// Consumes the encryptor, returning the wrapped writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Gets a shared reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Gets a mutable reference to the underlying writer.
    ///
    /// Writing directly to it bypasses encryption, so use with care.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }
}

impl<W: Write> Write for Encryptor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for chunk in buf.chunks(CHUNK) {
            let part = &mut self.scratch[..chunk.len()];
            part.copy_from_slice(chunk);
            self.cipher.encrypt(part);
            self.writer.write_all(part)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Encrypt then decrypt with the same password recovers the input.
    #[test]
    fn encrypt_decrypt_round_trip() {
        let plaintext = b"the quick brown fox";
        let mut buf = *plaintext;
        Cipher::new(b"hunter2").encrypt(&mut buf);
        assert_ne!(&buf, plaintext);
        Cipher::new(b"hunter2").decrypt(&mut buf);
        assert_eq!(&buf, plaintext);
    }

    // An `Encryptor`'s output, including its header, is recovered by `Decryptor`.
    #[test]
    fn encryptor_decryptor_round_trip() {
        let password = b"hunter2";
        let check_byte = 0x42;
        let plaintext = b"the quick brown fox jumps over the lazy dog".repeat(400);

        let mut encrypted = Vec::new();
        let mut encryptor =
            Encryptor::new(&mut encrypted, password, [7u8; HEADER_LEN - 1], check_byte).unwrap();
        encryptor.write_all(&plaintext).unwrap();
        encryptor.flush().unwrap();

        // 12-byte header plus the ciphertext.
        assert_eq!(encrypted.len(), HEADER_LEN + plaintext.len());

        let mut decryptor = Decryptor::new(encrypted.as_slice(), password).unwrap();
        assert_eq!(decryptor.check_byte(), check_byte);
        let mut decrypted = Vec::new();
        decryptor.read_to_end(&mut decrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
