//! This example demonstrates how to safely extract ZIP archives. It implements
//! several security measures to prevent common ZIP-based attacks while
//! providing a basic ZIP extraction. Limitations of this example (but not of
//! rawzip).
//!
//! - Supports only store and deflate compression methods
//! - Supports only UTF-8 file paths

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <archive.zip> <target_dir>", args[0]);
        std::process::exit(1);
    }

    let archive_path = &args[1];
    let target_dir = &args[2];
    extract_zip_archive(archive_path, target_dir)?;
    Ok(())
}

fn extract_zip_archive<P: AsRef<std::path::Path>>(
    archive_path: P,
    target_dir: P,
) -> Result<(), ExtractionError> {
    use rawzip::{CompressionMethod, ZipArchive, RECOMMENDED_BUFFER_SIZE};

    let archive_path = archive_path.as_ref();
    let target_dir = target_dir.as_ref();

    // Create target directory if it doesn't exist
    if !target_dir.exists() {
        std::fs::create_dir_all(target_dir).map_err(|e| {
            ExtractionError::io_context(
                e,
                format!(
                    "Failed to create target directory: {}",
                    target_dir.display()
                ),
            )
        })?;
    }

    let file = std::fs::File::open(archive_path).map_err(|e| {
        ExtractionError::io_context(
            e,
            format!("Failed to open ZIP archive: {}", archive_path.display()),
        )
    })?;
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buffer).map_err(|e| {
        ExtractionError::zip_context(
            e,
            format!("Failed to read ZIP archive: {}", archive_path.display()),
        )
    })?;

    let mut zip_start_offset = archive.directory_offset();

    // Maintain sorted list of compressed data ranges to detect overlaps:
    // https://www.bamsoftware.com/hacks/zipbomb/
    let mut compressed_ranges = Vec::new();
    let expected_entries = archive.entries_hint();
    let mut entries_processed = 0u64;

    let mut entries = archive.entries(&mut buffer);
    loop {
        let entry = match entries.next_entry() {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(e) => {
                // When an error is encountered, if we have processed the
                // expected number of entries, we treat this as reaching the end
                // rather than an error. For the vast majority of zips, this is
                // not necessary but is included for completeness.
                if entries_processed == expected_entries {
                    break;
                } else {
                    return Err(ExtractionError::zip_context(
                        e,
                        "Failed to read ZIP entry".to_string(),
                    ));
                }
            }
        };

        entries_processed += 1;
        let raw_path = entry.file_path();

        // Avoid zip slips by normalizing the path. Note that it is not required for
        // zip file paths to be UTF-8
        let file_path = match raw_path.try_normalize() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Skipped suspicious path: {raw_path:?}, reason: {e}");
                continue;
            }
        };

        let out_path = target_dir.join(file_path.as_ref());
        let zip_entry = archive.get_entry(entry.wayfinder()).map_err(|e| {
            ExtractionError::zip_context(
                e,
                format!("Failed to get ZIP entry for file: {}", file_path.as_ref()),
            )
        })?;

        zip_start_offset = entry.local_header_offset().min(zip_start_offset);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| {
                ExtractionError::io_context(
                    e,
                    format!("Failed to create directory: {}", out_path.display()),
                )
            })?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ExtractionError::io_context(
                    e,
                    format!("Failed to create parent directory: {}", parent.display()),
                )
            })?;
        }

        let reader = zip_entry.reader();

        // Check for overlapping compressed data ranges
        let current_range = zip_entry.compressed_data_range();
        let (current_start, current_end) = current_range;

        // Find insertion point to maintain sorted order by start offset
        let insert_pos = compressed_ranges
            .binary_search_by_key(&current_start, |&(start, _)| start)
            .unwrap_or_else(|pos| pos);

        // Check overlap with previous range (if exists)
        if insert_pos > 0 {
            let (_, prev_end) = compressed_ranges[insert_pos - 1];
            if prev_end > current_start {
                eprintln!("Skipped file with overlapping compressed data: {file_path:?} (range {}..{} overlaps with previous range ending at {})", 
                             current_start, current_end, prev_end);
                continue;
            }
        }

        // Check overlap with next range (if exists)
        if insert_pos < compressed_ranges.len() {
            let (next_start, _) = compressed_ranges[insert_pos];
            if current_end > next_start {
                eprintln!("Skipped file with overlapping compressed data: {file_path:?} (range {}..{} overlaps with next range starting at {})", 
                             current_start, current_end, next_start);
                continue;
            }
        }

        // Insert the range at the correct position to maintain sorted order
        compressed_ranges.insert(insert_pos, current_range);

        // "DEFLATE, the compression algorithm most commonly supported by zip
        // parsers, cannot achieve a compression ratio greater than 1032"
        // https://www.bamsoftware.com/hacks/zipbomb/
        let compressed_size = entry.compressed_size_hint();
        let uncompressed_size = entry.uncompressed_size_hint();
        if compressed_size > 0 && uncompressed_size / compressed_size > 1032 {
            eprintln!("Skipped potential zip bomb: compression ratio {:.1}:1 exceeds limit of 1032:1 for file: {file_path:?}", 
                         uncompressed_size as f64 / compressed_size as f64);
            continue;
        }

        let mut outfile = std::fs::File::create(&out_path).map_err(|e| {
            ExtractionError::io_context(
                e,
                format!("Failed to create output file: {}", out_path.display()),
            )
        })?;
        let method = entry.compression_method();
        match method {
            CompressionMethod::Store => {
                let mut verifier = zip_entry.verifying_reader(reader);
                std::io::copy(&mut verifier, &mut outfile).map_err(|e| {
                    ExtractionError::io_context(
                        e,
                        format!(
                            "Failed to extract uncompressed file: {}",
                            file_path.as_ref()
                        ),
                    )
                })?;
            }
            CompressionMethod::Deflate => {
                let inflater = flate2::read::DeflateDecoder::new(reader);
                let mut verifier = zip_entry.verifying_reader(inflater);
                std::io::copy(&mut verifier, &mut outfile).map_err(|e| {
                    ExtractionError::io_context(
                        e,
                        format!("Failed to extract deflated file: {}", file_path.as_ref()),
                    )
                })?;
            }
            _ => {
                eprintln!("Unsupported compression method {method:?} for file: {file_path:?}");
                continue;
            }
        }

        match entry.last_modified() {
            rawzip::time::ZipDateTimeKind::Utc(dt) => {
                let mtime = filetime::FileTime::from_unix_time(dt.to_unix(), dt.nanosecond());
                filetime::set_file_mtime(&out_path, mtime).map_err(|e| {
                    ExtractionError::io_context(
                        e,
                        format!(
                            "Failed to set file modification time for: {}",
                            out_path.display()
                        ),
                    )
                })?;
            }
            rawzip::time::ZipDateTimeKind::Local(dt) if dt.year() > 1980 => {
                // We only want to write out timestamps that are more recent
                // than 1980 (which is the start date for the msdos timestamp
                // format used in zip files).

                // Convert local time to UTC by treating it as it was UTC. This
                // is something you may (or may not) want to do too.
                let utc_time = rawzip::time::UtcDateTime::from_components(
                    dt.year(),
                    dt.month(),
                    dt.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                    dt.nanosecond(),
                );

                match utc_time {
                    Some(utc_time) => {
                        let mtime = filetime::FileTime::from_unix_time(
                            utc_time.to_unix(),
                            utc_time.nanosecond(),
                        );
                        filetime::set_file_mtime(&out_path, mtime).map_err(|e| {
                            ExtractionError::io_context(
                                e,
                                format!(
                                    "Failed to set file modification time for: {}",
                                    out_path.display()
                                ),
                            )
                        })?;
                    }
                    None => {
                        eprintln!("Invalid local time for file: {file_path:?}, skipping timestamp setting");
                    }
                }
            }

            _ => {}
        };

        // Set file attributes based on platform
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.mode();
            std::fs::set_permissions(
                &out_path,
                std::fs::Permissions::from_mode(mode.permissions()),
            )
            .map_err(|e| {
                ExtractionError::io_context(
                    e,
                    format!("Failed to set file permissions for: {}", out_path.display()),
                )
            })?;
        }

        #[cfg(windows)]
        {
            // Detect if the file should be marked as readonly
            if entry.mode().permissions() & 0o200 == 0 {
                let mut perms = std::fs::metadata(&out_path)
                    .map_err(|e| {
                        ExtractionError::io_context(
                            e,
                            format!("Failed to read file metadata for: {}", out_path.display()),
                        )
                    })?
                    .permissions();
                perms.set_readonly(true);
                std::fs::set_permissions(&out_path, perms).map_err(|e| {
                    ExtractionError::io_context(
                        e,
                        format!(
                            "Failed to set readonly attribute for: {}",
                            out_path.display()
                        ),
                    )
                })?;
            }
        }
    }

    if zip_start_offset > 0 {
        println!("ZIP starting offset: {}", zip_start_offset);
    }

    Ok(())
}

#[derive(Debug)]
enum ExtractionError {
    ZipError {
        error: rawzip::Error,
        context: String,
    },
    IoError {
        error: std::io::Error,
        context: String,
    },
}

impl std::fmt::Display for ExtractionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractionError::ZipError { error, context } => {
                write!(f, "{}: {}", context, error)
            }
            ExtractionError::IoError { error, context } => {
                write!(f, "{}: {}", context, error)
            }
        }
    }
}

impl std::error::Error for ExtractionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExtractionError::ZipError { error, .. } => Some(error),
            ExtractionError::IoError { error, .. } => Some(error),
        }
    }
}

impl ExtractionError {
    fn zip_context(error: rawzip::Error, context: String) -> Self {
        ExtractionError::ZipError { error, context }
    }

    fn io_context(error: std::io::Error, context: String) -> Self {
        ExtractionError::IoError { error, context }
    }
}
