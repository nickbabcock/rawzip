/// Specifies which ZIP headers to place data.
///
/// The ZIP specification allows for different data in local file headers versus
/// central directory headers. This type provides control over where data is
/// placed.
///
/// The default value is to place data in both header locations.
///
/// For usage example, see
/// [`ZipFileBuilder::extra_field`](crate::ZipFileBuilder::extra_field)
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header(u8);

#[cfg(feature = "std")]
impl Header {
    /// Include data only in the local file header.
    pub const LOCAL: Self = Self(0b01);

    /// Include data only in the central directory.
    pub const CENTRAL: Self = Self(0b10);

    #[inline]
    pub(crate) const fn new(value: u8) -> Self {
        Self(value)
    }

    #[inline]
    pub(crate) const fn includes_local(self) -> bool {
        self.0 & Self::LOCAL.0 != 0
    }

    #[inline]
    pub(crate) const fn includes_central(self) -> bool {
        self.0 & Self::CENTRAL.0 != 0
    }

    #[inline]
    pub(crate) const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

#[cfg(feature = "std")]
impl Default for Header {
    fn default() -> Self {
        Self(Self::LOCAL.0 | Self::CENTRAL.0)
    }
}

#[cfg(feature = "std")]
impl core::ops::BitOr for Header {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[cfg(feature = "std")]
impl core::ops::BitOrAssign for Header {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg(feature = "std")]
impl core::ops::BitAnd for Header {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

#[cfg(feature = "std")]
impl core::ops::BitAndAssign for Header {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// The general purpose bit flags of a ZIP entry.
///
/// (§ 4.4.4). Only the bits with a fixed, method-independent meaning are
/// surfaced as named accessors. Use [`EntryFlags::bits`] to inspect the raw
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryFlags(u16);

impl EntryFlags {
    const ENCRYPTED: u16 = 1 << 0;
    const DATA_DESCRIPTOR: u16 = 1 << 3;
    const STRONG_ENCRYPTION: u16 = 1 << 6;
    const LANGUAGE_ENCODING: u16 = 1 << 11;
    const MASKED: u16 = 1 << 13;

    #[inline]
    pub(crate) const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The raw 16-bit general purpose bit flag value.
    ///
    /// Use this to inspect the method-dependent bits 1 and 2, or any of the
    /// reserved bits not surfaced by the named accessors.
    #[inline]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Bit 0: the entry's data is encrypted.
    #[inline]
    pub const fn is_encrypted(self) -> bool {
        self.0 & Self::ENCRYPTED != 0
    }

    /// Bit 3: the crc-32, compressed size, and uncompressed size are zeroed in
    /// the local header, with the correct values stored in a data descriptor
    /// that follows the compressed data.
    #[inline]
    pub const fn has_data_descriptor(self) -> bool {
        self.0 & Self::DATA_DESCRIPTOR != 0
    }

    /// Bit 6: the entry uses strong encryption.
    #[inline]
    pub const fn has_strong_encryption(self) -> bool {
        self.0 & Self::STRONG_ENCRYPTION != 0
    }

    /// Bit 11 (EFS): the file name and comment are encoded as UTF-8.
    #[inline]
    pub const fn is_utf8(self) -> bool {
        self.0 & Self::LANGUAGE_ENCODING != 0
    }

    /// Bit 13: selected values in the local header are masked because the
    /// central directory is encrypted.
    #[inline]
    pub const fn is_masked(self) -> bool {
        self.0 & Self::MASKED != 0
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn test_header_bitflags_behavior() {
        // Test that default equals LOCAL | CENTRAL
        assert_eq!(Header::LOCAL | Header::CENTRAL, Header::default());

        // Test includes methods
        assert!(Header::LOCAL.includes_local());
        assert!(!Header::LOCAL.includes_central());

        assert!(!Header::CENTRAL.includes_local());
        assert!(Header::CENTRAL.includes_central());

        assert!(Header::default().includes_local());
        assert!(Header::default().includes_central());

        // Test bitwise operations
        let mut header = Header::LOCAL;
        header |= Header::CENTRAL;
        assert_eq!(header, Header::default());

        let intersection = Header::default() & Header::LOCAL;
        assert_eq!(intersection, Header::LOCAL);

        // Test intersects method
        assert!(Header::default().intersects(Header::LOCAL));
        assert!(Header::default().intersects(Header::CENTRAL));
        assert!(Header::LOCAL.intersects(Header::default()));
        assert!(!Header::LOCAL.intersects(Header::CENTRAL));
    }

    #[test]
    fn test_header_default() {
        assert_eq!(Header::default(), Header::LOCAL | Header::CENTRAL);
    }
}
