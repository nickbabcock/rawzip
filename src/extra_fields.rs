use crate::utils::le_u16;

/// A numeric identifier for an extra field in a Zip archive.
///
/// Constants defined here correspond to the IDs defined in the Zip specification.
///
/// See sections 4.5 and 4.6 of the Zip spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExtraFieldId(u16);

impl ExtraFieldId {
    pub const ZIP64: Self = Self(0x0001);
    pub const AV_INFO: Self = Self(0x0007);
    pub const EXTENDED_LANGUAGE_ENCODING: Self = Self(0x0008);
    pub const OS2: Self = Self(0x0009);
    pub const NTFS: Self = Self(0x000a);
    pub const OPENVMS: Self = Self(0x000c);
    pub const UNIX: Self = Self(0x000d);
    pub const FILE_STREAM_AND_FORK_DESCRIPTORS: Self = Self(0x000e);
    pub const PATCH_DESCRIPTOR: Self = Self(0x000f);
    pub const PKCS7_STORE: Self = Self(0x0014);
    pub const X509_CERT_ID_AND_SIG: Self = Self(0x0015);
    pub const X509_CERT_ID_CENTRAL_DIR: Self = Self(0x0016);
    pub const STRONG_ENCRYPTION_HEADER: Self = Self(0x0017);
    pub const RECORD_MANAGEMENT_CONTROLS: Self = Self(0x0018);
    pub const PKCS7_ENCRYPTION_RECIPIENT_CERT_LIST: Self = Self(0x0019);
    pub const TIMESTAMP_RECORD: Self = Self(0x0020);
    pub const POLICY_DECRYPTION_KEY_RECORD: Self = Self(0x0021);
    pub const SMARTCRYPT_KEY_PROVIDER: Self = Self(0x0022);
    pub const SMARTCRYPT_POLICY_KEY_DATA: Self = Self(0x0023);
    pub const IBM_S390_AS400_UNCOMPRESSED: Self = Self(0x0065);
    pub const IBM_S390_AS400_COMPRESSED: Self = Self(0x0066);
    pub const POSZIP_4690: Self = Self(0x4690);
    pub const EXTENDED_TIMESTAMP: Self = Self(0x5455);
    pub const INFO_ZIP_UNIX_ORIGINAL: Self = Self(0x5855);
    pub const INFO_ZIP_UNIX: Self = Self(0x7855);
    pub const INFO_ZIP_UNIX_UID_GID: Self = Self(0x7875);
    pub const JAVA_JAR: Self = Self(0xCAFE);
    pub const ANDROID_ZIP_ALIGNMENT: Self = Self(0xD935);
    pub const MACINTOSH: Self = Self(0x07c8);
    pub const ACORN_SPARKFS: Self = Self(0x4341);
    pub const WINDOWS_NT_SECURITY_DESCRIPTOR: Self = Self(0x4653);
    pub const AOS_VS_ACL: Self = Self(0x5356);
    pub const INFO_ZIP_UNICODE_COMMENT: Self = Self(0x6375);
    pub const INFO_ZIP_UNICODE_PATH: Self = Self(0x7075);
    pub const DATA_STREAM_ALIGNMENT: Self = Self(0xa11e);
    pub const MICROSOFT_OPEN_PACKAGING_GROWTH_HINT: Self = Self(0xa220);

    /// Returns the raw `u16` value of the extra field ID.
    #[inline]
    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

impl From<u16> for ExtraFieldId {
    fn from(id: u16) -> Self {
        Self(id)
    }
}

/// An iterator over extra field entries in a Zip archive.
///
/// This follows zip spec section 4.5 defines extensible data fields:
///
/// - Header ID - 2 bytes
/// - Data Size - 2 bytes
/// - Data - variable length
///
/// If the iterator encounters malformed or truncated data, it will stop
/// yielding entries. You can check [`ExtraFields::remaining_bytes()`] after
/// iteration to detect if any data was left unparsed.
#[derive(Debug, Clone)]
pub struct ExtraFields<'a> {
    data: &'a [u8],
}

impl<'a> ExtraFields<'a> {
    /// Creates a new iterator over the extra fields in the provided data slice.
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Returns the remaining unparsed bytes in the extra field data.
    #[inline]
    pub fn remaining_bytes(&self) -> &'a [u8] {
        self.data
    }
}

impl<'a> Iterator for ExtraFields<'a> {
    type Item = (ExtraFieldId, &'a [u8]);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let scratch = self.data;
        if scratch.len() < 4 {
            return None;
        }

        let (head, tail) = scratch.split_at(4);
        let kind = le_u16(&head[0..2]);
        let size = le_u16(&head[2..4]);

        if tail.len() < size as usize {
            return None;
        }

        let (body, rest) = tail.split_at(size as usize);

        // Only advance once we have the entire entry
        self.data = rest;

        Some((ExtraFieldId(kind), body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partial_parsing_with_remaining_bytes() {
        let data = [0x55, 0x54, 0x01, 0x00, 0xFF, 0x01, 0x00, 0x05];
        let mut iter = ExtraFields::new(&data);
        assert_eq!(iter.remaining_bytes(), &data);

        let (id, body) = iter.next().unwrap();
        assert_eq!(id, ExtraFieldId::EXTENDED_TIMESTAMP);
        assert_eq!(body, &[0xFF]);

        assert_eq!(iter.next(), None);
        assert_eq!(iter.remaining_bytes(), &[0x01, 0x00, 0x05]);
    }

    #[test]
    fn test_unknown_field_id() {
        let data = [0xFF, 0xFF, 0x02, 0x00, 0xDE, 0xAD];
        let mut iter = ExtraFields::new(&data);

        let (id, body) = iter.next().unwrap();
        assert_eq!(id, ExtraFieldId(0xFFFF));
        assert_eq!(body, &[0xDE, 0xAD]);

        assert_eq!(iter.next(), None);
    }
}
