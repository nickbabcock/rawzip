const fn gen_crc_table() -> [[u32; 256]; 16] {
    let mut table: [[u32; 256]; 16] = [[0; 256]; 16];
    let poly = 0xEDB88320; // Polynomial used in CRC-32

    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ poly;
            } else {
                crc >>= 1;
            }
            j += 1;
        }

        table[0][i] = crc;
        i += 1;
    }

    i = 1;
    while i < 16 {
        let mut j = 0;
        while j < 256 {
            table[i][j] = (table[i - 1][j] >> 8) ^ table[0][(table[i - 1][j] & 0xFF) as usize];
            j += 1;
        }
        i += 1;
    }

    table
}

// Prefer static over const to cut test times in half
// ref: https://github.com/srijs/rust-crc32fast/commit/e61ce6a39bbe9da495198a4037292ec299e8970f
static CRC_TABLE: [[u32; 256]; 16] = gen_crc_table();

/// Compute the CRC32 (IEEE) of a byte slice
///
/// Typically this function is used only to compute the CRC32 of data that is
/// held entirely in memory. When decompressing, a
/// [`ZipVerifier`](crate::ZipVerifier) is suitable to streaming computations.
///
/// While this crc implementation is the fastest known on Wasm, it falls a bit
/// short on native platforms. In a benchmark, using hardware intrinsics like
/// `PCLMULQDQ` saw 15 GB/s while the current slicing by 16 approach is "only" 5
/// GB/s.
///
/// Unfortunately, adopting `PCLMULQDQ` would require unsafe usage or a
/// dependency (`crc32fast`) as LLVM is unable to recognize the pattern.
///
/// The good news is that if a faster CRC algorithm is needed, then one can
/// always bring your own CRC implementation with
/// [`crate::ZipReader::claim_verifier`].
pub fn crc32(data: &[u8]) -> u32 {
    crc32_chunk(data, 0)
}

#[inline(always)]
fn crc32_slice16(data: &[u8], crc: u32) -> u32 {
    let w0 = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) ^ crc;
    CRC_TABLE[0x0][data[0xf] as usize]
        ^ CRC_TABLE[0x1][data[0xe] as usize]
        ^ CRC_TABLE[0x2][data[0xd] as usize]
        ^ CRC_TABLE[0x3][data[0xc] as usize]
        ^ CRC_TABLE[0x4][data[0xb] as usize]
        ^ CRC_TABLE[0x5][data[0xa] as usize]
        ^ CRC_TABLE[0x6][data[0x9] as usize]
        ^ CRC_TABLE[0x7][data[0x8] as usize]
        ^ CRC_TABLE[0x8][data[0x7] as usize]
        ^ CRC_TABLE[0x9][data[0x6] as usize]
        ^ CRC_TABLE[0xa][data[0x5] as usize]
        ^ CRC_TABLE[0xb][data[0x4] as usize]
        ^ CRC_TABLE[0xc][(w0 >> 24) as usize]
        ^ CRC_TABLE[0xd][((w0 >> 16) & 0xFF) as usize]
        ^ CRC_TABLE[0xe][((w0 >> 8) & 0xFF) as usize]
        ^ CRC_TABLE[0xf][(w0 & 0xFF) as usize]
}

#[inline]
pub fn crc32_chunk(data: &[u8], prev: u32) -> u32 {
    let mut chunks32 = data.chunks_exact(32);
    let mut crc = chunks32.by_ref().fold(!prev, |crc, data| {
        let crc = crc32_slice16(data, crc);
        crc32_slice16(&data[16..], crc)
    });

    let mut chunks16 = chunks32.remainder().chunks_exact(16);
    crc = chunks16
        .by_ref()
        .fold(crc, |crc, data| crc32_slice16(data, crc));
    crc = chunks16.remainder().iter().fold(crc, |crc, &x| {
        (crc >> 8) ^ CRC_TABLE[0][(u32::from(x) ^ (crc & 0xFF)) as usize]
    });

    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc() {
        let table = gen_crc_table();
        assert_eq!(table[0][0], 0x0000_0000);
        assert_eq!(table[0][1], 0x77073096);
        assert_eq!(table[0][2], 0xee0e612c);
        assert_eq!(table[1][1], 0x191B3141);
        assert_eq!(table[1][2], 0x32366282);

        let abc = b"EU4txt\nchecksum=\"ced5411e2d4a5ec724595c2c4f1b7347\"";
        assert_eq!(crc32(abc), 1702863696);
    }

    fn reference_crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc = (crc >> 8) ^ CRC_TABLE[0][((crc ^ u32::from(b)) & 0xFF) as usize];
        }
        !crc
    }

    #[test]
    fn test_crc_sizes() {
        let data: Vec<u8> = (0u8..=255).cycle().take(65536).collect();
        for &size in &[
            0, 1, 4, 15, 16, 17, 31, 32, 33, 64, 256, 1024, 4096, 16384, 65536,
        ] {
            let slice = &data[..size];
            assert_eq!(
                crc32(slice),
                reference_crc32(slice),
                "mismatch at size {size}"
            );
        }
    }

    #[test]
    fn test_crc_chunk_streaming() {
        let data: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let full = crc32(&data);
        let half = crc32_chunk(&data[..2048], 0);
        let streamed = crc32_chunk(&data[2048..], half);
        assert_eq!(full, streamed);
    }
}
