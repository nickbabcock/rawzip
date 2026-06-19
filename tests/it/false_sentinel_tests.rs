use rawzip::{RECOMMENDED_BUFFER_SIZE, ZipArchive};
use std::io::Cursor;

const LFH_SIG: u32 = 0x0403_4b50;
const CDH_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

/// Minimal raw ZIP byte builder for stored, empty entries. Keeps total size
/// small so even a 65535-entry archive stays a few MiB and well under 4 GiB.
struct RawZip {
    buf: Vec<u8>,
    /// (name, local_header_offset) for each written local file header.
    locals: Vec<(Vec<u8>, u32)>,
}

impl RawZip {
    fn new() -> Self {
        RawZip {
            buf: Vec::new(),
            locals: Vec::new(),
        }
    }

    fn push16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn push32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append a local file header for a stored, empty file.
    fn add_local(&mut self, name: &[u8]) {
        let offset = self.buf.len() as u32;
        self.push32(LFH_SIG);
        self.push16(20); // version needed
        self.push16(0); // flags
        self.push16(0); // compression: store
        self.push16(0); // mod time
        self.push16(0); // mod date
        self.push32(0); // crc32 (empty)
        self.push32(0); // compressed size
        self.push32(0); // uncompressed size
        self.push16(name.len() as u16);
        self.push16(0); // extra len
        self.buf.extend_from_slice(name);
        self.locals.push((name.to_vec(), offset));
    }

    /// Append the central directory for all previously added locals, returning
    /// (central_dir_offset, central_dir_size).
    fn write_central_directory(&mut self) -> (u32, u32) {
        let cd_start = self.buf.len() as u32;
        let locals = std::mem::take(&mut self.locals);
        for (name, local_offset) in &locals {
            self.push32(CDH_SIG);
            self.push16(20); // version made by
            self.push16(20); // version needed
            self.push16(0); // flags
            self.push16(0); // compression
            self.push16(0); // mod time
            self.push16(0); // mod date
            self.push32(0); // crc32
            self.push32(0); // compressed size
            self.push32(0); // uncompressed size
            self.push16(name.len() as u16);
            self.push16(0); // extra len
            self.push16(0); // comment len
            self.push16(0); // disk number start
            self.push16(0); // internal attrs
            self.push32(0); // external attrs
            self.push32(*local_offset);
            self.buf.extend_from_slice(name);
        }
        let cd_size = self.buf.len() as u32 - cd_start;
        (cd_start, cd_size)
    }

    /// Append a classic EOCD record.
    fn write_eocd(&mut self, entries: u16, cd_size: u32, cd_offset: u32) {
        self.push32(EOCD_SIG);
        self.push16(0); // disk number
        self.push16(0); // eocd disk
        self.push16(entries); // entries this disk
        self.push16(entries); // total entries
        self.push32(cd_size);
        self.push32(cd_offset);
        self.push16(0); // comment len
    }
}

/// A conformant non-zip64 archive with exactly 65535 entries. The EOCD entry
/// count equals the 0xFFFF sentinel, but there is deliberately no zip64 record.
fn build_65535_non_zip64() -> Vec<u8> {
    const N: usize = 65535;
    let mut z = RawZip::new();
    for i in 0..N {
        z.add_local(format!("{i}").as_bytes());
    }
    let (cd_offset, cd_size) = z.write_central_directory();
    z.write_eocd(N as u16, cd_size, cd_offset);
    z.buf
}

fn count_slice_entries(data: &[u8]) -> usize {
    let archive = ZipArchive::from_slice(data).expect("locate (slice)");
    archive
        .entries()
        .map(|e| e.map(|_| ()))
        .collect::<Result<Vec<_>, _>>()
        .expect("iterate (slice)")
        .len()
}

fn count_reader_entries(data: &[u8]) -> usize {
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive =
        ZipArchive::from_seekable(Cursor::new(data), &mut buffer).expect("locate (reader)");
    let mut entries = archive.entries(&mut buffer);
    let mut count = 0;
    while entries.next_entry().expect("iterate (reader)").is_some() {
        count += 1;
    }
    count
}

// Generate a classic zip file with exactly 65535 entries.
//
// Cross-validated by 7-Zip, python zipfile, and unzip.
//
// issue https://github.com/thejoshwolfe/yauzl/issues/108
#[test]
fn read_65535_entry_non_zip64_slice() {
    let data = build_65535_non_zip64();
    assert_eq!(count_slice_entries(&data), 65535);
}

#[test]
fn read_65535_entry_non_zip64_reader() {
    let data = build_65535_non_zip64();
    assert_eq!(count_reader_entries(&data), 65535);
}
