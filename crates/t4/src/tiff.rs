//! Multi-page TIFF Class F writer (CCITT Group 4, little endian, one strip per page).

use crate::Error;

/// One page of G4 data to write to a TIFF or PDF.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    /// Width in pixels.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Horizontal resolution in dots per inch (normally 204).
    pub x_dpi: u32,
    /// Vertical resolution in dots per inch (98 standard, 196 fine).
    pub y_dpi: u32,
    /// MMR (T.6) encoded image data, including the EOFB.
    pub g4: &'a [u8],
    /// Rows that were replaced during reception (0 when the page is clean).
    pub bad_lines: u32,
    /// Longest run of consecutive replaced rows.
    pub longest_bad_run: u32,
}

impl Page<'_> {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::InvalidPage("zero size"));
        }
        if self.x_dpi == 0 || self.y_dpi == 0 {
            return Err(Error::InvalidPage("zero resolution"));
        }
        if self.g4.is_empty() {
            return Err(Error::InvalidPage("empty image data"));
        }
        Ok(())
    }
}

/// File-level metadata copied into every page's IFD.
#[derive(Debug, Clone, Copy)]
pub struct TiffMeta<'a> {
    /// `Software` tag.
    pub software: &'a str,
    /// `DateTime` tag in the TIFF format `YYYY:MM:DD HH:MM:SS` (see [`format_tiff_datetime`]).
    pub datetime: &'a str,
    /// `ImageDescription` tag; the server records the remote station id (TSI) here.
    pub image_description: Option<&'a str>,
}

/// Format Unix seconds as a TIFF `DateTime` string (UTC).
pub fn format_tiff_datetime(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}:{m:02}:{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn ascii_bytes(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s
        .bytes()
        .map(|b| if (0x20..0x7F).contains(&b) { b } else { b'?' })
        .collect();
    v.push(0);
    v
}

enum Value {
    Short(u16),
    Short2(u16, u16),
    Long(u32),
    /// Offset into the file of a RATIONAL (8 bytes).
    Rational(u32),
    Ascii(Vec<u8>),
}

struct Entry {
    tag: u16,
    value: Value,
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn offset(out: &[u8]) -> Result<u32, Error> {
    u32::try_from(out.len()).map_err(|_| Error::TooLarge)
}

/// Write pages as a multi-page TIFF Class F file.
///
/// Tags per page: NewSubfileType, ImageWidth/Length, BitsPerSample 1, Compression 4 (G4),
/// Photometric 0 (MinIsWhite), FillOrder 1, ImageDescription, StripOffsets, SamplesPerPixel,
/// RowsPerStrip, StripByteCounts, X/YResolution, T6Options 0, ResolutionUnit inch,
/// PageNumber, Software, DateTime and, when rows were replaced, BadFaxLines,
/// CleanFaxData and ConsecutiveBadFaxLines.
pub fn write_tiff(pages: &[Page<'_>], meta: &TiffMeta<'_>) -> Result<Vec<u8>, Error> {
    if pages.is_empty() {
        return Err(Error::NoPages);
    }
    if meta.datetime.len() != 19 || !meta.datetime.is_ascii() {
        return Err(Error::InvalidMetadata(
            "datetime must be 'YYYY:MM:DD HH:MM:SS'",
        ));
    }
    let total = u16::try_from(pages.len()).map_err(|_| Error::TooLarge)?;
    let mut out = Vec::new();
    out.extend_from_slice(b"II");
    le16(&mut out, 42);
    le32(&mut out, 0); // patched to the first IFD
    let mut next_ptr_at = 4usize;

    for (idx, p) in pages.iter().enumerate() {
        p.validate()?;
        let strip_off = offset(&out)?;
        out.extend_from_slice(p.g4);
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let xres = offset(&out)?;
        le32(&mut out, p.x_dpi);
        le32(&mut out, 1);
        let yres = offset(&out)?;
        le32(&mut out, p.y_dpi);
        le32(&mut out, 1);

        let mut e = vec![
            Entry {
                tag: 254,
                value: Value::Long(2),
            },
            Entry {
                tag: 256,
                value: Value::Long(p.width),
            },
            Entry {
                tag: 257,
                value: Value::Long(p.height),
            },
            Entry {
                tag: 258,
                value: Value::Short(1),
            },
            Entry {
                tag: 259,
                value: Value::Short(4),
            },
            Entry {
                tag: 262,
                value: Value::Short(0),
            },
            Entry {
                tag: 266,
                value: Value::Short(1),
            },
        ];
        if let Some(d) = meta.image_description {
            e.push(Entry {
                tag: 270,
                value: Value::Ascii(ascii_bytes(d)),
            });
        }
        e.extend([
            Entry {
                tag: 273,
                value: Value::Long(strip_off),
            },
            Entry {
                tag: 277,
                value: Value::Short(1),
            },
            Entry {
                tag: 278,
                value: Value::Long(p.height),
            },
            Entry {
                tag: 279,
                value: Value::Long(u32::try_from(p.g4.len()).map_err(|_| Error::TooLarge)?),
            },
            Entry {
                tag: 282,
                value: Value::Rational(xres),
            },
            Entry {
                tag: 283,
                value: Value::Rational(yres),
            },
            Entry {
                tag: 293,
                value: Value::Long(0),
            },
            Entry {
                tag: 296,
                value: Value::Short(2),
            },
            Entry {
                tag: 297,
                value: Value::Short2(idx as u16, total),
            },
            Entry {
                tag: 305,
                value: Value::Ascii(ascii_bytes(meta.software)),
            },
            Entry {
                tag: 306,
                value: Value::Ascii(ascii_bytes(meta.datetime)),
            },
        ]);
        if p.bad_lines > 0 {
            e.extend([
                Entry {
                    tag: 326,
                    value: Value::Long(p.bad_lines),
                },
                Entry {
                    tag: 327,
                    value: Value::Short(1),
                },
                Entry {
                    tag: 328,
                    value: Value::Long(p.longest_bad_run),
                },
            ]);
        }

        // Out-of-line ASCII values go in front of the IFD.
        let mut ascii_offsets = Vec::new();
        for en in &e {
            if let Value::Ascii(b) = &en.value {
                if b.len() > 4 {
                    ascii_offsets.push(offset(&out)?);
                    out.extend_from_slice(b);
                    if out.len() % 2 == 1 {
                        out.push(0);
                    }
                } else {
                    ascii_offsets.push(0);
                }
            } else {
                ascii_offsets.push(0);
            }
        }

        let ifd = offset(&out)?;
        out[next_ptr_at..next_ptr_at + 4].copy_from_slice(&ifd.to_le_bytes());
        le16(&mut out, e.len() as u16);
        for (en, aoff) in e.iter().zip(&ascii_offsets) {
            le16(&mut out, en.tag);
            match &en.value {
                Value::Short(v) => {
                    le16(&mut out, 3);
                    le32(&mut out, 1);
                    le16(&mut out, *v);
                    le16(&mut out, 0);
                }
                Value::Short2(a, b) => {
                    le16(&mut out, 3);
                    le32(&mut out, 2);
                    le16(&mut out, *a);
                    le16(&mut out, *b);
                }
                Value::Long(v) => {
                    le16(&mut out, 4);
                    le32(&mut out, 1);
                    le32(&mut out, *v);
                }
                Value::Rational(o) => {
                    le16(&mut out, 5);
                    le32(&mut out, 1);
                    le32(&mut out, *o);
                }
                Value::Ascii(b) => {
                    le16(&mut out, 2);
                    le32(&mut out, b.len() as u32);
                    if b.len() > 4 {
                        le32(&mut out, *aoff);
                    } else {
                        let mut inl = [0u8; 4];
                        inl[..b.len()].copy_from_slice(b);
                        out.extend_from_slice(&inl);
                    }
                }
            }
        }
        next_ptr_at = out.len();
        le32(&mut out, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
