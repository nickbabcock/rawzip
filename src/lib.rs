#![cfg_attr(
    feature = "std",
    doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
)]
#![cfg_attr(
    not(feature = "std"),
    doc = "A low-level ZIP archive parser. This feature set exposes slice-based reading and core ZIP metadata types. Enable the `std` feature for reader-backed archives and writing."
)]
#![forbid(unsafe_code)]
#![cfg_attr(not(any(feature = "std", test)), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod archive;
mod crc;
mod errors;
pub mod extra_fields;
mod headers;
mod locator;
mod mode;
pub mod path;
#[cfg(feature = "std")]
mod reader_at;
pub mod time;
mod utils;
#[cfg(feature = "std")]
mod writer;
#[cfg(feature = "std")]
pub mod zipcrypto;

pub use archive::*;
pub use crc::{Crc32, crc32};
pub use errors::{Error, ErrorKind};
pub use headers::EntryFlags;
#[cfg(feature = "std")]
pub use headers::Header;
pub use locator::*;
pub use mode::{CreatorSystem, EntryMode, VersionMadeBy};
#[cfg(feature = "alloc")]
pub use path::EntryPath;
#[cfg(feature = "std")]
pub use reader_at::{FileReader, RangeReader, ReaderAt};
#[cfg(feature = "std")]
pub use writer::*;
