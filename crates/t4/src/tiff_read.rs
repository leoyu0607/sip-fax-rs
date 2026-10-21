//! TIFF Class F reader: parses the IFD chain of a (multi-page) TIFF and decodes pages to
//! packed 1-bit bitmaps.
//!
//! Supported: little and big endian, uncompressed (1), CCITT G3 (3, with or without the 2-D
//! option) and G4 (4), any number of strips, `FillOrder` 1 and 2, both photometric
//! interpretations, resolution units inch and centimetre. Compression 2 (EOL-less, byte aligned
//! "CCITT RLE"), tiles, and more than one bit per sample are rejected with
//! [`Error::UnsupportedTiff`]. The input is untrusted: every offset and count is bounds checked
//! and nothing here panics.

use crate::{Coding, DEFAULT_MAX_ROWS, DecoderConfig, Error, MAX_WIDTH, decode_page, row_bytes};
use std::collections::HashMap;

/// Most IFDs (pages) accepted in one file.
const MAX_PAGES: usize = 4096;
/// Most strips accepted in one page.
const MAX_STRIPS: u64 = 1 << 16;

/// How the image data of a page is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffCoding {
    /// Uncompressed (compression 1).
    None,
    /// CCITT G3 one-dimensional (compression 3, T4Options bit 0 clear).
    Mh,
    /// CCITT G3 two-dimensional (compression 3, T4Options bit 0 set).
    Mr,
    /// CCITT G4 (compression 4).
    Mmr,
}

/// Header information of one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiffPageInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Horizontal resolution in dpi, rounded (0 when the file does not say).
    pub x_dpi: u32,
    /// Vertical resolution in dpi, rounded (0 when the file does not say).
    pub y_dpi: u32,
    /// Storage of the image data.
    pub coding: TiffCoding,
}

/// A decoded page: packed 1-bit rows, MSB first, **1 = black**, padding bits 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Horizontal resolution in dpi (0 = unknown).
    pub x_dpi: u32,
    /// Vertical resolution in dpi (0 = unknown).
    pub y_dpi: u32,
    /// `height * ceil(width / 8)` bytes.
    pub data: Vec<u8>,
    /// Rows replaced or missing while decoding (0 for a clean page).
    pub bad_lines: u32,
}

#[derive(Debug, Clone)]
struct PageEntry {
    info: TiffPageInfo,
    rows_per_strip: u32,
    strips: Vec<(u64, u64)>,
    reverse_bits: bool,
    /// MinIsBlack: stored bits are 1 = white. Only matters for uncompressed data; the T.4 / T.6
    /// code words name black and white runs themselves.
    invert: bool,
}

/// A parsed TIFF file. Parsing reads only the headers; pixels are decoded per page by
/// [`TiffReader::decode_page`].
#[derive(Debug)]
pub struct TiffReader<'a> {
    data: &'a [u8],
    pages: Vec<PageEntry>,
}

#[derive(Clone, Copy)]
struct Cursor<'a> {
    d: &'a [u8],
    le: bool,
}

impl Cursor<'_> {
    fn bytes<const N: usize>(&self, o: u64) -> Result<[u8; N], Error> {
        let o = usize::try_from(o).map_err(|_| Error::InvalidTiff("offset"))?;
        let end = o.checked_add(N).ok_or(Error::InvalidTiff("offset"))?;
        self.d
            .get(o..end)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::InvalidTiff("truncated file"))
    }
    fn u16(&self, o: u64) -> Result<u16, Error> {
        let b = self.bytes::<2>(o)?;
        Ok(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(&self, o: u64) -> Result<u32, Error> {
        let b = self.bytes::<4>(o)?;
        Ok(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
}

/// One IFD entry: type, count and the offset of its value bytes.
#[derive(Clone, Copy)]
struct Entry {
    typ: u16,
    count: u64,
    at: u64,
}

impl Entry {
    fn size(&self) -> u64 {
        match self.typ {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            _ => 8,
        }
    }

    /// Integer values (BYTE, SHORT, LONG), at most `limit` of them.
    fn ints(&self, c: &Cursor<'_>, limit: u64) -> Result<Vec<u64>, Error> {
        if self.count > limit {
            return Err(Error::InvalidTiff("too many values in a tag"));
        }
        (0..self.count)
            .map(|i| match self.typ {
                1 => c.bytes::<1>(self.at + i).map(|b| u64::from(b[0])),
                3 => c.u16(self.at + 2 * i).map(u64::from),
                4 => c.u32(self.at + 4 * i).map(u64::from),
                _ => Err(Error::InvalidTiff("unexpected tag type")),
            })
            .collect()
    }

    fn one(&self, c: &Cursor<'_>) -> Result<u64, Error> {
        self.ints(c, 1)?
            .first()
            .copied()
            .ok_or(Error::InvalidTiff("empty tag"))
    }

    /// RATIONAL as a float; `None` for a zero denominator.
    fn rational(&self, c: &Cursor<'_>) -> Result<Option<f64>, Error> {
        if self.typ != 5 || self.count < 1 {
            return Err(Error::InvalidTiff("resolution is not a rational"));
        }
        let n = f64::from(c.u32(self.at)?);
        let d = f64::from(c.u32(self.at + 4)?);
        Ok((d != 0.0).then(|| n / d))
    }
}

fn parse_ifd(c: &Cursor<'_>, off: u64) -> Result<PageEntry, Error> {
    let n = u64::from(c.u16(off)?);
    let mut tags = HashMap::new();
    for i in 0..n {
        let e = off + 2 + i * 12;
        let tag = c.u16(e)?;
        let typ = c.u16(e + 2)?;
        let count = u64::from(c.u32(e + 4)?);
        let mut ent = Entry { typ, count, at: 0 };
        let total = count
            .checked_mul(ent.size())
            .ok_or(Error::InvalidTiff("tag too large"))?;
        ent.at = if total <= 4 {
            e + 8
        } else {
            u64::from(c.u32(e + 8)?)
        };
        tags.insert(tag, ent);
    }
    let get = |t: u16| tags.get(&t).copied();
    let one =
        |t: u16, default: u64| -> Result<u64, Error> { get(t).map_or(Ok(default), |e| e.one(c)) };

    let width = one(256, 0)?;
    let height = one(257, 0)?;
    if width == 0 || height == 0 {
        return Err(Error::InvalidTiff("missing or zero image size"));
    }
    if width > u64::from(MAX_WIDTH) || height > u64::from(DEFAULT_MAX_ROWS) {
        return Err(Error::UnsupportedTiff("image too large"));
    }
    if one(258, 1)? != 1 || one(277, 1)? != 1 {
        return Err(Error::UnsupportedTiff("not a 1-bit single sample image"));
    }
    if get(322).is_some() || get(324).is_some() {
        return Err(Error::UnsupportedTiff("tiled images"));
    }
    let coding = match one(259, 1)? {
        1 => TiffCoding::None,
        3 => {
            if one(292, 0)? & 1 == 1 {
                TiffCoding::Mr
            } else {
                TiffCoding::Mh
            }
        }
        4 => TiffCoding::Mmr,
        _ => return Err(Error::UnsupportedTiff("compression")),
    };
    let invert = match one(262, 0)? {
        0 => false,
        1 => true,
        _ => return Err(Error::UnsupportedTiff("photometric interpretation")),
    };
    let reverse_bits = match one(266, 1)? {
        1 => false,
        2 => true,
        _ => return Err(Error::InvalidTiff("fill order")),
    };
    let offsets = get(273)
        .ok_or(Error::InvalidTiff("no strip offsets"))?
        .ints(c, MAX_STRIPS)?;
    let counts = get(279)
        .ok_or(Error::InvalidTiff("no strip byte counts"))?
        .ints(c, MAX_STRIPS)?;
    if offsets.len() != counts.len() || offsets.is_empty() {
        return Err(Error::InvalidTiff("strip tables disagree"));
    }
    let rows_per_strip = one(278, u64::from(u32::MAX))?.clamp(1, u64::from(u32::MAX)) as u32;
    let height = height as u32;
    if u64::from(height).div_ceil(u64::from(rows_per_strip)) > offsets.len() as u64 {
        return Err(Error::InvalidTiff("fewer strips than the image needs"));
    }
    for (o, n) in offsets.iter().zip(&counts) {
        let end = o.checked_add(*n).ok_or(Error::InvalidTiff("strip range"))?;
        if end > c.d.len() as u64 {
            return Err(Error::InvalidTiff("strip lies beyond the end of the file"));
        }
    }

    let unit = one(296, 2)?;
    let dpi = |t: u16| -> Result<u32, Error> {
        let Some(e) = get(t) else { return Ok(0) };
        let v = e.rational(c)?.unwrap_or(0.0);
        let v = if unit == 3 { v * 2.54 } else { v };
        Ok(if v.is_finite() && (0.0..=100_000.0).contains(&v) {
            v.round() as u32
        } else {
            0
        })
    };
    Ok(PageEntry {
        info: TiffPageInfo {
            width: width as u32,
            height,
            x_dpi: dpi(282)?,
            y_dpi: dpi(283)?,
            coding,
        },
        rows_per_strip,
        strips: offsets.into_iter().zip(counts).collect(),
        reverse_bits,
        invert,
    })
}

impl<'a> TiffReader<'a> {
    /// Parse the header and every IFD of `data`.
    pub fn new(data: &'a [u8]) -> Result<Self, Error> {
        let le = match data.get(..2) {
            Some(b"II") => true,
            Some(b"MM") => false,
            _ => return Err(Error::InvalidTiff("not a TIFF file")),
        };
        let c = Cursor { d: data, le };
        if c.u16(2)? != 42 {
            return Err(Error::InvalidTiff("bad TIFF magic"));
        }
        let mut pages: Vec<PageEntry> = Vec::new();
        let mut seen: Vec<u64> = Vec::new();
        let mut off = u64::from(c.u32(4)?);
        while off != 0 {
            if pages.len() >= MAX_PAGES {
                return Err(Error::UnsupportedTiff("too many pages"));
            }
            if seen.contains(&off) {
                return Err(Error::InvalidTiff("IFD chain loops"));
            }
            seen.push(off);
            pages.push(parse_ifd(&c, off)?);
            let n = u64::from(c.u16(off)?);
            off = u64::from(c.u32(off + 2 + n * 12)?);
        }
        if pages.is_empty() {
            return Err(Error::InvalidTiff("no pages"));
        }
        Ok(Self { data, pages })
    }

    /// Number of pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Header information of page `index` (0-based).
    pub fn info(&self, index: usize) -> Option<TiffPageInfo> {
        self.pages.get(index).map(|p| p.info)
    }

    /// Decode page `index` (0-based).
    ///
    /// A page that is damaged part-way still decodes: missing rows are white and counted in
    /// [`Bitmap::bad_lines`]. Data that yields no row at all is an error.
    pub fn decode_page(&self, index: usize) -> Result<Bitmap, Error> {
        let p = self.pages.get(index).ok_or(Error::PageOutOfRange {
            page: index,
            pages: self.pages.len(),
        })?;
        let (w, h) = (p.info.width, p.info.height);
        let rb = row_bytes(w);
        let mut data = vec![0u8; rb * h as usize];
        let mut bad = 0u32;
        let mut decoded_any = false;
        let mut first_err = None;
        let mut row0 = 0u32;
        for (o, n) in &p.strips {
            if row0 >= h {
                break;
            }
            let rows = p.rows_per_strip.min(h - row0);
            let raw = usize::try_from(*o)
                .ok()
                .zip(usize::try_from(*o + *n).ok())
                .and_then(|(a, b)| self.data.get(a..b))
                .ok_or(Error::InvalidTiff("strip lies beyond the end of the file"))?;
            let rev: Vec<u8>;
            let strip = if p.reverse_bits {
                rev = raw.iter().map(|b| b.reverse_bits()).collect();
                &rev[..]
            } else {
                raw
            };
            let dst = &mut data[row0 as usize * rb..(row0 + rows) as usize * rb];
            let copied = match p.info.coding {
                TiffCoding::None => {
                    let n = strip.len().min(dst.len());
                    dst[..n].copy_from_slice(&strip[..n]);
                    n
                }
                c => {
                    let coding = match c {
                        TiffCoding::Mh => Coding::Mh,
                        TiffCoding::Mr => Coding::Mr { k: 2 },
                        _ => Coding::Mmr,
                    };
                    let mut cfg = DecoderConfig::new(w, coding);
                    cfg.max_rows = rows;
                    let pd = decode_page(strip, cfg)?;
                    let n = pd.bitmap.len().min(dst.len());
                    dst[..n].copy_from_slice(&pd.bitmap[..n]);
                    bad += pd.summary.stats.bad_lines;
                    if first_err.is_none() {
                        first_err = pd.summary.error;
                    }
                    n
                }
            };
            let got = (copied / rb) as u32;
            if p.invert && p.info.coding == TiffCoding::None {
                for b in &mut dst[..copied] {
                    *b = !*b;
                }
            }
            if got > 0 {
                decoded_any = true;
            }
            bad += rows.saturating_sub(got);
            row0 += rows;
        }
        if !decoded_any {
            return Err(first_err.unwrap_or(Error::InvalidTiff("page has no decodable rows")));
        }
        clear_padding(&mut data, w);
        Ok(Bitmap {
            width: w,
            height: h,
            x_dpi: p.info.x_dpi,
            y_dpi: p.info.y_dpi,
            data,
            bad_lines: bad,
        })
    }
}

/// Zero the padding bits at the end of every row.
fn clear_padding(data: &mut [u8], width: u32) {
    let rb = row_bytes(width);
    if !width.is_multiple_of(8) {
        let mask = 0xFFu8 << (8 - width % 8);
        for row in data.chunks_exact_mut(rb) {
            row[rb - 1] &= mask;
        }
    }
}
