/// An error that occurred while reading or writing a zip file
#[derive(Debug)]
pub struct Error {
    inner: Box<ErrorInner>,
}

impl Error {
    pub(crate) fn io(err: std::io::Error) -> Error {
        Error::from(ErrorKind::IO(err))
    }

    pub(crate) fn utf8(err: std::str::Utf8Error) -> Error {
        Error::from(ErrorKind::InvalidUtf8(err))
    }

    pub(crate) fn is_eof(&self) -> bool {
        matches!(self.inner.kind, ErrorKind::Eof)
    }

    /// The kind of error that occurred
    pub fn kind(&self) -> &ErrorKind {
        &self.inner.kind
    }
}

#[derive(Debug)]
struct ErrorInner {
    kind: ErrorKind,
}

/// The kind of error that occurred
#[derive(Debug)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Missing end of central directory
    MissingEndOfCentralDirectory,

    /// Missing zip64 end of central directory
    MissingZip64EndOfCentralDirectory,

    /// Buffer size too small
    BufferTooSmall,

    /// Invalid end of central directory signature
    InvalidSignature { expected: u32, actual: u32 },

    /// Invalid inflated file crc checksum
    InvalidChecksum { expected: u32, actual: u32 },

    /// An unexpected inflated file size
    InvalidSize { expected: u64, actual: u64 },

    /// Invalid UTF-8 sequence
    InvalidUtf8(std::str::Utf8Error),

    /// An invalid input error with associated message
    InvalidInput { msg: String },

    /// An IO error
    IO(std::io::Error),

    /// An unexpected end of file
    Eof,
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.inner.kind)?;
        Ok(())
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            ErrorKind::IO(ref err) => err.fmt(f),
            ErrorKind::MissingEndOfCentralDirectory => {
                write!(f, "Missing end of central directory")
            }
            ErrorKind::MissingZip64EndOfCentralDirectory => {
                write!(f, "Missing zip64 end of central directory")
            }
            ErrorKind::BufferTooSmall => {
                write!(f, "Buffer size too small")
            }
            ErrorKind::Eof => {
                write!(f, "Unexpected end of file")
            }
            ErrorKind::InvalidSignature { expected, actual } => {
                write!(
                    f,
                    "Invalid signature: expected 0x{:08x}, got 0x{:08x}",
                    expected, actual
                )
            }
            ErrorKind::InvalidChecksum { expected, actual } => {
                write!(
                    f,
                    "Invalid checksum: expected 0x{:08x}, got 0x{:08x}",
                    expected, actual
                )
            }
            ErrorKind::InvalidSize { expected, actual } => {
                write!(f, "Invalid size: expected {}, got {}", expected, actual)
            }
            ErrorKind::InvalidUtf8(ref err) => {
                write!(f, "Invalid UTF-8: {}", err)
            }
            ErrorKind::InvalidInput { ref msg } => {
                write!(f, "Invalid input: {}", msg)
            }
        }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Error {
        Error {
            inner: Box::new(ErrorInner { kind }),
        }
    }
}

impl From<std::io::Error> for ErrorKind {
    fn from(err: std::io::Error) -> ErrorKind {
        ErrorKind::IO(err)
    }
}
