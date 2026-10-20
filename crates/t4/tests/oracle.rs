//! Oracle tests: libtiff (`tiffcp`, `tiffcmp`, `tiffinfo`) and poppler (`pdfinfo`, `pdftoppm`)
//! are independent implementations of T.4/T.6, TIFF and PDF. F01-F03 decode libtiff-encoded
//! data with our decoder, F06-F08 let libtiff and poppler read what we write.
//!
//! The tools are required: a missing tool FAILS the test. Set `T4_ORACLE=skip` to skip these
//! tests (host machines without the tools); CI runs them in the Rocky 9 test image.

mod common;

use common::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use t4::{Coding, DecoderConfig, Page, TiffMeta, decode_page, encode_mmr, write_pdf, write_tiff};

// ---- harness ----

/// True when the tests must be skipped. Panics (fails the test) if a tool is missing.
fn skip_or_require(tools: &[&str]) -> bool {
    if std::env::var("T4_ORACLE").as_deref() == Ok("skip") {
        eprintln!("T4_ORACLE=skip: oracle test skipped");
        return true;
    }
    for t in tools {
        match Command::new(t).output() {
            Ok(_) => {}
            Err(e) => panic!(
                "oracle tool `{t}` is not available ({e}); install libtiff-tools / poppler-utils \
                 or run the tests with T4_ORACLE=skip"
            ),
        }
    }
    false
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("t4-oracle-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self, f: &str) -> PathBuf {
        self.0.join(f)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(tool: &str, args: &[&str]) -> Output {
    let out = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    assert!(
        out.status.success(),
        "{tool} {args:?} failed ({}):\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

// ---- minimal TIFF reader (both byte orders) for libtiff's output ----

type Ifd = HashMap<u16, Vec<u64>>;

fn parse_tiff(d: &[u8]) -> (Vec<Ifd>, bool) {
    let le = &d[..2] == b"II";
    let u16_at = |o: usize| -> u64 {
        let b = [d[o], d[o + 1]];
        u64::from(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |o: usize| -> u64 {
        let b = [d[o], d[o + 1], d[o + 2], d[o + 3]];
        u64::from(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let mut ifds = Vec::new();
    let mut off = u32_at(4) as usize;
    while off != 0 {
        let n = u16_at(off) as usize;
        let mut ifd = Ifd::new();
        for i in 0..n {
            let e = off + 2 + i * 12;
            let tag = u16_at(e) as u16;
            let typ = u16_at(e + 2);
            let count = u32_at(e + 4) as usize;
            let size = match typ {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                _ => 8,
            };
            let at = if count * size <= 4 {
                e + 8
            } else {
                u32_at(e + 8) as usize
            };
            let vals = match typ {
                3 => (0..count).map(|k| u16_at(at + 2 * k)).collect(),
                4 => (0..count).map(|k| u32_at(at + 4 * k)).collect(),
                _ => Vec::new(),
            };
            ifd.insert(tag, vals);
        }
        ifds.push(ifd);
        off = u32_at(off + 2 + n * 12) as usize;
    }
    (ifds, le)
}

struct Decoded {
    width: u32,
    height: u32,
    bitmap: Vec<u8>,
    coding: Option<Coding>,
    strips: usize,
}

/// Decode every strip of page `ifd` with our decoder (or copy raw data for compression 1).
fn read_page(data: &[u8], ifd: &Ifd) -> Decoded {
    let one = |t: u16| ifd.get(&t).and_then(|v| v.first()).copied();
    let width = one(256).unwrap() as u32;
    let height = one(257).unwrap() as u32;
    let compression = one(259).unwrap();
    let fill = one(266).unwrap_or(1);
    assert_eq!(one(262), Some(0), "libtiff output must stay min-is-white");
    let rps = one(278).unwrap_or(u64::from(height)).min(u64::from(height)) as u32;
    let offsets = &ifd[&273];
    let counts = &ifd[&279];
    let coding = match compression {
        1 => None,
        3 => Some(if one(292).unwrap_or(0) & 1 == 1 {
            Coding::Mr { k: 2 }
        } else {
            Coding::Mh
        }),
        4 => Some(Coding::Mmr),
        c => panic!("unexpected compression {c}"),
    };
    let mut bitmap = Vec::new();
    for (i, (o, c)) in offsets.iter().zip(counts).enumerate() {
        let mut strip = data[*o as usize..(*o + *c) as usize].to_vec();
        if fill == 2 {
            for b in &mut strip {
                *b = b.reverse_bits();
            }
        }
        let rows = rps.min(height - i as u32 * rps);
        match coding {
            None => bitmap.extend_from_slice(&strip[..rows as usize * rb(width)]),
            Some(c) => {
                let page = decode_page(&strip, DecoderConfig::new(width, c)).unwrap();
                let s = page.summary;
                assert_eq!(s.stats.rows, rows, "strip {i}: rows ({s:?})");
                assert_eq!(s.stats.bad_lines, 0, "strip {i}: bad lines ({s:?})");
                assert_eq!(s.error, None, "strip {i}: {s:?}");
                bitmap.extend_from_slice(&page.bitmap);
            }
        }
    }
    Decoded {
        width,
        height,
        bitmap,
        coding,
        strips: offsets.len(),
    }
}

/// Longest run of two-dimensionally coded rows (tag bit 0 after an EOL) in strip 0.
fn max_consecutive_2d_rows(path: &Path) -> usize {
    let data = std::fs::read(path).unwrap();
    let (ifds, _) = parse_tiff(&data);
    let (o, c) = (ifds[0][&273][0] as usize, ifds[0][&279][0] as usize);
    let strip = &data[o..o + c];
    let (mut best, mut run) = (0, 0);
    let mut zeros = 0usize;
    let mut i = 0;
    while i < strip.len() * 8 {
        let bit = (strip[i / 8] >> (7 - i % 8)) & 1;
        if bit == 0 {
            zeros += 1;
        } else {
            if zeros >= 11 && i + 1 < strip.len() * 8 {
                let tag = (strip[(i + 1) / 8] >> (7 - (i + 1) % 8)) & 1;
                if tag == 1 {
                    run = 0;
                } else {
                    run += 1;
                    best = best.max(run);
                }
                i += 1;
            }
            zeros = 0;
        }
        i += 1;
    }
    best
}

fn first_page(path: &Path) -> Decoded {
    let data = std::fs::read(path).unwrap();
    let (ifds, _) = parse_tiff(&data);
    assert_eq!(ifds.len(), 1);
    read_page(&data, &ifds[0])
}

// ---- F01-F03: libtiff encodes, we decode ----

fn libtiff_to_ours(width: u32, rows: u32, specs: &[&str], tag: &str) {
    let t = Tmp::new(tag);
    let mut checked = 0;
    for (name, bm) in bitmaps(width, rows) {
        for ydpi in [98u32, 196] {
            let src = t.path("src.tif");
            std::fs::write(&src, uncompressed_tiff(&[(width, rows, ydpi, &bm)])).unwrap();
            for spec in specs {
                for rps in [None, Some("100000")] {
                    let dst = t.path("dst.tif");
                    let mut args = vec!["-c", spec];
                    if let Some(r) = rps {
                        args.extend(["-r", r]);
                    }
                    args.extend([p(&src), p(&dst)]);
                    run("tiffcp", &args);
                    let got = first_page(&dst);
                    let ctx = format!("{name} {width}x{rows} ydpi={ydpi} {spec} rps={rps:?}");
                    assert_eq!((got.width, got.height), (width, rows), "{ctx}");
                    assert!(got.coding.is_some(), "{ctx}");
                    if rps.is_some() {
                        assert_eq!(got.strips, 1, "{ctx}: -r 100000 must give one strip");
                    }
                    assert_eq!(got.bitmap, bm, "pixel mismatch: {ctx}");
                    if spec.contains("2d") {
                        let max_2d = max_consecutive_2d_rows(&dst);
                        let k = if ydpi == 98 { 2 } else { 4 };
                        assert_eq!(max_2d, k - 1, "{ctx}: libtiff K should be {k}");
                    }
                    checked += 1;
                }
            }
        }
    }
    eprintln!("{tag}: {checked} libtiff-encoded pages decoded pixel-exact");
}

#[test]
fn f01_libtiff_g3_1d_is_decoded_pixel_exact() {
    if skip_or_require(&["tiffcp"]) {
        return;
    }
    for width in [1728u32, 2048, 2432] {
        libtiff_to_ours(width, 250, &["g3:1d", "g3:1d:fill"], "f01");
    }
}

#[test]
fn f02_libtiff_g3_2d_is_decoded_pixel_exact() {
    if skip_or_require(&["tiffcp"]) {
        return;
    }
    // y resolution 98 makes libtiff use K=2, 196 makes it use K=4 (tag bits are followed).
    for width in [1728u32, 2048, 2432] {
        libtiff_to_ours(width, 250, &["g3:2d", "g3:2d:fill"], "f02");
    }
}

#[test]
fn f03_libtiff_g4_is_decoded_pixel_exact() {
    if skip_or_require(&["tiffcp"]) {
        return;
    }
    for width in [1728u32, 2048, 2432] {
        libtiff_to_ours(width, 250, &["g4"], "f03");
    }
}

/// Every run length 0..=2600, white-then-black and black-then-white, through libtiff's
/// encoders: checks every code word of our tables against libtiff.
#[test]
fn f01_f03_every_run_length_against_libtiff() {
    if skip_or_require(&["tiffcp"]) {
        return;
    }
    let width = 2600u32;
    let rows = 2 * (width + 1);
    let mut bm = vec![0u8; rb(width) * rows as usize];
    for n in 0..=width {
        for x in n..width {
            set_px(&mut bm, width, x, 2 * n); // white n, black to the end
        }
        for x in 0..n {
            set_px(&mut bm, width, x, 2 * n + 1); // black n, white to the end
        }
    }
    let t = Tmp::new("runs");
    let src = t.path("src.tif");
    std::fs::write(&src, uncompressed_tiff(&[(width, rows, 98, &bm)])).unwrap();
    for spec in ["g3:1d", "g3:2d", "g4"] {
        let dst = t.path("dst.tif");
        run("tiffcp", &["-c", spec, p(&src), p(&dst)]);
        let got = first_page(&dst);
        assert_eq!(got.bitmap, bm, "run-length table check failed for {spec}");
    }
}

// ---- F06: our MMR encoder, libtiff decodes ----

fn meta() -> TiffMeta<'static> {
    TiffMeta {
        software: "sip-fax-rs t4 test",
        datetime: "2026:10:21 08:30:00",
        image_description: Some("TSI: +886 2 2345 6789"),
    }
}

#[test]
fn f06_our_mmr_encoder_is_decoded_by_libtiff() {
    if skip_or_require(&["tiffcp", "tiffcmp"]) {
        return;
    }
    let t = Tmp::new("f06");
    let mut n = 0;
    for width in [1728u32, 2048, 2432] {
        let rows = 250u32;
        for (name, bm) in bitmaps(width, rows) {
            let g4 = encode_mmr(&bm, width).unwrap();
            let page = Page {
                width,
                height: rows,
                x_dpi: 204,
                y_dpi: 196,
                g4: &g4,
                bad_lines: 0,
                longest_bad_run: 0,
            };
            let ours = t.path("ours.tif");
            std::fs::write(&ours, write_tiff(&[page], &meta()).unwrap()).unwrap();
            let raw = t.path("raw.tif");
            run("tiffcp", &["-c", "none", p(&ours), p(&raw)]);
            let got = first_page(&raw);
            assert_eq!(got.coding, None);
            assert_eq!(
                got.bitmap, bm,
                "libtiff decoded our MMR differently: {name} {width}"
            );
            // and tiffcmp against an independently written uncompressed reference
            let reference = t.path("ref.tif");
            std::fs::write(&reference, uncompressed_tiff(&[(width, rows, 196, &bm)])).unwrap();
            run("tiffcmp", &["-t", p(&reference), p(&ours)]);
            n += 1;
        }
    }
    eprintln!("f06: {n} pages encoded by us were decoded pixel-exact by libtiff");
}

// ---- F07: multi-page TIFF ----

#[test]
fn f07_multipage_tiff_is_valid_for_libtiff() {
    if skip_or_require(&["tiffinfo", "tiffcmp", "tiffcp"]) {
        return;
    }
    let t = Tmp::new("f07");
    let all = bitmaps(1728, 180);
    let specs: [(usize, u32, u32); 3] = [(3, 98, 180), (4, 196, 180), (2, 98, 180)];
    let encoded: Vec<Vec<u8>> = specs
        .iter()
        .map(|(i, _, _)| encode_mmr(&all[*i].1, 1728).unwrap())
        .collect();
    let pages: Vec<Page> = specs
        .iter()
        .zip(&encoded)
        .enumerate()
        .map(|(n, ((_, y, h), g4))| Page {
            width: 1728,
            height: *h,
            x_dpi: 204,
            y_dpi: *y,
            g4,
            bad_lines: if n == 1 { 5 } else { 0 },
            longest_bad_run: if n == 1 { 2 } else { 0 },
        })
        .collect();
    let ours = t.path("ours.tif");
    std::fs::write(&ours, write_tiff(&pages, &meta()).unwrap()).unwrap();

    let info = run("tiffinfo", &[p(&ours)]);
    let text = String::from_utf8_lossy(&info.stdout).into_owned();
    let err = String::from_utf8_lossy(&info.stderr).into_owned();
    eprintln!("--- tiffinfo ---\n{text}\n--- stderr ---\n{err}");
    assert!(
        err.trim().is_empty(),
        "libtiff warned about our file: {err}"
    );
    assert_eq!(
        text.matches("TIFF Directory at offset").count(),
        3,
        "page count"
    );
    assert_eq!(text.matches("Compression Scheme: CCITT Group 4").count(), 3);
    assert_eq!(
        text.matches("Image Width: 1728 Image Length: 180").count(),
        3
    );
    assert_eq!(text.matches("Resolution: 204, 98 pixels/inch").count(), 2);
    assert_eq!(text.matches("Resolution: 204, 196 pixels/inch").count(), 1);
    assert_eq!(text.matches("min-is-white").count(), 3);
    assert_eq!(text.matches("msb-to-lsb").count(), 3);
    for n in 0..3 {
        assert!(
            text.contains(&format!("Page Number: {n}-3")),
            "page number {n}"
        );
    }
    assert_eq!(text.matches("Software: sip-fax-rs t4 test").count(), 3);
    assert_eq!(text.matches("2026:10:21 08:30:00").count(), 3);
    assert_eq!(text.matches("TSI: +886 2 2345 6789").count(), 3);
    assert_eq!(text.matches("Bad Fax Lines: 5").count(), 1);
    assert_eq!(text.matches("Consecutive Bad Fax Lines: 2").count(), 1);
    assert_eq!(text.matches("Fax Data: receiver regenerated").count(), 1);

    // tiffcmp against an uncompressed reference built independently. NOTE: without `-t`
    // tiffcmp stops at the first tag difference ("Compression: 1 4") and reports success
    // without looking at a single pixel, so `-t` is essential here.
    let reference = t.path("ref.tif");
    let refs: Vec<(u32, u32, u32, &[u8])> = specs
        .iter()
        .map(|(i, y, h)| (1728, *h, *y, &all[*i].1[..]))
        .collect();
    std::fs::write(&reference, uncompressed_tiff(&refs)).unwrap();
    run("tiffcmp", &["-t", p(&reference), p(&ours)]);

    // Negative control: tiffcmp must notice a single flipped pixel (otherwise the check
    // above would prove nothing).
    let mut bad = all[3].1.clone();
    bad[1000] ^= 0x10;
    let refs: Vec<(u32, u32, u32, &[u8])> = vec![
        (1728, 180, 98, &bad[..]),
        (1728, 180, 196, &all[4].1[..]),
        (1728, 180, 98, &all[2].1[..]),
    ];
    let bad_ref = t.path("bad.tif");
    std::fs::write(&bad_ref, uncompressed_tiff(&refs)).unwrap();
    let cmp = Command::new("tiffcmp")
        .args(["-t", p(&bad_ref), p(&ours)])
        .output()
        .unwrap();
    assert!(
        !cmp.status.success(),
        "tiffcmp must fail on a different image: {cmp:?}"
    );

    // libtiff can also re-encode our pages page by page and we agree on the pixels.
    let raw = t.path("raw.tif");
    run("tiffcp", &["-c", "none", p(&ours), p(&raw)]);
    let data = std::fs::read(&raw).unwrap();
    let (ifds, _) = parse_tiff(&data);
    assert_eq!(ifds.len(), 3);
    for (ifd, (i, _, _)) in ifds.iter().zip(specs) {
        assert_eq!(read_page(&data, ifd).bitmap, all[i].1);
    }
}

// ---- F08: PDF ----

fn parse_pbm(d: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&d[..2], b"P4", "expected a binary PBM");
    let mut i = 2;
    let mut nums = Vec::new();
    while nums.len() < 2 {
        while d[i].is_ascii_whitespace() {
            i += 1;
        }
        if d[i] == b'#' {
            while d[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let s = i;
        while d[i].is_ascii_digit() {
            i += 1;
        }
        nums.push(
            std::str::from_utf8(&d[s..i])
                .unwrap()
                .parse::<u32>()
                .unwrap(),
        );
    }
    i += 1; // single whitespace after the header
    (nums[0], nums[1], d[i..].to_vec())
}

#[test]
fn f08_pdf_is_valid_for_poppler_and_renders_pixel_exact() {
    if skip_or_require(&["pdfinfo", "pdftoppm"]) {
        return;
    }
    let t = Tmp::new("f08");
    let all = bitmaps(1728, 300);
    // (bitmap index, y dpi, rows)
    let specs: [(usize, u32, u32); 5] = [
        (3, 98, 300),
        (4, 196, 300),
        (2, 98, 300),
        (0, 196, 300),
        (1, 98, 300),
    ];
    let encoded: Vec<Vec<u8>> = specs
        .iter()
        .map(|(i, _, _)| encode_mmr(&all[*i].1, 1728).unwrap())
        .collect();
    let pages: Vec<Page> = specs
        .iter()
        .zip(&encoded)
        .map(|((_, y, h), g4)| Page {
            width: 1728,
            height: *h,
            x_dpi: 204,
            y_dpi: *y,
            g4,
            bad_lines: 0,
            longest_bad_run: 0,
        })
        .collect();
    let pdf = t.path("ours.pdf");
    std::fs::write(&pdf, write_pdf(&pages).unwrap()).unwrap();

    let info = run("pdfinfo", &["-f", "1", "-l", "5", p(&pdf)]);
    let text = String::from_utf8_lossy(&info.stdout).into_owned();
    let err = String::from_utf8_lossy(&info.stderr).into_owned();
    eprintln!("--- pdfinfo ---\n{text}\n--- stderr ---\n{err}");
    assert!(err.trim().is_empty(), "poppler complained: {err}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("Pages:") && l.split_whitespace().last() == Some("5"))
    );
    for (n, (_, y, h)) in specs.iter().enumerate() {
        let w_pt = 1728.0 * 72.0 / 204.0;
        let h_pt = f64::from(*h) * 72.0 / f64::from(*y);
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("Page {:4} size:", n + 1)))
            .unwrap_or_else(|| panic!("no size line for page {}", n + 1));
        let mut it = line.split("size:").nth(1).unwrap().split_whitespace();
        let gw: f64 = it.next().unwrap().parse().unwrap();
        assert_eq!(it.next(), Some("x"));
        let gh: f64 = it.next().unwrap().parse().unwrap();
        assert!(
            (gw - w_pt).abs() < 0.01 && (gh - h_pt).abs() < 0.01,
            "page {} size {line:?} expected {w_pt} x {h_pt}",
            n + 1
        );
    }

    for (n, (i, y, h)) in specs.iter().enumerate() {
        let page = (n + 1).to_string();
        let prefix = t.path(&format!("page{n}"));
        run(
            "pdftoppm",
            &[
                "-mono",
                "-rx",
                "204",
                "-ry",
                &y.to_string(),
                "-f",
                &page,
                "-l",
                &page,
                p(&pdf),
                p(&prefix),
            ],
        );
        let out = std::fs::read_dir(&t.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|f| {
                f.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&format!("page{n}-"))
            })
            .expect("pdftoppm output");
        let (w, hh, data) = parse_pbm(&std::fs::read(&out).unwrap());
        assert_eq!((w, hh), (1728, *h), "rendered size of page {}", n + 1);
        assert_eq!(
            data,
            all[*i].1,
            "poppler raster differs from the source bitmap on page {}",
            n + 1
        );
    }
}
