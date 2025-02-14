#![doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))]
#![forbid(unsafe_code)]

mod archive;
mod crc;
mod errors;
mod locator;
mod reader_at;
mod utils;
mod writer;

pub use archive::*;
pub use crc::crc32;
pub use errors::Error;
pub use locator::*;
pub use reader_at::{FileReader, ReaderAt};
pub use writer::*;
