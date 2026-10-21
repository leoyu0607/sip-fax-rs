//! T.4/T.6 (MH/MR/MMR) fax image coding, TIFF Class F writer and PDF output.
//!
//! * [`Decoder`]: streaming MH / MR / MMR decoder with non-ECM error handling and statistics.
//! * [`Encoder`]: MH / MR / MMR encoder (MMR is what the server stores).
//! * [`write_tiff`]: multi-page TIFF Class F (CCITT G4) writer.
//! * [`TiffReader`]: reads TIFF Class F files (G3 / G4 / uncompressed) back into bitmaps.
//! * [`png`]: 1-bit full page and 300 px thumbnail PNG output for the web preview.
//! * [`write_pdf`]: minimal PDF writer embedding G4 data with `CCITTFaxDecode`.
//!
//! Pixel rows are packed 1-bit, most significant bit first, **1 = black**, `ceil(width / 8)`
//! bytes per row (padding bits are 0).
//!
//! The crate is pure Rust and performs no I/O. Input is untrusted: nothing here panics on
//! arbitrary bytes and the decoder enforces hard limits on rows and buffered data.

#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

mod bitio;
mod changes;
mod decoder;
mod encoder;
mod error;
mod pdf;
pub mod png;
mod tables;
mod tiff;
mod tiff_read;

pub use decoder::{
    DEFAULT_MAX_ROWS, Decoder, DecoderConfig, EndReason, PageData, Stats, Summary, decode_page,
};
pub use encoder::{Encoder, encode_mmr};
pub use error::Error;
pub use pdf::write_pdf;
pub use tiff::{Page, TiffMeta, format_tiff_datetime, write_tiff};
pub use tiff_read::{Bitmap, TiffCoding, TiffPageInfo, TiffReader};

/// Widest supported line in pixels (the fax widths are 1728, 2048 and 2432).
pub const MAX_WIDTH: u32 = 8192;

/// Compression scheme of a T.4/T.6 data stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coding {
    /// T.4 one-dimensional (Modified Huffman) with EOL.
    Mh,
    /// T.4 two-dimensional (Modified READ): every `k`-th row is one-dimensional. The decoder
    /// follows the tag bit after each EOL and ignores `k`; the encoder uses it.
    Mr {
        /// Maximum run of two-dimensionally coded rows plus one (2 at standard, 4 at fine).
        k: u8,
    },
    /// T.6 two-dimensional (Modified Modified READ), no EOLs, ends with EOFB.
    Mmr,
}

/// Number of bytes in one packed row.
pub fn row_bytes(width: u32) -> usize {
    (width as usize).div_ceil(8)
}
