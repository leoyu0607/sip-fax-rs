//! PNG output for the web preview: a full page as 1-bit grayscale and a 300 px wide 8-bit
//! grayscale thumbnail.
//!
//! * Standard resolution pages (98 dpi vertical against 204 dpi horizontal) have non-square
//!   pixels; every row is written twice so the page has the right proportions. Fine resolution
//!   (196 dpi) is written as is. See [`row_repeat`].
//! * The thumbnail is an exact area average: every output pixel is the mean of the (fractional)
//!   source area it covers, computed on the aspect-corrected page. Its height follows the
//!   corrected aspect ratio.

use crate::{Bitmap, Error, row_bytes};
use std::io::Cursor;

/// Width of a thumbnail in pixels.
pub const THUMB_WIDTH: u32 = 300;

/// How many times each row is written so the page keeps its proportions: 2 when the horizontal
/// resolution is at least 1.5 times the vertical one (standard: 204 x 98), otherwise 1 (fine:
/// 204 x 196, or unknown resolution).
pub fn row_repeat(x_dpi: u32, y_dpi: u32) -> u32 {
    if x_dpi > 0 && y_dpi > 0 && u64::from(x_dpi) * 2 >= u64::from(y_dpi) * 3 {
        2
    } else {
        1
    }
}

fn encode(width: u32, height: u32, depth: png::BitDepth, data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(Cursor::new(&mut out), width, height);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(depth);
        let mut w = enc.write_header().map_err(|_| Error::Png)?;
        w.write_image_data(data).map_err(|_| Error::Png)?;
        w.finish().map_err(|_| Error::Png)?;
    }
    Ok(out)
}

fn check(bm: &Bitmap) -> Result<(), Error> {
    if bm.width == 0 || bm.height == 0 || bm.data.len() != row_bytes(bm.width) * bm.height as usize
    {
        return Err(Error::InvalidPage("bitmap size does not match its data"));
    }
    Ok(())
}

/// Encode the whole page as a 1-bit grayscale PNG (black text on white), repeating each row
/// [`row_repeat`] times.
pub fn encode_full(bm: &Bitmap) -> Result<Vec<u8>, Error> {
    check(bm)?;
    let rep = row_repeat(bm.x_dpi, bm.y_dpi);
    let rb = row_bytes(bm.width);
    let mut img = Vec::with_capacity(bm.data.len() * rep as usize);
    for row in bm.data.chunks_exact(rb) {
        // PNG grayscale: 0 = black, so invert the 1 = black bitmap.
        for _ in 0..rep {
            img.extend(row.iter().map(|b| !b));
        }
    }
    encode(bm.width, bm.height * rep, png::BitDepth::One, &img)
}

/// Size of the thumbnail of `bm`: width [`THUMB_WIDTH`], height from the corrected aspect ratio
/// (at least 1).
pub fn thumb_size(bm: &Bitmap) -> (u32, u32) {
    let rep = u64::from(row_repeat(bm.x_dpi, bm.y_dpi));
    let corrected = u64::from(bm.height) * rep;
    let h =
        (corrected * u64::from(THUMB_WIDTH) + u64::from(bm.width) / 2) / u64::from(bm.width.max(1));
    (THUMB_WIDTH, (h as u32).max(1))
}

/// Area-average the page down to [`THUMB_WIDTH`] pixels wide, 8-bit gray (255 = white).
/// Returns `(width, height, pixels)`.
pub fn thumbnail(bm: &Bitmap) -> Result<(u32, u32, Vec<u8>), Error> {
    check(bm)?;
    let rep = row_repeat(bm.x_dpi, bm.y_dpi);
    let (tw, th) = thumb_size(bm);
    let (w, h) = (bm.width as usize, bm.height as usize);
    let rb = row_bytes(bm.width);

    // Horizontal pass: per source row, the mean "whiteness" of each of the tw output columns.
    let mut rows = vec![0f32; h * tw as usize];
    let mut cum = vec![0u32; w + 1];
    let mut white = vec![0u8; w];
    let scale = w as f64 / f64::from(tw);
    for (y, row) in bm.data.chunks_exact(rb).enumerate() {
        for x in 0..w {
            let w1 = u8::from(row[x >> 3] & (0x80 >> (x & 7)) == 0);
            white[x] = w1;
            cum[x + 1] = cum[x] + u32::from(w1);
        }
        let f = |t: f64| -> f64 {
            let i = (t.floor() as usize).min(w);
            let base = f64::from(cum[i]);
            if i >= w {
                base
            } else {
                base + (t - i as f64) * f64::from(white[i])
            }
        };
        let out = &mut rows[y * tw as usize..(y + 1) * tw as usize];
        for (ox, o) in out.iter_mut().enumerate() {
            let a = ox as f64 * scale;
            let b = (ox as f64 + 1.0) * scale;
            *o = ((f(b) - f(a)) / (b - a)) as f32;
        }
    }

    // Vertical pass on the corrected page (source row r covers corrected rows r*rep..(r+1)*rep).
    let corrected = (h as u32 * rep) as f64;
    let yscale = corrected / f64::from(th);
    let rep_f = f64::from(rep);
    let mut pixels = vec![0u8; tw as usize * th as usize];
    for oy in 0..th as usize {
        let ya = oy as f64 * yscale;
        let yb = (oy as f64 + 1.0) * yscale;
        let first = (ya / rep_f).floor() as usize;
        let last = (((yb / rep_f).ceil() as usize).max(first + 1)).min(h);
        for ox in 0..tw as usize {
            let mut acc = 0f64;
            for r in first..last {
                let lo = ya.max(r as f64 * rep_f);
                let hi = yb.min((r as f64 + 1.0) * rep_f);
                if hi > lo {
                    acc += (hi - lo) * f64::from(rows[r * tw as usize + ox]);
                }
            }
            let v = (acc / (yb - ya) * 255.0).round().clamp(0.0, 255.0);
            pixels[oy * tw as usize + ox] = v as u8;
        }
    }
    Ok((tw, th, pixels))
}

/// Encode the thumbnail as an 8-bit grayscale PNG.
pub fn encode_thumb(bm: &Bitmap) -> Result<Vec<u8>, Error> {
    let (w, h, px) = thumbnail(bm)?;
    encode(w, h, png::BitDepth::Eight, &px)
}

/// Encode an arbitrary packed 1-bit bitmap (1 = black) as a 1-bit PNG without any resolution
/// handling. Used by the property tests; [`encode_full`] is the production entry point.
pub fn encode_bilevel(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>, Error> {
    encode_full(&Bitmap {
        width,
        height,
        x_dpi: 0,
        y_dpi: 0,
        data: data.to_vec(),
        bad_lines: 0,
    })
}
