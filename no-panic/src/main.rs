//! Proves that rawzip reads a slice without panic paths.
//!
//! The panic handler calls a function that does not exist. If the optimizer
//! cannot remove a panic path, the link fails.
//!
//! To find the source of a panic path, define the missing function and
//! disassemble the binary:
//!
//! ```bash
//! RUSTFLAGS="-C link-arg=--defsym=rawzip_has_a_panic_path=0 -C debuginfo=2" \
//!   cargo build --release --target x86_64-unknown-none
//! llvm-objdump -d --demangle target/x86_64-unknown-none/release/rawzip-no-panic
//! ```
//!
//! Then give the address of each jump to a panic function to
//! `addr2line -f -i -C -e <binary>`.
#![no_std]
#![no_main]

use core::hint::black_box;
use core::panic::PanicInfo;

unsafe extern "C" {
    fn rawzip_has_a_panic_path() -> !;
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    unsafe { rawzip_has_a_panic_path() }
}

/// Locates the archive, iterates its central directory, and reads each entry
fn read(data: &[u8]) -> Option<u64> {
    let archive = rawzip::ZipArchive::from_slice(data).ok()?;
    let mut total = archive.entries_hint();
    for entry in archive.entries() {
        let entry = entry.ok()?;
        total = total
            .wrapping_add(entry.file_path().as_ref().len() as u64)
            .wrapping_add(entry.compressed_size_hint())
            .wrapping_add(entry.local_header_offset());
        let entry = archive.get_entry(entry.wayfinder()).ok()?;
        total = total.wrapping_add(entry.data().len() as u64);
    }
    Some(total)
}

/// Reads input that the optimizer cannot see
#[unsafe(no_mangle)]
#[allow(clippy::empty_loop, reason = "the binary is linked, never run")]
pub extern "C" fn _start() -> ! {
    let input: &[u8] = black_box(&[]);
    black_box(read(input));
    loop {}
}
