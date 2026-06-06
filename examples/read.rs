use rawzip::{RECOMMENDED_BUFFER_SIZE, ZipArchive};
use std::env;
use std::fs::File;
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <archive.zip> [filename]", args[0]);
        eprintln!("Extract a file from a ZIP archive to stdout");
        eprintln!("If no filename is provided, extracts the last file in the central directory");
        std::process::exit(1);
    }

    let archive_path = &args[1];
    let target_filename = args.get(2);

    let file = File::open(archive_path)?;
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buffer)?;

    let mut found_entry = None;
    let mut entries = archive.entries(&mut buffer);
    while let Some(entry) = entries.next_entry()? {
        if entry.is_dir() {
            continue;
        }

        match target_filename {
            Some(name) if name.as_bytes() == entry.file_path().as_ref() => {
                found_entry = Some((entry.wayfinder(), entry.compression_method()));
                break;
            }
            None => {
                found_entry = Some((entry.wayfinder(), entry.compression_method()));
            }
            _ => {}
        }
    }

    let Some((wayfinder, compression_method)) = found_entry else {
        eprintln!("File not found in archive");
        std::process::exit(1);
    };

    let zip_entry = archive.get_entry(wayfinder)?;
    let reader = zip_entry.reader();

    let stdout = io::stdout();
    let mut stdout_lock = stdout.lock();

    match compression_method {
        rawzip::CompressionMethod::STORE => {
            let mut verifier = zip_entry.verifying_reader(reader);
            io::copy(&mut verifier, &mut stdout_lock)?;
        }
        rawzip::CompressionMethod::DEFLATE => {
            let inflater = flate2::read::DeflateDecoder::new(reader);
            let mut verifier = zip_entry.verifying_reader(inflater);
            io::copy(&mut verifier, &mut stdout_lock)?;
        }
        rawzip::CompressionMethod::ZSTD => {
            let decoder = zstd::Decoder::new(reader)?;
            let mut verifier = zip_entry.verifying_reader(decoder);
            io::copy(&mut verifier, &mut stdout_lock)?;
        }
        _ => {
            eprintln!("Error: Unsupported compression method: {compression_method:?}");
            std::process::exit(1);
        }
    }

    Ok(())
}
