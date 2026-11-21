fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let Some(method) = args.get(1) else {
        eprintln!("Usage: {} <method> <file>", args[0]);
        std::process::exit(1);
    };

    let Some(path) = args.get(2) else {
        eprintln!("Usage: {} <file>", args[0]);
        std::process::exit(1);
    };

    match method.as_str() {
        "raw" => {
            let now = std::time::Instant::now();
            let result = rawzip_impl(path).unwrap();
            println!("rawzip: {:?} {}", now.elapsed(), result);
        }
        "rc" => {
            let now = std::time::Instant::now();
            let result = rc_zip(path).unwrap();
            println!("rc_zip: {:?} {}", now.elapsed(), result);
        }
        "zip" => {
            let now = std::time::Instant::now();
            let result = zip(path).unwrap();
            println!("zip: {:?} {}", now.elapsed(), result);
        }
        "eazip" => {
            let now = std::time::Instant::now();
            let result = eazip(path).unwrap();
            println!("eazip: {:?} {}", now.elapsed(), result);
        }
        "async" => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let now = std::time::Instant::now();
            let result = rt.block_on(async_zip(path)).unwrap();
            println!("async_zip: {:?} {}", now.elapsed(), result);
        }
        _ => {
            eprintln!("Invalid method: {}", method);
            std::process::exit(1);
        }
    }
}

fn rawzip_impl<P: AsRef<std::path::Path>>(fp: P) -> anyhow::Result<u64> {
    let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let file = std::fs::File::open(fp)?;
    let archive = rawzip::ZipArchive::from_file(file, &mut buffer)?;
    let mut entries = archive.entries(&mut buffer);

    let mut result = 0;
    while let Some(entry) = entries.next_entry()? {
        result += entry.uncompressed_size_hint();
    }

    Ok(result)
}

fn rc_zip<P: AsRef<std::path::Path>>(fp: P) -> anyhow::Result<u64> {
    use rc_zip_sync::ReadZip;

    let file = std::fs::File::open(fp)?;
    let reader = file.read_zip()?;

    let result = reader.entries().map(|x| x.uncompressed_size).sum::<u64>();
    Ok(result)
}

fn zip<P: AsRef<std::path::Path>>(fp: P) -> anyhow::Result<u64> {
    use zip::ZipArchive;

    let file = std::fs::File::open(fp)?;
    let mut archive = ZipArchive::new(file)?;
    let entries = archive.len();
    let mut result = 0;
    for ind in 0..entries {
        let entry = archive.by_index_raw(ind)?;
        result += entry.size();
    }

    Ok(result)
}

fn eazip<P: AsRef<std::path::Path>>(fp: P) -> anyhow::Result<u64> {
    let archive = eazip::ArchiveReader::open(fp)?;
    let result = archive
        .entries()
        .iter()
        .map(|x| x.uncompressed_size)
        .sum::<u64>();
    Ok(result)
}

async fn async_zip<P: AsRef<std::path::Path>>(fp: P) -> anyhow::Result<u64> {
    use async_zip::base::read::seek::ZipFileReader;
    use tokio::{fs::File, io::BufReader};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    let data = BufReader::new(File::open(fp).await?);
    let reader = ZipFileReader::new(data.compat()).await?;
    let result = reader
        .file()
        .entries()
        .iter()
        .map(|x| x.uncompressed_size())
        .sum::<u64>();
    Ok(result)
}
