//! P01 oracle: the PNG written by `t4::png` is decoded by libpng (`pngtopnm`, netpbm) and
//! compared pixel by pixel with what libtiff (`tiffcp`, `tifftopnm`) decodes from the same
//! TIFF. Neither tool shares any code with this crate.
//!
//! Like `oracle.rs` the tools are required: a missing tool FAILS the test; `T4_ORACLE=skip`
//! skips (host machines without them). CI runs these in the Rocky 9 test image
//! (`scripts/e2e-docker.sh`).

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use t4::png;
use t4::{Page, TiffMeta, TiffReader, encode_mmr, write_tiff};

fn skip_or_require(tools: &[&str]) -> bool {
    if std::env::var("T4_ORACLE").as_deref() == Ok("skip") {
        eprintln!("T4_ORACLE=skip: oracle test skipped");
        return true;
    }
    for t in tools {
        // `tifftopnm --version` and friends exit non-zero; spawning is what matters.
        if let Err(e) = Command::new(t).arg("--help").output() {
            panic!(
                "oracle tool `{t}` is not available ({e}); install libtiff-tools / netpbm-progs \
                 or run the tests with T4_ORACLE=skip"
            );
        }
    }
    false
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("t4-pngoracle-{}-{name}", std::process::id()));
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

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn run(tool: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    assert!(
        out.status.success(),
        "{tool} {args:?} failed ({}):\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A decoded image from a PNM stream: 0 = black, 255 = white (bilevel inputs are scaled).
struct Pnm {
    width: usize,
    height: usize,
    gray: Vec<u8>,
}

/// Minimal P4 (PBM) / P5 (PGM) parser: all that `tifftopnm` / `pngtopnm` produce here.
fn parse_pnm(d: &[u8]) -> Pnm {
    let mut pos = 0;
    let mut token = |d: &[u8]| -> String {
        loop {
            while pos < d.len() && d[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < d.len() && d[pos] == b'#' {
                while pos < d.len() && d[pos] != b'\n' {
                    pos += 1;
                }
            } else {
                break;
            }
        }
        let start = pos;
        while pos < d.len() && !d[pos].is_ascii_whitespace() {
            pos += 1;
        }
        String::from_utf8_lossy(&d[start..pos]).into_owned()
    };
    let magic = token(d);
    let width: usize = token(d).parse().unwrap();
    let height: usize = token(d).parse().unwrap();
    match magic.as_str() {
        "P4" => {
            pos += 1; // single whitespace after the header
            let line = width.div_ceil(8);
            let mut gray = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let bit = d[pos + y * line + x / 8] & (0x80 >> (x % 8)) != 0;
                    gray.push(if bit { 0 } else { 255 }); // PBM: 1 = black
                }
            }
            Pnm {
                width,
                height,
                gray,
            }
        }
        "P5" => {
            let maxval: usize = token(d).parse().unwrap();
            assert!(maxval <= 255);
            pos += 1;
            let gray = d[pos..pos + width * height]
                .iter()
                .map(|v| (usize::from(*v) * 255 / maxval) as u8)
                .collect();
            Pnm {
                width,
                height,
                gray,
            }
        }
        other => panic!("unexpected PNM magic {other}"),
    }
}

fn meta() -> TiffMeta<'static> {
    TiffMeta {
        software: "sip-fax-rs png oracle",
        datetime: "2026:10:21 08:30:00",
        image_description: None,
    }
}

/// libtiff's decode of page `page` (1-based) of `tiff`.
fn libtiff_page(t: &Tmp, tiff: &Path, page: usize) -> Pnm {
    let one = t.path("one.tif");
    let sel = format!("{},{}", p(tiff), page - 1);
    run("tiffcp", &["-c", "none", &sel, p(&one)]);
    parse_pnm(&run("tifftopnm", &[p(&one)]))
}

/// libpng's decode of a PNG held in memory.
fn libpng(t: &Tmp, png_bytes: &[u8]) -> Pnm {
    let f = t.path("page.png");
    std::fs::write(&f, png_bytes).unwrap();
    parse_pnm(&run("pngtopnm", &[p(&f)]))
}

/// `tiff_path` is what t4 reads; `truth_path` is the file libtiff decodes for the reference
/// (the same file, or the uncompressed source when libtiff itself cannot re-read its own
/// pathological G3 output: a 1-pixel checkerboard overflows its 1-D run buffer).
fn check_page(t: &Tmp, tiff_path: &Path, truth_path: &Path, page: usize, ctx: &str) {
    eprintln!("checking {ctx}");
    let data = std::fs::read(tiff_path).unwrap();
    let reader = TiffReader::new(&data).unwrap_or_else(|e| panic!("{ctx}: reader: {e}"));
    let bmp = reader
        .decode_page(page - 1)
        .unwrap_or_else(|e| panic!("{ctx}: decode: {e}"));
    let truth = libtiff_page(t, truth_path, page);
    assert_eq!(
        (bmp.width as usize, bmp.height as usize),
        (truth.width, truth.height),
        "{ctx}: size"
    );
    // our own TIFF reader against libtiff first, so a failure points at the right layer
    for y in 0..truth.height {
        for x in 0..truth.width {
            assert_eq!(
                get_px(&bmp.data, bmp.width, x as u32, y as u32),
                truth.gray[y * truth.width + x] == 0,
                "{ctx}: t4 reader vs libtiff at ({x},{y})"
            );
        }
    }

    let rep = if bmp.y_dpi > 0 && bmp.x_dpi >= bmp.y_dpi * 3 / 2 {
        2
    } else {
        1
    };
    let full = png::encode_full(&bmp).unwrap();
    let got = libpng(t, &full);
    assert_eq!(got.width, truth.width, "{ctx}: png width");
    assert_eq!(got.height, truth.height * rep, "{ctx}: png height");
    for y in 0..got.height {
        for x in 0..got.width {
            let png_px = got.gray[y * got.width + x];
            assert!(png_px == 0 || png_px == 255, "{ctx}: not bilevel: {png_px}");
            assert_eq!(
                png_px,
                truth.gray[(y / rep) * truth.width + x],
                "{ctx}: PNG vs TIFF at ({x},{y}), row repeat {rep}"
            );
        }
    }
    if rep == 2 {
        for y in (0..got.height).step_by(2) {
            assert_eq!(
                got.gray[y * got.width..(y + 1) * got.width],
                got.gray[(y + 1) * got.width..(y + 2) * got.width],
                "{ctx}: PNG rows {y} and {} differ",
                y + 1
            );
        }
    }

    // thumbnail: libpng must decode exactly what the thumbnailer produced
    let (tw, th, px) = png::thumbnail(&bmp).unwrap();
    let thumb = libpng(t, &png::encode_thumb(&bmp).unwrap());
    assert_eq!(
        (thumb.width as u32, thumb.height as u32),
        (tw, th),
        "{ctx}: thumb size"
    );
    assert_eq!(thumb.gray, px, "{ctx}: thumb pixels as decoded by libpng");
}

fn write_source(t: &Tmp, name: &str, pages: &[(u32, u32, u32, &[u8])]) -> PathBuf {
    // (width, rows, y_dpi, bitmap): independent uncompressed TIFF from common/
    let path = t.path(name);
    std::fs::write(&path, uncompressed_tiff(pages)).unwrap();
    path
}

#[test]
fn p01_png_of_our_g4_tiff_matches_libtiff_pixel_for_pixel_standard_and_fine() {
    if skip_or_require(&["tiffcp", "tifftopnm", "pngtopnm"]) {
        return;
    }
    let t = Tmp::new("g4");
    let mut checked = 0;
    for width in [1728u32, 2048, 2432, 1001] {
        for (name, bm) in bitmaps(width, 120) {
            for ydpi in [98u32, 196] {
                let g4 = encode_mmr(&bm, width).unwrap();
                let page = Page {
                    width,
                    height: 120,
                    x_dpi: 204,
                    y_dpi: ydpi,
                    g4: &g4,
                    bad_lines: 0,
                    longest_bad_run: 0,
                };
                let path = t.path("ours.tif");
                std::fs::write(&path, write_tiff(&[page], &meta()).unwrap()).unwrap();
                check_page(
                    &t,
                    &path,
                    &path,
                    1,
                    &format!("ours-g4 {name} {width} y={ydpi}"),
                );
                checked += 1;
            }
        }
    }
    eprintln!("P01: {checked} t4-written pages: libpng(PNG) == libtiff(TIFF)");
}

#[test]
fn p01_png_of_libtiff_encoded_mh_mr_mmr_matches_libtiff_pixel_for_pixel() {
    if skip_or_require(&["tiffcp", "tifftopnm", "pngtopnm"]) {
        return;
    }
    let t = Tmp::new("libtiff-codings");
    let mut checked = 0;
    for width in [1728u32, 2048, 1001] {
        for (name, bm) in bitmaps(width, 100) {
            for ydpi in [98u32, 196] {
                let src = write_source(&t, "src.tif", &[(width, 100, ydpi, &bm)]);
                for spec in ["none", "g3:1d", "g3:1d:fill", "g3:2d", "g3:2d:fill", "g4"] {
                    for rps in [None, Some("30")] {
                        let dst = t.path("coded.tif");
                        let mut args = vec!["-c", spec];
                        if let Some(r) = rps {
                            args.extend(["-r", r]);
                        }
                        args.extend([p(&src), p(&dst)]);
                        run("tiffcp", &args);
                        check_page(
                            &t,
                            &dst,
                            &src,
                            1,
                            &format!("libtiff {spec} rps={rps:?} {name} {width} y={ydpi}"),
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    eprintln!("P01: {checked} libtiff-encoded pages: libpng(PNG) == libtiff(TIFF)");
}

#[test]
fn p01_png_of_every_page_of_a_multipage_tiff_matches_libtiff() {
    if skip_or_require(&["tiffcp", "tifftopnm", "pngtopnm"]) {
        return;
    }
    let t = Tmp::new("multipage");
    let width = 1728u32;
    let all = bitmaps(width, 64);
    let encoded: Vec<Vec<u8>> = all
        .iter()
        .map(|(_, bm)| encode_mmr(bm, width).unwrap())
        .collect();
    let pages: Vec<Page> = encoded
        .iter()
        .enumerate()
        .map(|(i, g4)| Page {
            width,
            height: 64,
            x_dpi: 204,
            y_dpi: if i % 2 == 0 { 98 } else { 196 },
            g4,
            bad_lines: 0,
            longest_bad_run: 0,
        })
        .collect();
    let path = t.path("multi.tif");
    std::fs::write(&path, write_tiff(&pages, &meta()).unwrap()).unwrap();
    let data = std::fs::read(&path).unwrap();
    assert_eq!(TiffReader::new(&data).unwrap().page_count(), 5);
    for page in 1..=5 {
        check_page(&t, &path, &path, page, &format!("multipage page {page}"));
    }
}
