use rawzip::ZipArchiveWriter;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::UNIX_EPOCH;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Parse flags and arguments
    let mut use_zstd = false;
    let mut positional_args = Vec::new();

    for arg in &args[1..] {
        if arg == "--zstd" {
            use_zstd = true;
        } else {
            positional_args.push(arg.as_str());
        }
    }

    if positional_args.len() < 2 {
        eprintln!("Usage: {} [--zstd] <output.zip> <input_path>...", args[0]);
        eprintln!("Create a ZIP archive from the specified files and directories");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --zstd    Use zstd compression (level 3) instead of deflate");
        std::process::exit(1);
    }

    let compression_method = if use_zstd {
        rawzip::CompressionMethod::Zstd
    } else {
        rawzip::CompressionMethod::Deflate
    };

    let output_path = positional_args[0];
    let input_paths = &positional_args[1..];

    let output_file = File::create(output_path)?;
    let writer = std::io::BufWriter::new(output_file);
    let mut archive = ZipArchiveWriter::new(writer);

    for input_path in input_paths {
        let path = Path::new(input_path);
        if path.is_file() {
            add_file_to_archive(
                &mut archive,
                path,
                path.file_name().unwrap().to_str().unwrap(),
                compression_method,
            )?;
        } else if path.is_dir() {
            add_directory_to_archive(&mut archive, path, "", compression_method)?;
        } else {
            eprintln!(
                "Warning: '{}' does not exist or is not a regular file/directory",
                input_path
            );
        }
    }

    archive.finish()?;
    println!("Successfully created '{}'", output_path);
    Ok(())
}

fn get_modification_time(
    metadata: &fs::Metadata,
) -> Result<rawzip::time::UtcDateTime, Box<dyn std::error::Error>> {
    let modified = metadata.modified()?;

    // Convert system time to UTC DateTime
    let unix_seconds = modified.duration_since(UNIX_EPOCH)?.as_secs() as i64;
    Ok(rawzip::time::UtcDateTime::from_unix(unix_seconds))
}

fn add_file_to_archive<W: Write>(
    archive: &mut ZipArchiveWriter<W>,
    file_path: &Path,
    archive_path: &str,
    compression_method: rawzip::CompressionMethod,
) -> Result<(), Box<dyn std::error::Error>> {
    let metadata = fs::metadata(file_path)?;
    let modification_time = get_modification_time(&metadata)?;

    let mut builder = archive
        .new_file(archive_path)
        .compression_method(compression_method)
        .last_modified(modification_time);

    if let Some(permissions) = get_unix_permissions(&metadata) {
        builder = builder.unix_permissions(permissions);
    }

    // Read and compress the file content
    let mut file = fs::File::open(file_path)?;
    let (mut entry, config) = builder.start()?;
    match compression_method {
        rawzip::CompressionMethod::Deflate => {
            let encoder =
                flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
            let mut writer = config.wrap(encoder);
            std::io::copy(&mut file, &mut writer)?;
            let (encoder, output) = writer.finish()?;
            encoder.finish()?;
            entry.finish(output)?;
        }
        rawzip::CompressionMethod::Zstd => {
            let encoder = zstd::Encoder::new(&mut entry, 3)?;
            let mut writer = config.wrap(encoder);
            std::io::copy(&mut file, &mut writer)?;
            let (encoder, output) = writer.finish()?;
            encoder.finish()?;
            entry.finish(output)?;
        }
        _ => {
            return Err("Unsupported compression method".into());
        }
    }

    println!("  adding: {}", archive_path);
    Ok(())
}

fn add_directory_to_archive<W: Write>(
    archive: &mut ZipArchiveWriter<W>,
    dir_path: &Path,
    base_path: &str,
    compression_method: rawzip::CompressionMethod,
) -> Result<(), Box<dyn std::error::Error>> {
    let entries = fs::read_dir(dir_path)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_str().unwrap();

        let archive_path = if base_path.is_empty() {
            name_str.to_string()
        } else {
            format!("{}/{}", base_path, name_str)
        };

        if path.is_file() {
            add_file_to_archive(archive, &path, &archive_path, compression_method)?;
        } else if path.is_dir() {
            // Add directory entry
            let metadata = fs::metadata(&path)?;
            let modification_time = get_modification_time(&metadata)?;

            let dir_archive_path = format!("{}/", archive_path);
            let mut builder = archive
                .new_dir(&dir_archive_path)
                .last_modified(modification_time);

            if let Some(permissions) = get_unix_permissions(&metadata) {
                builder = builder.unix_permissions(permissions);
            }

            builder.create()?;
            println!("  adding: {}", dir_archive_path);

            // Recursively add directory contents
            add_directory_to_archive(archive, &path, &archive_path, compression_method)?;
        }
    }

    Ok(())
}

#[cfg(unix)]
fn get_unix_permissions(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn get_unix_permissions(_metadata: &fs::Metadata) -> Option<u32> {
    None
}
