//! Proves that rawzip reads a slice without panic paths.
//!
//! The panic handler calls a function that does not exist. If the optimizer
//! cannot remove a panic path, the link fails. Each feature adds one more step
//! of the slice reader, so that you can find the step that panics.
//!
//! To find the source of a panic path, define the missing function and
//! disassemble the binary:
//!
//! ```bash
//! RUSTFLAGS="-C link-arg=--defsym=rawzip_has_a_panic_path=0" \
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

#[cfg(feature = "locate")]
#[inline(never)]
fn locate(data: &[u8]) -> u64 {
    match rawzip::ZipArchive::from_slice(data) {
        Ok(archive) => archive.entries_hint(),
        Err(_) => u64::MAX,
    }
}

#[cfg(feature = "entries")]
#[inline(never)]
fn entries(data: &[u8]) -> u64 {
    let Ok(archive) = rawzip::ZipArchive::from_slice(data) else {
        return u64::MAX;
    };

    let mut total = 0u64;
    for entry in archive.entries() {
        let Ok(entry) = entry else {
            return u64::MAX;
        };
        total = total
            .wrapping_add(entry.file_path().as_ref().len() as u64)
            .wrapping_add(entry.compressed_size_hint())
            .wrapping_add(entry.local_header_offset());
    }
    total
}

#[cfg(feature = "data")]
#[inline(never)]
fn data(data: &[u8]) -> u64 {
    let Ok(archive) = rawzip::ZipArchive::from_slice(data) else {
        return u64::MAX;
    };

    let mut total = 0u64;
    for entry in archive.entries() {
        let Ok(entry) = entry else {
            return u64::MAX;
        };
        let Ok(entry) = archive.get_entry(entry.wayfinder()) else {
            return u64::MAX;
        };
        total = total.wrapping_add(entry.data().len() as u64);
    }
    total
}

/// Has a panic path, to show that the check can fail
#[cfg(feature = "canary")]
#[inline(never)]
fn canary(data: &[u8], i: usize) -> u8 {
    data[i]
}

/// Calls each step with input that the optimizer cannot see
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let input: &[u8] = black_box(&[]);

    #[cfg(feature = "locate")]
    black_box(locate(input));
    #[cfg(feature = "entries")]
    black_box(entries(input));
    #[cfg(feature = "data")]
    black_box(data(input));
    #[cfg(feature = "canary")]
    black_box(canary(input, black_box(0)));

    loop {}
}
