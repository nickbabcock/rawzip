#![no_main]
use libfuzzer_sys::fuzz_target;
use rawzip::{Error, ErrorKind};

fuzz_target!(|data: &[u8]| fuzz_zip(data));

fn fuzz_zip(data: &[u8]) {
    match (fuzz_slice_zip_archive(data), fuzz_reader_zip_archive(data)) {
        (Ok(()), Ok(())) => {}
        (Err(e1), Err(e2)) if errors_eq(&e1, e2.kind()) => {}
        (Err(e1), Err(e2)) => panic!("Inconsistent errors: {:?} vs {:?}", e1, e2),
        (Ok(()), Err(e)) => {
            panic!("Slice method succeeded, but reader method failed: {:?}", e);
        }
        (Err(e), Ok(())) => {
            panic!("Reader method succeeded, but slice method failed: {:?}", e);
        }
    }
}

fn fuzz_reader_zip_archive(data: &[u8]) -> Result<(), rawzip::Error> {
    let mut buf = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let Ok(archive) = rawzip::ZipArchive::from_seekable(std::io::Cursor::new(data), &mut buf)
    else {
        return Ok(());
    };

    let mut entries = archive.entries(&mut buf);
    while let Ok(Some(entry)) = entries.next_entry() {
        if entry.is_dir() {
            continue;
        };

        let _name = entry.file_path().try_normalize();
        let _date = entry.last_modified();
        let _mode = entry.mode();
        let position = entry.wayfinder();
        let Ok(ent) = archive.get_entry(position) else {
            continue;
        };

        let _range = ent.compressed_data_range();
        match entry.compression_method() {
            rawzip::CompressionMethod::Store => {
                let mut verifier = ent.verifying_reader(ent.reader());
                let mut sink = std::io::sink();
                let Ok(_) = std::io::copy(&mut verifier, &mut sink) else {
                    continue;
                };
            }
            rawzip::CompressionMethod::Deflate => {
                let inflater = flate2::read::DeflateDecoder::new(ent.reader());
                let mut verifier = ent.verifying_reader(inflater);
                let mut sink = std::io::sink();
                let Ok(_) = std::io::copy(&mut verifier, &mut sink) else {
                    continue;
                };
            }
            _ => continue,
        }
    }

    Ok(())
}

fn fuzz_slice_zip_archive(data: &[u8]) -> Result<(), rawzip::Error> {
    let Ok(archive) = rawzip::ZipArchive::from_slice(data) else {
        return Ok(());
    };

    let mut entries = archive.entries();
    while let Ok(Some(entry)) = entries.next_entry() {
        if entry.is_dir() {
            continue;
        };

        let _name = entry.file_path().try_normalize();
        let _date = entry.last_modified();
        let _mode = entry.mode();
        let position = entry.wayfinder();
        let Ok(ent) = archive.get_entry(position) else {
            continue;
        };

        let _range = ent.compressed_data_range();
        match entry.compression_method() {
            rawzip::CompressionMethod::Store => {
                let mut verifier = ent.verifying_reader(ent.data());
                let mut sink = std::io::sink();
                let Ok(_) = std::io::copy(&mut verifier, &mut sink) else {
                    continue;
                };
            }
            rawzip::CompressionMethod::Deflate => {
                let inflater = flate2::read::DeflateDecoder::new(ent.data());
                let mut verifier = ent.verifying_reader(inflater);
                let mut sink = std::io::sink();
                let Ok(_) = std::io::copy(&mut verifier, &mut sink) else {
                    continue;
                };
            }
            _ => continue,
        }
    }

    Ok(())
}

fn errors_eq(a: &Error, b: &ErrorKind) -> bool {
    println!("Comparing errors: {:?} vs {:?}", a, b);
    match (a.kind(), b) {
        (
            ErrorKind::InvalidSignature {
                expected: a_exp, ..
            },
            ErrorKind::InvalidSignature {
                expected: b_exp, ..
            },
        ) => a_exp == b_exp,
        (
            ErrorKind::InvalidChecksum {
                expected: a_exp, ..
            },
            ErrorKind::InvalidChecksum {
                expected: b_exp, ..
            },
        ) => a_exp == b_exp,
        (
            ErrorKind::InvalidSize {
                expected: a_exp, ..
            },
            ErrorKind::InvalidSize {
                expected: b_exp, ..
            },
        ) => a_exp == b_exp,
        (ErrorKind::InvalidUtf8(a), ErrorKind::InvalidUtf8(b)) => a == b,
        (ErrorKind::InvalidInput { msg: a }, ErrorKind::InvalidInput { msg: b }) => a == b,
        (ErrorKind::IO(a), ErrorKind::IO(b)) => a.kind() == b.kind(),
        (ErrorKind::Eof, ErrorKind::Eof) => true,
        (ErrorKind::MissingEndOfCentralDirectory, ErrorKind::MissingEndOfCentralDirectory) => true,
        (
            ErrorKind::MissingZip64EndOfCentralDirectory,
            ErrorKind::MissingZip64EndOfCentralDirectory,
        ) => true,
        (ErrorKind::BufferTooSmall, ErrorKind::BufferTooSmall) => true,
        _ => false,
    }
}
