//! Tiny multi-page uncompressed bilevel TIFF writer (little endian, 1 strip/page).
//!
//! Deliberately shares no code with [`crate::tiff_reader`], so the reader acts as
//! an independent check on this writer.

use std::io;
use std::path::Path;

/// Fax line width in pixels (A4 at 204 dpi).
pub const FAX_WIDTH: u32 = 1728;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSpec {
    pub width: u32,
    pub height: u32,
    pub x_dpi: u32,
    pub y_dpi: u32,
}

impl PageSpec {
    /// Standard-resolution fax page (204 x 98 dpi).
    pub fn standard(height: u32) -> Self {
        Self {
            width: FAX_WIDTH,
            height,
            x_dpi: 204,
            y_dpi: 98,
        }
    }
    /// Fine-resolution fax page (204 x 196 dpi).
    pub fn fine(height: u32) -> Self {
        Self {
            width: FAX_WIDTH,
            height,
            x_dpi: 204,
            y_dpi: 196,
        }
    }
}

/// Deterministic text-like pixel pattern; 1 bit = black, MSB first. This is exactly what
/// [`encode`] stores for page `page_idx`, so tests use it as the reference bitmap.
pub fn render_bitmap(spec: &PageSpec, page_idx: u32) -> Vec<u8> {
    render_page(spec, page_idx)
}

fn render_page(spec: &PageSpec, page_idx: u32) -> Vec<u8> {
    let row_bytes = spec.width.div_ceil(8) as usize;
    let mut data = vec![0u8; row_bytes * spec.height as usize];
    for y in 0..spec.height {
        let row = &mut data[y as usize * row_bytes..][..row_bytes];
        let border = y < 4 || y >= spec.height.saturating_sub(4);
        // "text lines": 8 pixel rows of dashes every 24 rows, pattern shifts per page
        let text_row = (y % 24) < 8 && y > 16;
        for (i, b) in row.iter_mut().enumerate() {
            let x = i as u32 * 8;
            *b = if border || i == 0 || i == row_bytes - 1 {
                0xFF
            } else if text_row && ((x / 40 + page_idx) % 3 != 0) {
                0xF0 >> ((x / 8 + page_idx) % 4)
            } else {
                0x00
            };
        }
    }
    data
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// IFD entry: tag, type (3 = SHORT, 4 = LONG, 5 = RATIONAL), count, value/offset.
fn entry(out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32) {
    le16(out, tag);
    le16(out, typ);
    le32(out, count);
    // SHORT values (count 1) are left-justified in the 4-byte field; LE makes
    // that identical to writing the u32.
    le32(out, value);
}

const N_ENTRIES: u16 = 15;
const IFD_LEN: u32 = 2 + N_ENTRIES as u32 * 12 + 4;

/// Encode pages as one TIFF file.
pub fn encode(pages: &[PageSpec]) -> Vec<u8> {
    assert!(!pages.is_empty(), "at least one page required");
    let mut out = Vec::new();
    out.extend_from_slice(b"II");
    le16(&mut out, 42);
    le32(&mut out, 0); // patched below with first IFD offset

    // page block layout: [pixel data][2 rationals = 16 bytes][IFD]
    let mut ifd_offsets = Vec::new();
    let mut pos = 8u32;
    for p in pages {
        let data_len = p.width.div_ceil(8) * p.height;
        assert!(data_len % 2 == 0, "page data must be word aligned");
        ifd_offsets.push(pos + data_len + 16);
        pos += data_len + 16 + IFD_LEN;
    }
    out[4..8].copy_from_slice(&ifd_offsets[0].to_le_bytes());

    for (i, p) in pages.iter().enumerate() {
        let data_start = out.len() as u32;
        let data = render_page(p, i as u32);
        let data_len = data.len() as u32;
        out.extend_from_slice(&data);
        let res_off = out.len() as u32;
        le32(&mut out, p.x_dpi);
        le32(&mut out, 1);
        le32(&mut out, p.y_dpi);
        le32(&mut out, 1);
        assert_eq!(out.len() as u32, ifd_offsets[i]);

        le16(&mut out, N_ENTRIES);
        entry(&mut out, 254, 4, 1, 2); // NewSubfileType: page of multi-page doc
        entry(&mut out, 256, 4, 1, p.width);
        entry(&mut out, 257, 4, 1, p.height);
        entry(&mut out, 258, 3, 1, 1); // BitsPerSample
        entry(&mut out, 259, 3, 1, 1); // Compression: none
        entry(&mut out, 262, 3, 1, 0); // Photometric: min-is-white
        entry(&mut out, 266, 3, 1, 1); // FillOrder: MSB first
        entry(&mut out, 273, 4, 1, data_start); // StripOffsets
        entry(&mut out, 277, 3, 1, 1); // SamplesPerPixel
        entry(&mut out, 278, 4, 1, p.height); // RowsPerStrip
        entry(&mut out, 279, 4, 1, data_len); // StripByteCounts
        entry(&mut out, 282, 5, 1, res_off); // XResolution
        entry(&mut out, 283, 5, 1, res_off + 8); // YResolution
        entry(&mut out, 296, 3, 1, 2); // ResolutionUnit: inch
        entry(&mut out, 297, 3, 2, i as u32 | ((pages.len() as u32) << 16)); // PageNumber
        let next = ifd_offsets.get(i + 1).copied().unwrap_or(0);
        le32(&mut out, next);
    }
    out
}

/// Write pages to `path`.
pub fn write_file(path: &Path, pages: &[PageSpec]) -> io::Result<()> {
    std::fs::write(path, encode(pages))
}
