//! Test helpers shared by the integration tests: bitmap generators, a tiny independent
//! uncompressed TIFF writer, bit-string helpers.
#![allow(dead_code)]

/// Bytes per row.
pub fn rb(width: u32) -> usize {
    (width as usize).div_ceil(8)
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

pub fn set_px(bm: &mut [u8], width: u32, x: u32, y: u32) {
    bm[y as usize * rb(width) + (x >> 3) as usize] |= 0x80 >> (x & 7);
}

pub fn get_px(bm: &[u8], width: u32, x: u32, y: u32) -> bool {
    bm[y as usize * rb(width) + (x >> 3) as usize] & (0x80 >> (x & 7)) != 0
}

/// The five test bitmaps (1 = black): all white, all black, checkerboard, text-like scan,
/// random noise.
pub fn bitmaps(width: u32, rows: u32) -> Vec<(&'static str, Vec<u8>)> {
    let n = rb(width) * rows as usize;
    let white = vec![0u8; n];
    let mut black = vec![0u8; n];
    for y in 0..rows {
        for x in 0..width {
            set_px(&mut black, width, x, y);
        }
    }
    let mut checker = vec![0u8; n];
    for y in 0..rows {
        for x in 0..width {
            if (x + y) % 2 == 0 {
                set_px(&mut checker, width, x, y);
            }
        }
    }
    // Text-like: margins, lines of "glyphs" of varying widths and heights, a rule and a
    // 1-pixel vertical line, with a slight skew every few lines.
    let mut text = vec![0u8; n];
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for y in 0..rows {
        for x in 0..width {
            if y < 6 || (x == 40 && y % 5 != 0) {
                set_px(&mut text, width, x, y);
            }
        }
    }
    let mut line_top = 20;
    while line_top + 16 < rows {
        let mut x = 60 + (line_top % 7);
        while x + 30 < width.saturating_sub(60) {
            let gw = 3 + (rng.next() % 9) as u32;
            let gh = 6 + (rng.next() % 9) as u32;
            if !rng.next().is_multiple_of(6) {
                for dy in 0..gh {
                    for dx in 0..gw {
                        // hollow glyph: only the outline
                        if dx < 2 || dx + 2 >= gw || dy < 2 || dy + 2 >= gh {
                            set_px(&mut text, width, x + dx, line_top + 16 - gh + dy);
                        }
                    }
                }
            }
            x += gw + 2 + (rng.next() % 5) as u32;
        }
        line_top += 24;
    }
    let mut noise = vec![0u8; n];
    for b in noise.iter_mut() {
        *b = (rng.next() >> 24) as u8;
    }
    // clear padding bits
    if !width.is_multiple_of(8) {
        for y in 0..rows as usize {
            noise[y * rb(width) + rb(width) - 1] &= 0xFFu8 << (8 - width % 8);
        }
    }
    vec![
        ("white", white),
        ("black", black),
        ("checker", checker),
        ("text", text),
        ("noise", noise),
    ]
}

pub fn row(bm: &[u8], width: u32, y: usize) -> &[u8] {
    &bm[y * rb(width)..(y + 1) * rb(width)]
}

/// Bit string like "0001 1010" to packed bytes (padded with zeros).
pub fn bits(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut n) = (0u8, 0);
    for c in s.chars().filter(|c| *c == '0' || *c == '1') {
        acc = (acc << 1) | u8::from(c == '1');
        n += 1;
        if n == 8 {
            out.push(acc);
            acc = 0;
            n = 0;
        }
    }
    if n > 0 {
        out.push(acc << (8 - n));
    }
    out
}

/// Positions (bit index of the first zero) of every EOL (>= 11 zeros then a 1) in a stream.
pub fn eol_positions(data: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut zeros = 0usize;
    for i in 0..data.len() * 8 {
        let bit = (data[i / 8] >> (7 - i % 8)) & 1;
        if bit == 0 {
            zeros += 1;
        } else {
            if zeros >= 11 {
                v.push(i - zeros);
            }
            zeros = 0;
        }
    }
    v
}

fn le16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn le32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}

/// Independent minimal uncompressed multi-page TIFF writer: (width, rows, y_dpi, bitmap).
pub fn uncompressed_tiff(pages: &[(u32, u32, u32, &[u8])]) -> Vec<u8> {
    let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
    let mut next_at = 4usize;
    for (i, (w, h, ydpi, bm)) in pages.iter().enumerate() {
        assert_eq!(bm.len(), rb(*w) * *h as usize);
        let data = out.len() as u32;
        out.extend_from_slice(bm);
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let xr = out.len() as u32;
        le32(&mut out, 204);
        le32(&mut out, 1);
        let yr = out.len() as u32;
        le32(&mut out, *ydpi);
        le32(&mut out, 1);
        let ifd = out.len() as u32;
        out[next_at..next_at + 4].copy_from_slice(&ifd.to_le_bytes());
        let entries: [(u16, u16, u32); 14] = [
            (254, 4, 2),
            (256, 4, *w),
            (257, 4, *h),
            (258, 3, 1),
            (259, 3, 1),
            (262, 3, 0),
            (266, 3, 1),
            (273, 4, data),
            (277, 3, 1),
            (278, 4, *h),
            (279, 4, bm.len() as u32),
            (282, 5, xr),
            (283, 5, yr),
            (296, 3, 2),
        ];
        le16(&mut out, entries.len() as u16 + 1);
        for (tag, typ, val) in entries {
            le16(&mut out, tag);
            le16(&mut out, typ);
            le32(&mut out, 1);
            le32(&mut out, val);
        }
        // PageNumber (297): two SHORTs
        le16(&mut out, 297);
        le16(&mut out, 3);
        le32(&mut out, 2);
        le16(&mut out, i as u16);
        le16(&mut out, pages.len() as u16);
        next_at = out.len();
        le32(&mut out, 0);
    }
    out
}
