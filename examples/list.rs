use rawzip::{ZipArchive, RECOMMENDED_BUFFER_SIZE};
use std::env;
use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        eprintln!("Usage: {} <archive.zip>", args[0]);
        eprintln!("List the contents of a ZIP archive");
        std::process::exit(1);
    }

    let archive_path = &args[1];
    let file = File::open(archive_path)?;
    let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buffer)?;

    println!("Archive:  {}", archive_path);

    let mut comment_reader = archive.comment();
    if comment_reader.remaining() > 0 {
        print!("Comment: ");
        std::io::copy(&mut comment_reader, &mut std::io::stdout().lock())?;
        println!();
    }

    println!();
    println!("   Length  Date/Time             Perms       Name");
    println!("---------  --------------------  ----------  -------");

    let mut total_uncompressed = 0u64;
    let mut total_compressed = 0u64;
    let mut file_count = 0u64;

    let mut entries = archive.entries(&mut buffer);
    while let Some(entry) = entries.next_entry()? {
        let uncompressed_size = entry.uncompressed_size_hint();
        let compressed_size = entry.compressed_size_hint();

        total_uncompressed += uncompressed_size;
        total_compressed += compressed_size;
        file_count += 1;

        // Format permissions
        let mode = entry.mode();
        let permissions_str = format_permissions(mode.value());

        // Show uncompressed size, or empty for directories
        let size_str = if entry.is_dir() {
            format!("{:9}", "")
        } else {
            format!("{:9}", uncompressed_size)
        };

        print!(
            "{}  {:20}  {:10}  ",
            size_str,
            entry.last_modified(),
            permissions_str
        );
        std::io::stdout().write_all(entry.file_path().as_ref())?;
        println!();
    }

    println!("---------  --------------------  ----------  -------");
    println!(
        "{:9}                                             {} files",
        total_uncompressed, file_count
    );

    if total_compressed > 0 && total_uncompressed > 0 {
        let compression_ratio = (total_compressed as f64 / total_uncompressed as f64) * 100.0;
        println!(
            "Compressed size: {} bytes ({:.1}%)",
            total_compressed, compression_ratio
        );
    }

    Ok(())
}

fn format_permissions(mode: u32) -> String {
    let file_type = match mode & 0o170000 {
        0o040000 => 'd', // Directory
        0o120000 => 'l', // Symbolic link
        0o100000 => '-', // Regular file
        0o060000 => 'b', // Block device
        0o020000 => 'c', // Character device
        0o010000 => 'p', // FIFO
        0o140000 => 's', // Socket
        _ => '?',        // Unknown
    };

    let owner = format!(
        "{}{}{}",
        if mode & 0o400 != 0 { 'r' } else { '-' },
        if mode & 0o200 != 0 { 'w' } else { '-' },
        if mode & 0o100 != 0 { 'x' } else { '-' }
    );

    let group = format!(
        "{}{}{}",
        if mode & 0o040 != 0 { 'r' } else { '-' },
        if mode & 0o020 != 0 { 'w' } else { '-' },
        if mode & 0o010 != 0 { 'x' } else { '-' }
    );

    let other = format!(
        "{}{}{}",
        if mode & 0o004 != 0 { 'r' } else { '-' },
        if mode & 0o002 != 0 { 'w' } else { '-' },
        if mode & 0o001 != 0 { 'x' } else { '-' }
    );

    format!("{}{}{}{}", file_type, owner, group, other)
}
