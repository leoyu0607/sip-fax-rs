//! Error type shared by the decoder, encoder and writers.

use std::fmt;

/// Errors from configuration, decoding, encoding and file writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The line width is zero or above [`crate::MAX_WIDTH`].
    InvalidWidth(u32),
    /// The MR `k` parameter is below 2.
    InvalidK(u8),
    /// A row passed to the encoder has the wrong length.
    RowSize {
        /// Bytes required per row.
        expected: usize,
        /// Bytes supplied.
        got: usize,
    },
    /// The data contains a bit pattern that is not a valid code word.
    InvalidCode,
    /// A row decoded to more pixels than the line width, or did not fit the reference line.
    LineOverrun,
    /// The data ended in the middle of a row.
    Truncated,
    /// A valid but unsupported feature (the T.6 extension / uncompressed mode).
    Unsupported,
    /// A row needed more changing elements than the line width allows.
    TooManyChanges,
    /// A page has an unusable size or resolution, or its G4 data is empty.
    InvalidPage(&'static str),
    /// Metadata cannot be represented (for example a malformed date).
    InvalidMetadata(&'static str),
    /// A file would exceed the 4 GiB offsets of classic TIFF / PDF.
    TooLarge,
    /// There are no pages to write.
    NoPages,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidWidth(w) => write!(f, "invalid line width {w}"),
            Error::InvalidK(k) => write!(f, "invalid MR k parameter {k}"),
            Error::RowSize { expected, got } => {
                write!(f, "row has {got} bytes, expected {expected}")
            }
            Error::InvalidCode => write!(f, "invalid code word"),
            Error::LineOverrun => write!(f, "line is longer than the page width"),
            Error::Truncated => write!(f, "data ended inside a row"),
            Error::Unsupported => write!(f, "unsupported coding extension"),
            Error::TooManyChanges => write!(f, "too many changing elements in a row"),
            Error::InvalidPage(why) => write!(f, "invalid page: {why}"),
            Error::InvalidMetadata(why) => write!(f, "invalid metadata: {why}"),
            Error::TooLarge => write!(f, "output exceeds 4 GiB"),
            Error::NoPages => write!(f, "no pages to write"),
        }
    }
}

impl std::error::Error for Error {}
