//! Minimal independent TIFF metadata reader (page count, size, resolution).
//! Handles both byte orders; does not decode pixel data.

use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    pub width: u32,
    pub height: u32,
    /// Resolution converted to dots per inch (None if tags are absent).
    pub x_dpi: Option<f64>,
    pub y_dpi: Option<f64>,
    pub compression: Option<u32>,
    pub bits_per_sample: Option<u32>,
}

#[derive(Debug)]
pub enum TiffError {
    Io(std::io::Error),
    Malformed(&'static str),
}

impl fmt::Display for TiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Malformed(m) => write!(f, "malformed TIFF: {m}"),
        }
    }
}
impl std::error::Error for TiffError {}

struct Rd<'a> {
    b: &'a [u8],
    le: bool,
}

impl Rd<'_> {
    fn u16(&self, o: usize) -> Result<u16, TiffError> {
        let s = self.b.get(o..o + 2).ok_or(TiffError::Malformed("eof"))?;
        let a = [s[0], s[1]];
        Ok(if self.le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    }
    fn u32(&self, o: usize) -> Result<u32, TiffError> {
        let s = self.b.get(o..o + 4).ok_or(TiffError::Malformed("eof"))?;
        let a = [s[0], s[1], s[2], s[3]];
        Ok(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
}

/// Parse all IFDs of an in-memory TIFF.
pub fn parse(bytes: &[u8]) -> Result<Vec<PageInfo>, TiffError> {
    let le = match bytes.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(TiffError::Malformed("bad byte-order mark")),
    };
    let r = Rd { b: bytes, le };
    if r.u16(2)? != 42 {
        return Err(TiffError::Malformed("bad magic"));
    }
    let mut ifd = r.u32(4)? as usize;
    let mut pages = Vec::new();
    let mut seen = Vec::new();
    while ifd != 0 {
        if seen.contains(&ifd) || pages.len() > 100_000 {
            return Err(TiffError::Malformed("IFD loop"));
        }
        seen.push(ifd);
        let n = r.u16(ifd)? as usize;
        let (mut w, mut h, mut xr, mut yr) = (None, None, None, None);
        let (mut unit, mut comp, mut bps) = (2u32, None, None);
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            let tag = r.u16(e)?;
            let typ = r.u16(e + 2)?;
            let count = r.u32(e + 4)?;
            let scalar = |r: &Rd| -> Result<u32, TiffError> {
                match typ {
                    3 => Ok(u32::from(r.u16(e + 8)?)),
                    4 => r.u32(e + 8),
                    _ => Err(TiffError::Malformed("unexpected tag type")),
                }
            };
            let rational = |r: &Rd| -> Result<f64, TiffError> {
                if typ != 5 || count != 1 {
                    return Err(TiffError::Malformed("bad rational"));
                }
                let off = r.u32(e + 8)? as usize;
                let (n, d) = (r.u32(off)?, r.u32(off + 4)?);
                if d == 0 {
                    return Err(TiffError::Malformed("zero denominator"));
                }
                Ok(f64::from(n) / f64::from(d))
            };
            match tag {
                256 => w = Some(scalar(&r)?),
                257 => h = Some(scalar(&r)?),
                258 => bps = Some(scalar(&r)?),
                259 => comp = Some(scalar(&r)?),
                282 => xr = Some(rational(&r)?),
                283 => yr = Some(rational(&r)?),
                296 => unit = scalar(&r)?,
                _ => {}
            }
        }
        let scale = match unit {
            2 => 1.0,
            3 => 2.54,
            _ => 1.0,
        };
        pages.push(PageInfo {
            width: w.ok_or(TiffError::Malformed("no width"))?,
            height: h.ok_or(TiffError::Malformed("no length"))?,
            x_dpi: xr.map(|v| v * scale),
            y_dpi: yr.map(|v| v * scale),
            compression: comp,
            bits_per_sample: bps,
        });
        ifd = r.u32(ifd + 2 + n * 12)? as usize;
    }
    if pages.is_empty() {
        return Err(TiffError::Malformed("no IFD"));
    }
    Ok(pages)
}

/// Parse a TIFF file from disk.
pub fn read_file(path: &Path) -> Result<Vec<PageInfo>, TiffError> {
    parse(&std::fs::read(path).map_err(TiffError::Io)?)
}
