//! M3e host tests: TIFF reader, PNG output (P01 Rust side, P02) and their properties.
//! The independent libtiff / netpbm comparison of P01 lives in `png_oracle.rs`.

mod common;

use common::*;
use proptest::prelude::*;
use t4::png::{self, THUMB_WIDTH};
use t4::{
    Bitmap, Coding, Encoder, Error, Page, TiffCoding, TiffMeta, TiffReader, encode_mmr, write_tiff,
};

fn meta() -> TiffMeta<'static> {
    TiffMeta {
        software: "sip-fax-rs preview test",
        datetime: "2026:10:21 08:30:00",
        image_description: Some("0229999999"),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Depth {
    One,
    Eight,
}

/// Decode a PNG with the `png` crate: (width, height, bit depth, line bytes, pixels).
fn decode_png(data: &[u8]) -> (u32, u32, Depth, Vec<u8>) {
    let mut dec = ::png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(::png::Transformations::IDENTITY);
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, ::png::ColorType::Grayscale);
    let depth = match info.bit_depth {
        ::png::BitDepth::One => Depth::One,
        ::png::BitDepth::Eight => Depth::Eight,
        d => panic!("unexpected depth {d:?}"),
    };
    buf.truncate(info.buffer_size());
    (info.width, info.height, depth, buf)
}

fn g4_tiff(pages: &[(u32, u32, u32, u32, &[u8])]) -> Vec<u8> {
    // (width, height, x_dpi, y_dpi, bitmap)
    let encoded: Vec<Vec<u8>> = pages
        .iter()
        .map(|(w, _, _, _, bm)| encode_mmr(bm, *w).unwrap())
        .collect();
    let ps: Vec<Page> = pages
        .iter()
        .zip(&encoded)
        .map(|((w, h, x, y, _), g4)| Page {
            width: *w,
            height: *h,
            x_dpi: *x,
            y_dpi: *y,
            g4,
            bad_lines: 0,
            longest_bad_run: 0,
        })
        .collect();
    write_tiff(&ps, &meta()).unwrap()
}

fn pixel(bm: &[u8], width: u32, x: u32, y: u32) -> bool {
    get_px(bm, width, x, y)
}

// ---------------------------------------------------------------- reader

#[test]
fn p01_reader_roundtrips_our_g4_tiff_with_page_order_and_resolution() {
    let (w, h) = (1728u32, 120u32);
    let all = bitmaps(w, h);
    let pages: Vec<(u32, u32, u32, u32, &[u8])> = all
        .iter()
        .enumerate()
        .map(|(i, (_, bm))| (w, h, 204, if i % 2 == 0 { 98 } else { 196 }, bm.as_slice()))
        .collect();
    let tiff = g4_tiff(&pages);
    let r = TiffReader::new(&tiff).unwrap();
    assert_eq!(r.page_count(), 5);
    for (i, (name, bm)) in all.iter().enumerate() {
        let info = r.info(i).unwrap();
        assert_eq!(info.coding, TiffCoding::Mmr);
        assert_eq!((info.width, info.height, info.x_dpi), (w, h, 204), "{name}");
        assert_eq!(info.y_dpi, if i % 2 == 0 { 98 } else { 196 }, "{name}");
        let page = r.decode_page(i).unwrap();
        assert_eq!(&page.data, bm, "page {i} ({name}) pixels");
        assert_eq!(page.bad_lines, 0);
    }
    assert_eq!(r.info(5), None);
    assert_eq!(
        r.decode_page(5).unwrap_err(),
        Error::PageOutOfRange { page: 5, pages: 5 }
    );
}

/// A tiny independent TIFF writer for compression 1/3, both byte orders, several strips,
/// fill order and photometric options.
struct Spec<'a> {
    width: u32,
    height: u32,
    compression: u16,
    t4_options: Option<u32>,
    photometric: u16,
    fill_order: u16,
    big_endian: bool,
    rows_per_strip: u32,
    unit: u16,
    xres: (u32, u32),
    yres: (u32, u32),
    bm: &'a [u8],
}

fn build(spec: &Spec) -> Vec<u8> {
    let be = spec.big_endian;
    let p16 = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let p32 = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let rb = rb(spec.width);
    let mut strips: Vec<Vec<u8>> = Vec::new();
    let mut y = 0;
    while y < spec.height {
        let rows = spec.rows_per_strip.min(spec.height - y);
        let part = &spec.bm[y as usize * rb..(y + rows) as usize * rb];
        // The data is stored in the file's photometric polarity.
        let stored: Vec<u8> = part
            .iter()
            .map(|b| if spec.photometric == 1 { !b } else { *b })
            .collect();
        let data = match spec.compression {
            1 => stored,
            3 => {
                let coding = if spec.t4_options == Some(1) {
                    Coding::Mr { k: 2 }
                } else {
                    Coding::Mh
                };
                let mut e = Encoder::new(spec.width, coding).unwrap();
                for r in part.chunks(rb) {
                    e.push_row(r).unwrap();
                }
                e.finish()
            }
            _ => unreachable!(),
        };
        let data = if spec.fill_order == 2 {
            data.iter().map(|b| b.reverse_bits()).collect()
        } else {
            data
        };
        strips.push(data);
        y += rows;
    }
    let mut out = if be { b"MM".to_vec() } else { b"II".to_vec() };
    out.extend_from_slice(&p16(42));
    out.extend_from_slice(&p32(0));
    let mut offs = Vec::new();
    for s in &strips {
        offs.push(out.len() as u32);
        out.extend_from_slice(s);
        if out.len() % 2 == 1 {
            out.push(0);
        }
    }
    let offs_at = out.len() as u32;
    for o in &offs {
        out.extend_from_slice(&p32(*o));
    }
    let counts_at = out.len() as u32;
    for s in &strips {
        out.extend_from_slice(&p32(s.len() as u32));
    }
    let xr = out.len() as u32;
    out.extend_from_slice(&p32(spec.xres.0));
    out.extend_from_slice(&p32(spec.xres.1));
    let yr = out.len() as u32;
    out.extend_from_slice(&p32(spec.yres.0));
    out.extend_from_slice(&p32(spec.yres.1));
    let ifd = out.len() as u32;
    out[4..8].copy_from_slice(&p32(ifd));
    let n = strips.len() as u32;
    let mut entries: Vec<(u16, u16, u32, u32)> = vec![
        (256, 4, 1, spec.width),
        (257, 4, 1, spec.height),
        (258, 3, 1, 1),
        (259, 3, 1, u32::from(spec.compression)),
        (262, 3, 1, u32::from(spec.photometric)),
        (266, 3, 1, u32::from(spec.fill_order)),
        (273, 4, n, if n == 1 { offs[0] } else { offs_at }),
        (277, 3, 1, 1),
        (278, 4, 1, spec.rows_per_strip),
        (
            279,
            4,
            n,
            if n == 1 {
                strips[0].len() as u32
            } else {
                counts_at
            },
        ),
        (282, 5, 1, xr),
        (283, 5, 1, yr),
    ];
    if let Some(o) = spec.t4_options {
        entries.push((292, 4, 1, o));
    }
    entries.push((296, 3, 1, u32::from(spec.unit)));
    out.extend_from_slice(&p16(entries.len() as u16));
    for (tag, typ, count, val) in entries {
        out.extend_from_slice(&p16(tag));
        out.extend_from_slice(&p16(typ));
        out.extend_from_slice(&p32(count));
        if typ == 3 && count == 1 {
            // SHORT value sits in the first two bytes of the field
            out.extend_from_slice(&p16(val as u16));
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&p32(val));
        }
    }
    out.extend_from_slice(&p32(0));
    out
}

fn spec<'a>(bm: &'a [u8], width: u32, height: u32) -> Spec<'a> {
    Spec {
        width,
        height,
        compression: 3,
        t4_options: Some(0),
        photometric: 0,
        fill_order: 1,
        big_endian: false,
        rows_per_strip: height,
        unit: 2,
        xres: (204, 1),
        yres: (98, 1),
        bm,
    }
}

#[test]
fn p01_reader_handles_mh_mr_uncompressed_strips_endianness_fillorder_photometric() {
    let (w, h) = (1728u32, 90u32);
    let all = bitmaps(w, h);
    let mut checked = 0;
    for (name, bm) in &all {
        for compression in [1u16, 3] {
            for t4 in [None, Some(0u32), Some(1)] {
                if compression == 1 && t4.is_some() {
                    continue;
                }
                for be in [false, true] {
                    for fill in [1u16, 2] {
                        for photo in [0u16, 1] {
                            for rps in [h, 25] {
                                let mut s = spec(bm, w, h);
                                s.compression = compression;
                                s.t4_options = t4;
                                s.big_endian = be;
                                s.fill_order = fill;
                                s.photometric = photo;
                                s.rows_per_strip = rps;
                                let tiff = build(&s);
                                let ctx = format!(
                                    "{name} comp={compression} t4={t4:?} be={be} fill={fill} photo={photo} rps={rps}"
                                );
                                let r = TiffReader::new(&tiff).expect(&ctx);
                                assert_eq!(r.page_count(), 1, "{ctx}");
                                let page = r.decode_page(0).expect(&ctx);
                                assert_eq!(&page.data, bm, "pixels: {ctx}");
                                assert_eq!(page.bad_lines, 0, "{ctx}");
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    // 5 bitmaps x (uncompressed + G3 without T4Options + MH + MR) x 2 byte orders x 2 fill orders x 2 photometrics x 2 strip layouts
    assert_eq!(checked, 5 * 4 * 2 * 2 * 2 * 2, "case count");
}

#[test]
fn p01_reader_converts_centimetre_resolution_to_dpi() {
    let (w, h) = (64u32, 16u32);
    let bm = vec![0u8; rb(w) * h as usize];
    let mut s = spec(&bm, w, h);
    s.unit = 3;
    s.xres = (803, 10); // 80.3 dots/cm = 203.96 dpi
    s.yres = (385, 10); // 38.5 dots/cm = 97.79 dpi
    let tiff = build(&s);
    let info = TiffReader::new(&tiff).unwrap().info(0).unwrap();
    assert_eq!((info.x_dpi, info.y_dpi), (204, 98));
}

#[test]
fn p04_reader_rejects_malformed_and_unsupported_files_with_errors() {
    let (w, h) = (128u32, 32u32);
    let bm = &bitmaps(w, h)[3].1;
    let good = g4_tiff(&[(w, h, 204, 98, bm)]);
    assert!(TiffReader::new(&good).is_ok());

    assert_eq!(
        TiffReader::new(b"").unwrap_err(),
        Error::InvalidTiff("not a TIFF file")
    );
    assert_eq!(
        TiffReader::new(b"%PDF-1.4 not a tiff").unwrap_err(),
        Error::InvalidTiff("not a TIFF file")
    );
    let mut bad_magic = good.clone();
    bad_magic[2] = 43;
    assert_eq!(
        TiffReader::new(&bad_magic).unwrap_err(),
        Error::InvalidTiff("bad TIFF magic")
    );
    // IFD offset beyond the file
    let mut far = good.clone();
    far[4..8].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
    assert!(matches!(
        TiffReader::new(&far).unwrap_err(),
        Error::InvalidTiff(_)
    ));
    // zero offset = no IFD
    let mut none = good.clone();
    none[4..8].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        TiffReader::new(&none).unwrap_err(),
        Error::InvalidTiff("no pages")
    );
    // an IFD whose next pointer points at itself
    let ifd = u32::from_le_bytes(good[4..8].try_into().unwrap()) as usize;
    let n = u16::from_le_bytes(good[ifd..ifd + 2].try_into().unwrap()) as usize;
    let mut looped = good.clone();
    looped[ifd + 2 + n * 12..ifd + 6 + n * 12].copy_from_slice(&(ifd as u32).to_le_bytes());
    assert_eq!(
        TiffReader::new(&looped).unwrap_err(),
        Error::InvalidTiff("IFD chain loops")
    );
    // compression 2 (CCITT RLE) is reported, not mis-decoded
    let mut rle = build(&spec(bm, w, h));
    let comp_at = find_entry(&rle, 259);
    rle[comp_at + 8] = 2;
    assert_eq!(
        TiffReader::new(&rle).unwrap_err(),
        Error::UnsupportedTiff("compression")
    );
    // strip table beyond end of file
    let mut cut = good.clone();
    cut.truncate(40);
    assert!(TiffReader::new(&cut).is_err());
}

/// Offset of the IFD entry for `tag` in a little endian single page file.
fn find_entry(t: &[u8], tag: u16) -> usize {
    let ifd = u32::from_le_bytes(t[4..8].try_into().unwrap()) as usize;
    let n = u16::from_le_bytes(t[ifd..ifd + 2].try_into().unwrap()) as usize;
    (0..n)
        .map(|i| ifd + 2 + i * 12)
        .find(|e| u16::from_le_bytes(t[*e..*e + 2].try_into().unwrap()) == tag)
        .unwrap()
}

#[test]
fn p04_reader_never_panics_on_truncation_or_bit_flips_and_reports_garbled_data() {
    let (w, h) = (256u32, 40u32);
    let bm = &bitmaps(w, h)[3].1;
    let good = g4_tiff(&[(w, h, 204, 98, bm), (w, h, 204, 98, bm)]);
    // every prefix
    for len in 0..good.len() {
        if let Ok(r) = TiffReader::new(&good[..len]) {
            for i in 0..r.page_count() + 1 {
                let _ = r.decode_page(i);
            }
        }
    }
    // every single byte replaced by 0xFF / 0x00
    for i in 0..good.len() {
        for v in [0x00u8, 0xFF, 0x55] {
            let mut t = good.clone();
            t[i] = v;
            if let Ok(r) = TiffReader::new(&t) {
                for p in 0..r.page_count() {
                    let _ = r.decode_page(p);
                }
            }
        }
    }
    // image data replaced by zeros: no rows decode at all -> an error, not an empty bitmap
    let g4_len = encode_mmr(bm, w).unwrap().len();
    let mut noisy = good.clone();
    for b in &mut noisy[8..8 + g4_len] {
        *b = 0x00;
    }
    let r = TiffReader::new(&noisy).unwrap();
    assert!(r.decode_page(0).is_err(), "an all-zero strip has no rows");
    assert!(r.decode_page(1).is_ok(), "the second page is untouched");
}

#[test]
fn p04_reader_pads_a_truncated_strip_with_white_and_counts_missing_rows() {
    let (w, h) = (1728u32, 80u32);
    let bm = &bitmaps(w, h)[4].1; // noise: every row matters
    let full = encode_mmr(bm, w).unwrap();
    let cut = &full[..full.len() / 2];
    let page = Page {
        width: w,
        height: h,
        x_dpi: 204,
        y_dpi: 98,
        g4: cut,
        bad_lines: 0,
        longest_bad_run: 0,
    };
    let tiff = write_tiff(&[page], &meta()).unwrap();
    let r = TiffReader::new(&tiff).unwrap();
    let bmp = r.decode_page(0).unwrap();
    assert_eq!(bmp.data.len(), bm.len());
    assert!(bmp.bad_lines > 0 && bmp.bad_lines < h, "{}", bmp.bad_lines);
    let good_rows = (h - bmp.bad_lines) as usize;
    assert_eq!(&bmp.data[..good_rows * rb(w)], &bm[..good_rows * rb(w)]);
    assert!(
        bmp.data[good_rows * rb(w)..].iter().all(|b| *b == 0),
        "missing rows are white"
    );
}

// ---------------------------------------------------------------- P01 (Rust-side decode)

fn bitmap(w: u32, h: u32, x: u32, y: u32, data: Vec<u8>) -> Bitmap {
    Bitmap {
        width: w,
        height: h,
        x_dpi: x,
        y_dpi: y,
        data,
        bad_lines: 0,
    }
}

#[test]
fn p01_full_png_standard_repeats_every_row_twice_and_fine_does_not() {
    for width in [1728u32, 2048, 2432, 1001] {
        for (name, bm) in bitmaps(width, 70) {
            // standard: 204 x 98
            let std_png = png::encode_full(&bitmap(width, 70, 204, 98, bm.clone())).unwrap();
            let (w, h, depth, px) = decode_png(&std_png);
            assert_eq!((w, h, depth), (width, 140, Depth::One), "{name}");
            let line = rb(width);
            assert_eq!(px.len(), line * 140);
            for y in 0..70u32 {
                for dup in 0..2 {
                    for x in 0..width {
                        let png_black = px[(2 * y + dup) as usize * line + (x >> 3) as usize]
                            & (0x80 >> (x & 7))
                            == 0;
                        assert_eq!(
                            png_black,
                            pixel(&bm, width, x, y),
                            "{name} std {width} ({x},{y}) dup {dup}"
                        );
                    }
                }
            }
            // fine: 204 x 196
            let fine_png = png::encode_full(&bitmap(width, 70, 204, 196, bm.clone())).unwrap();
            let (w, h, depth, px) = decode_png(&fine_png);
            assert_eq!((w, h, depth), (width, 70, Depth::One), "{name}");
            for y in 0..70u32 {
                for x in 0..width {
                    let png_black =
                        px[y as usize * line + (x >> 3) as usize] & (0x80 >> (x & 7)) == 0;
                    assert_eq!(png_black, pixel(&bm, width, x, y), "{name} fine ({x},{y})");
                }
            }
        }
    }
}

#[test]
fn p01_row_repeat_rule() {
    assert_eq!(png::row_repeat(204, 98), 2);
    assert_eq!(png::row_repeat(204, 196), 1);
    assert_eq!(png::row_repeat(408, 196), 2);
    assert_eq!(png::row_repeat(204, 136), 2, "exactly 1.5 doubles");
    assert_eq!(png::row_repeat(204, 137), 1);
    assert_eq!(
        png::row_repeat(0, 98),
        1,
        "unknown resolution is left alone"
    );
    assert_eq!(png::row_repeat(204, 0), 1);
}

#[test]
fn p01_tiff_to_png_end_to_end_for_both_resolutions() {
    let (w, h) = (1728u32, 150u32);
    let bm = bitmaps(w, h)[3].1.clone();
    for (ydpi, rows_out) in [(98u32, 300u32), (196, 150)] {
        let tiff = g4_tiff(&[(w, h, 204, ydpi, &bm)]);
        let r = TiffReader::new(&tiff).unwrap();
        let page = r.decode_page(0).unwrap();
        let (pw, ph, _, px) = decode_png(&png::encode_full(&page).unwrap());
        assert_eq!((pw, ph), (w, rows_out));
        let rep = (rows_out / h) as usize;
        for y in 0..rows_out as usize {
            for x in 0..w {
                let black = px[y * rb(w) + (x >> 3) as usize] & (0x80 >> (x & 7)) == 0;
                assert_eq!(black, pixel(&bm, w, x, (y / rep) as u32));
            }
        }
    }
}

// ---------------------------------------------------------------- P02

/// Straightforward reference: build the aspect-corrected image pixel by pixel (0 / 255), then
/// for every output pixel sum `overlap_x * overlap_y * value` over the source pixels it
/// covers. No shared code with the implementation.
fn reference_thumb(bm: &[u8], w: u32, h: u32, rep: u32) -> (u32, u32, Vec<u8>) {
    let hc = h * rep;
    let tw = 300u32;
    let th = ((u64::from(hc) * 300 + u64::from(w) / 2) / u64::from(w)).max(1) as u32;
    let value = |x: u32, yc: u32| -> f64 {
        if get_px(bm, w, x, yc / rep) {
            0.0
        } else {
            255.0
        }
    };
    let sx = f64::from(w) / f64::from(tw);
    let sy = f64::from(hc) / f64::from(th);
    let mut out = Vec::new();
    for oy in 0..th {
        for ox in 0..tw {
            let (x0, x1) = (f64::from(ox) * sx, f64::from(ox + 1) * sx);
            let (y0, y1) = (f64::from(oy) * sy, f64::from(oy + 1) * sy);
            let mut sum = 0.0;
            let mut area = 0.0;
            let xs = x0.floor() as u32..(x1.ceil() as u32).min(w);
            for x in xs {
                let ox_len = (f64::from(x + 1)).min(x1) - f64::from(x).max(x0);
                if ox_len <= 0.0 {
                    continue;
                }
                let ys = y0.floor() as u32..(y1.ceil() as u32).min(hc);
                for y in ys {
                    let oy_len = (f64::from(y + 1)).min(y1) - f64::from(y).max(y0);
                    if oy_len <= 0.0 {
                        continue;
                    }
                    sum += ox_len * oy_len * value(x, y);
                    area += ox_len * oy_len;
                }
            }
            out.push((sum / area).round().clamp(0.0, 255.0) as u8);
        }
    }
    (tw, th, out)
}

fn assert_thumb_matches(width: u32, height: u32, ydpi: u32, bm: &[u8], ctx: &str) {
    let rep = if ydpi == 98 { 2 } else { 1 };
    let (rw, rh, reference) = reference_thumb(bm, width, height, rep);
    let (tw, th, px) = png::thumbnail(&bitmap(width, height, 204, ydpi, bm.to_vec())).unwrap();
    assert_eq!((tw, th), (rw, rh), "{ctx}: size");
    assert_eq!(tw, THUMB_WIDTH);
    let worst = px
        .iter()
        .zip(&reference)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(worst <= 1, "{ctx}: worst difference {worst} gray levels");
}

#[test]
fn p02_thumbnail_matches_independent_area_average_within_one_gray_level() {
    // Non-integer ratios on purpose: 1728/300 = 5.76, 2048/300 = 6.83, 2432/300 = 8.1067,
    // 1000/300 = 3.33, and 299 / 301 pixels (down- and up-scaling around 300).
    for (width, height) in [
        (1728u32, 250u32),
        (2048, 200),
        (2432, 180),
        (1000, 77),
        (301, 41),
        (299, 41),
        (64, 20),
    ] {
        for (name, bm) in bitmaps(width, height) {
            for ydpi in [98u32, 196] {
                assert_thumb_matches(
                    width,
                    height,
                    ydpi,
                    &bm,
                    &format!("{name} {width}x{height} y={ydpi}"),
                );
            }
        }
    }
}

#[test]
fn p02_thumbnail_size_follows_corrected_aspect_ratio() {
    // 1728 x 1143 standard resolution is an A4 page: 1143 rows * 2 = 2286 corrected rows.
    let a4_std = bitmap(1728, 1143, 204, 98, vec![0; rb(1728) * 1143]);
    assert_eq!(png::thumb_size(&a4_std), (300, 397));
    let a4_fine = bitmap(1728, 2286, 204, 196, vec![0; rb(1728) * 2286]);
    assert_eq!(png::thumb_size(&a4_fine), (300, 397));
    // The thumbnail PNG itself carries that size and 8-bit depth.
    let (w, h, depth, px) = decode_png(&png::encode_thumb(&a4_std).unwrap());
    assert_eq!((w, h, depth), (300, 397, Depth::Eight));
    assert_eq!(px.len(), 300 * 397);
    assert!(px.iter().all(|v| *v == 255), "a white page stays white");
    // A very flat page still gets one row.
    assert_eq!(
        png::thumb_size(&bitmap(2432, 1, 204, 196, vec![0; rb(2432)])).1,
        1
    );
}

#[test]
fn p02_thumbnail_of_solid_and_half_pages_has_exact_values() {
    let (w, h) = (1728u32, 100u32);
    let n = rb(w) * h as usize;
    let (_, _, white) = png::thumbnail(&bitmap(w, h, 204, 196, vec![0; n])).unwrap();
    assert!(white.iter().all(|v| *v == 255));
    assert_eq!(w % 8, 0, "no padding bits to keep clear");
    let black = vec![0xFFu8; n];
    let (_, _, px) = png::thumbnail(&bitmap(w, h, 204, 196, black)).unwrap();
    assert!(px.iter().all(|v| *v == 0));
    // left half black, right half white: columns 0..149 are 0, 150..299 are 255
    let mut half = vec![0u8; n];
    for y in 0..h {
        for x in 0..w / 2 {
            set_px(&mut half, w, x, y);
        }
    }
    let (_, th, px) = png::thumbnail(&bitmap(w, h, 204, 196, half)).unwrap();
    for y in 0..th as usize {
        assert!(px[y * 300..y * 300 + 150].iter().all(|v| *v == 0));
        assert!(px[y * 300 + 150..y * 300 + 300].iter().all(|v| *v == 255));
    }
}

#[test]
fn p04_png_encoders_reject_inconsistent_bitmaps() {
    let bad = bitmap(100, 10, 204, 98, vec![0; 5]);
    assert!(png::encode_full(&bad).is_err());
    assert!(png::encode_thumb(&bad).is_err());
    assert!(png::thumbnail(&bitmap(0, 10, 204, 98, vec![])).is_err());
    assert!(png::encode_bilevel(8, 0, &[]).is_err());
}

// ---------------------------------------------------------------- properties

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Any bitmap survives bitmap -> 1-bit PNG -> decode unchanged (pixels only; padding bits
    /// are not part of the image).
    #[test]
    fn p01_prop_png_roundtrip_any_size(
        width in 1u32..300,
        height in 1u32..40,
        seed in any::<u64>(),
        density in 0u8..=255,
    ) {
        let line = rb(width);
        let mut rng = Rng(seed | 1);
        let mut bm = vec![0u8; line * height as usize];
        for y in 0..height {
            for x in 0..width {
                if (rng.next() >> 32) as u8 <= density {
                    set_px(&mut bm, width, x, y);
                }
            }
        }
        let enc = png::encode_bilevel(width, height, &bm).unwrap();
        let (w, h, depth, px) = decode_png(&enc);
        prop_assert_eq!((w, h, depth), (width, height, Depth::One));
        for y in 0..height {
            for x in 0..width {
                let black = px[y as usize * line + (x >> 3) as usize] & (0x80 >> (x & 7)) == 0;
                prop_assert_eq!(black, get_px(&bm, width, x, y));
            }
        }
    }

    /// bitmap -> MMR -> TIFF -> reader returns the same bitmap, and the 98 dpi PNG has every
    /// row twice.
    #[test]
    fn p01_prop_tiff_reader_png_pipeline(
        width in 1u32..200,
        height in 1u32..30,
        seed in any::<u64>(),
        std_res in any::<bool>(),
    ) {
        let line = rb(width);
        let mut rng = Rng(seed | 1);
        let mut bm = vec![0u8; line * height as usize];
        for y in 0..height {
            for x in 0..width {
                if rng.next() >> 61 == 0 {
                    set_px(&mut bm, width, x, y);
                }
            }
        }
        let ydpi = if std_res { 98 } else { 196 };
        let tiff = g4_tiff(&[(width, height, 204, ydpi, &bm)]);
        let r = TiffReader::new(&tiff).unwrap();
        let page = r.decode_page(0).unwrap();
        prop_assert_eq!(&page.data, &bm);
        let (_, h, _, px) = decode_png(&png::encode_full(&page).unwrap());
        let rep = if std_res { 2 } else { 1 };
        prop_assert_eq!(h, height * rep);
        for y in 0..h {
            for x in 0..width {
                let black = px[y as usize * line + (x >> 3) as usize] & (0x80 >> (x & 7)) == 0;
                prop_assert_eq!(black, get_px(&bm, width, x, y / rep));
            }
        }
    }

    /// Arbitrary bytes never panic the reader; whatever parses can be decoded or errors cleanly.
    #[test]
    fn p04_prop_reader_never_panics_on_random_bytes(
        data in proptest::collection::vec(any::<u8>(), 0..600),
        with_header in any::<bool>(),
    ) {
        let mut d = data;
        if with_header && d.len() >= 8 {
            d[..4].copy_from_slice(b"II*\0");
            d[4..8].copy_from_slice(&8u32.to_le_bytes());
        }
        if let Ok(r) = TiffReader::new(&d) {
            for i in 0..r.page_count() {
                let _ = r.decode_page(i);
            }
        }
    }

    /// Thumbnails of arbitrary small pages stay within one gray level of the reference.
    #[test]
    fn p02_prop_thumbnail_close_to_reference(
        width in 1u32..120,
        height in 1u32..25,
        seed in any::<u64>(),
        std_res in any::<bool>(),
    ) {
        let line = rb(width);
        let mut rng = Rng(seed | 1);
        let mut bm = vec![0u8; line * height as usize];
        for y in 0..height {
            for x in 0..width {
                if rng.next() >> 62 == 0 {
                    set_px(&mut bm, width, x, y);
                }
            }
        }
        let rep = if std_res { 2 } else { 1 };
        let (_, rh, reference) = reference_thumb(&bm, width, height, rep);
        let (_, th, px) = png::thumbnail(&bitmap(width, height, 204, if std_res { 98 } else { 196 }, bm.clone())).unwrap();
        prop_assert_eq!(th, rh);
        let worst = px.iter().zip(&reference).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        prop_assert!(worst <= 1, "worst {}", worst);
    }
}
