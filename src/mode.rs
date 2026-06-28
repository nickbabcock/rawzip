/// The host system that produced a zip entry.
///
/// Derived from central directory "version made by" (APPNOTE § 4.4.2.2)
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CreatorSystem(u8);

impl CreatorSystem {
    pub const FAT: Self = Self(0);
    pub const UNIX: Self = Self(3);
    pub const NTFS: Self = Self(10);

    /// Go and Info-ZIP calls this NTFS
    pub const MVS: Self = Self(11);
    pub const VFAT: Self = Self(14);
    pub const MACOS: Self = Self(19);

    /// Wraps a raw creator-system identifier.
    #[inline]
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// Returns the raw creator-system identifier.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self.0
    }

    /// Returns the creator-system name (e.g. `"UNIX"`) when known.
    #[inline]
    pub const fn name(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("FAT"),
            3 => Some("UNIX"),
            10 => Some("NTFS"),
            11 => Some("MVS"),
            14 => Some("VFAT"),
            19 => Some("MACOS"),
            _ => None,
        }
    }
}

impl core::fmt::Debug for CreatorSystem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.name() {
            Some(name) => write!(f, "CreatorSystem::{name}"),
            None => write!(f, "CreatorSystem({})", self.0),
        }
    }
}

impl core::fmt::Display for CreatorSystem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.0, self.name().unwrap_or("UNKNOWN"))
    }
}

impl From<u8> for CreatorSystem {
    fn from(id: u8) -> Self {
        Self(id)
    }
}

/// The "version made by" field from a central directory record
///
/// (APPNOTE § 4.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VersionMadeBy(u16);

impl VersionMadeBy {
    /// Builds a value from a creator system and a ZIP specification version (in
    /// tenths, e.g. `20` for version 2.0).
    #[inline]
    pub(crate) const fn new(system: CreatorSystem, zip_version: u8) -> Self {
        Self(((system.as_u8() as u16) << 8) | zip_version as u16)
    }

    /// Wraps a raw "version made by" value.
    #[inline]
    pub const fn from_raw(value: u16) -> Self {
        Self(value)
    }

    /// The host system that created the entry (the high byte).
    #[inline]
    pub const fn creator_system(self) -> CreatorSystem {
        CreatorSystem((self.0 >> 8) as u8)
    }

    /// The ZIP specification version supported by the software that created the
    /// entry.
    ///
    /// The value is encoded in tenths (APPNOTE § 4.4.2.3). For example, `20`
    /// means version 2.0 and `45` means 4.5.
    #[inline]
    pub const fn zip_version(self) -> u8 {
        self.0 as u8
    }

    /// Returns the raw "version made by" value.
    #[inline]
    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

/// File mode information for a given zip file entry.
///
/// This represents Unix-style file permissions and type information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMode(u32);

impl EntryMode {
    /// Creates a new Mode from a raw mode value.
    #[must_use]
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw mode value
    #[must_use]
    pub const fn value(&self) -> u32 {
        self.0
    }

    /// Returns true if this is a symbolic link.
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        self.0 & S_IFMT == S_IFLNK
    }

    /// Returns the Unix permission bits (e.g., 0o755).
    #[must_use]
    pub const fn permissions(&self) -> u32 {
        self.0 & 0o777
    }
}

/// Unix file type and permission constants
const S_IFMT: u32 = 0o170000; // File type mask
const S_IFSOCK: u32 = 0o140000; // Socket
const S_IFLNK: u32 = 0o120000; // Symbolic link
const S_IFREG: u32 = 0o100000; // Regular file
const S_IFBLK: u32 = 0o060000; // Block device
const S_IFDIR: u32 = 0o040000; // Directory
const S_IFCHR: u32 = 0o020000; // Character device
const S_IFIFO: u32 = 0o010000; // FIFO
const S_ISUID: u32 = 0o004000; // Set user ID
const S_ISGID: u32 = 0o002000; // Set group ID
const S_ISVTX: u32 = 0o001000; // Sticky bit

/// MSDOS file attribute constants
const MSDOS_DIR: u32 = 0x10;
const MSDOS_READONLY: u32 = 0x01;

/// Converts Unix mode to file mode
pub(crate) fn unix_mode_to_file_mode(m: u32) -> u32 {
    let mut mode = m & 0o777; // Basic permissions

    // Set file type bits based on Unix mode
    match m & S_IFMT {
        S_IFBLK => mode |= S_IFBLK,
        S_IFCHR => mode |= S_IFCHR,
        S_IFDIR => mode |= S_IFDIR,
        S_IFIFO => mode |= S_IFIFO,
        S_IFLNK => mode |= S_IFLNK,
        S_IFSOCK => mode |= S_IFSOCK,
        _ => mode |= S_IFREG, // Default to regular file
    }

    // Set special permission bits
    if m & S_ISGID != 0 {
        mode |= S_ISGID;
    }
    if m & S_ISUID != 0 {
        mode |= S_ISUID;
    }
    if m & S_ISVTX != 0 {
        mode |= S_ISVTX;
    }

    mode
}

/// Converts MSDOS attributes to file mode, following Go's zip reader logic
pub(crate) fn msdos_mode_to_file_mode(m: u32) -> u32 {
    if m & MSDOS_DIR != 0 {
        S_IFDIR | 0o777
    } else if m & MSDOS_READONLY != 0 {
        S_IFREG | 0o444
    } else {
        S_IFREG | 0o666
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::format;

    #[test]
    fn creator_system_known_names() {
        assert_eq!(CreatorSystem::FAT.name(), Some("FAT"));
        assert_eq!(CreatorSystem::UNIX.name(), Some("UNIX"));
        assert_eq!(CreatorSystem::NTFS.name(), Some("NTFS"));
        assert_eq!(CreatorSystem::MVS.name(), Some("MVS"));
        assert_eq!(CreatorSystem::VFAT.name(), Some("VFAT"));
        assert_eq!(CreatorSystem::MACOS.name(), Some("MACOS"));
        assert_eq!(CreatorSystem::new(7).name(), None);
        assert_eq!(CreatorSystem::NTFS.as_u8(), 10);
        assert_eq!(CreatorSystem::MVS.as_u8(), 11);
    }

    #[test]
    fn creator_system_raw_roundtrip() {
        for id in [0u8, 3, 10, 11, 14, 19, 7, 255] {
            assert_eq!(CreatorSystem::new(id).as_u8(), id);
            assert_eq!(CreatorSystem::from(id), CreatorSystem::new(id));
        }
    }

    #[test]
    fn creator_system_debug_and_display() {
        assert_eq!(format!("{:?}", CreatorSystem::UNIX), "CreatorSystem::UNIX");
        assert_eq!(format!("{:?}", CreatorSystem::new(7)), "CreatorSystem(7)");
        assert_eq!(format!("{}", CreatorSystem::UNIX), "3 (UNIX)");
        assert_eq!(format!("{}", CreatorSystem::new(7)), "7 (UNKNOWN)");
    }

    #[test]
    fn version_made_by_decomposition() {
        // High byte = creator system, low byte = zip version (tenths).
        let v = VersionMadeBy::from_raw(0x031e);
        assert_eq!(v.creator_system(), CreatorSystem::UNIX);
        assert_eq!(v.zip_version(), 30);
        assert_eq!(v.as_u16(), 0x031e);
    }

    #[test]
    fn version_made_by_new_matches_raw() {
        let v = VersionMadeBy::new(CreatorSystem::MACOS, 20);
        assert_eq!(v.as_u16(), (19 << 8) | 20);
        assert_eq!(v.creator_system(), CreatorSystem::MACOS);
        assert_eq!(v.zip_version(), 20);
    }
}
