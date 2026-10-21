//! Shared plumbing of the real-binary end-to-end tests (`e2e_m2d.rs`, `e2e_m3d.rs`): a running
//! `sip-faxd`, source TIFFs, the result.json reader and the double pixel comparison of the
//! stored TIFF / PDF (libtiff and poppler, each with a negative control, ADR-025).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use fax_sim::gateway::FaxTx;
use fax_sim::sip_uac::{ReinviteBy, UacConfig, UacReport};
use fax_sim::tiff_writer;
use fax_sim::tiff_writer::{PageSpec, render_bitmap};
use interop::{Faxd, FaxdOptions};
use serde_json::Value;

// ------------------------------------------------------------------ plumbing

/// UDPTL port ranges are handed out per test so that parallel tests never collide.
static NEXT_RANGE: AtomicU16 = AtomicU16::new(31_000);

pub fn media_range(ports: u16) -> String {
    let low = NEXT_RANGE.fetch_add(ports.next_multiple_of(2) * 2 + 2, Ordering::Relaxed);
    let low = low.next_multiple_of(2);
    format!("{low}-{}", low + ports * 2 - 1)
}

pub fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A `sip-faxd` that prints its log when the test fails.
pub struct Server(pub Option<Faxd>);

impl std::ops::Deref for Server {
    type Target = Faxd;
    fn deref(&self) -> &Faxd {
        self.0.as_ref().expect("running")
    }
}

impl Server {
    /// Stops the server (SIGTERM) and returns its complete log.
    pub fn stop(mut self) -> Vec<String> {
        self.0.take().map(Faxd::stop).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some(f) = &self.0 {
                eprintln!(
                    "---- sip-faxd log ----\n{}\n---- end ----",
                    f.log().join("\n")
                );
            }
        }
    }
}

pub fn faxd(name: &str, ports: u16, trunk: &str, call: &str) -> Server {
    faxd_with(name, ports, trunk, call, "")
}

pub fn faxd_with(name: &str, ports: u16, trunk: &str, call: &str, capture_extra: &str) -> Server {
    faxd_full(name, ports, trunk, call, capture_extra, None)
}

/// A server whose `[database] url` is `db_url` (default: its own SQLite file).
pub fn faxd_db(name: &str, ports: u16, trunk: &str, db_url: &str) -> Server {
    faxd_full(name, ports, trunk, "", "", Some(db_url.into()))
}

/// A server with `[[routes]]` (and whatever else `tail` holds) appended to its configuration.
pub fn faxd_routes(name: &str, ports: u16, tail: &str) -> Server {
    let opts = FaxdOptions {
        call: "station_id = \"0287654321\"\n".into(),
        extra: format!(
            "\n[media]\nport_range = \"{}\"\n\n{tail}\n",
            media_range(ports)
        ),
        real_fax: true,
        ..FaxdOptions::default()
    };
    Server(Some(Faxd::start(name, &opts)))
}

fn faxd_full(
    name: &str,
    ports: u16,
    trunk: &str,
    call: &str,
    capture_extra: &str,
    database_url: Option<String>,
) -> Server {
    let opts = FaxdOptions {
        database_url,
        trunk: trunk.into(),
        call: format!("station_id = \"0287654321\"\n{call}"),
        extra: format!("\n[media]\nport_range = \"{}\"\n", media_range(ports)),
        real_fax: true,
        capture_extra: capture_extra.into(),
        ..FaxdOptions::default()
    };
    Server(Some(Faxd::start(name, &opts)))
}

pub fn specs(n: usize, fine: bool, base_height: u32) -> Vec<PageSpec> {
    (0..n)
        .map(|i| {
            let h = base_height + 40 * i as u32;
            if fine {
                PageSpec::fine(h)
            } else {
                PageSpec::standard(h)
            }
        })
        .collect()
}

pub fn source_tiff(dir: &Path, name: &str, pages: &[PageSpec]) -> PathBuf {
    let p = dir.join(name);
    tiff_writer::write_file(&p, pages).unwrap();
    p
}

pub fn uac(f: &Server, by: ReinviteBy, tiff: &Path, ident: &str) -> UacConfig {
    let mut cfg = UacConfig::new(f.addr().parse().unwrap(), by);
    let mut tx = FaxTx::new(tiff.to_path_buf());
    tx.ident = ident.into();
    cfg.fax = Some(tx);
    cfg.wait_for_server = Duration::from_secs(20);
    cfg
}

pub fn run(cmd: &str, args: &[&str]) -> std::process::Output {
    let out = Command::new(cmd).args(args).output().unwrap_or_else(|e| {
        panic!("{cmd}: {e} (the E tests need libtiff-tools and poppler-utils)")
    });
    assert!(
        out.status.success(),
        "{cmd} {args:?} failed ({}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

pub fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// The result.json written by the capture, once it exists.
pub fn capture_dir(f: &Server, call_id: &str) -> PathBuf {
    f.calls_dir.join(fax_in::capture::dir_name(call_id))
}

pub fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn result_json(f: &Server, call_id: &str) -> Value {
    let path = capture_dir(f, call_id).join("result.json");
    wait_for("result.json", || {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    })
}

// ------------------------------------------------------------------ pixel comparison

pub type Ifd = HashMap<u16, Vec<u64>>;

/// Minimal TIFF reader for libtiff's uncompressed output: returns (width, height, bitmap) per
/// page, bitmaps packed MSB first with 1 = black (Photometric MinIsWhite).
pub fn read_uncompressed(d: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
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
    let mut pages = Vec::new();
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
                3 => 2,
                4 => 4,
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
        assert_eq!(
            ifd[&259],
            vec![1],
            "tiffcp -c none output must be uncompressed"
        );
        assert_eq!(ifd[&262], vec![0], "MinIsWhite expected");
        let (w, h) = (ifd[&256][0] as u32, ifd[&257][0] as u32);
        let mut bm = Vec::new();
        for (o, c) in ifd[&273].iter().zip(&ifd[&279]) {
            bm.extend_from_slice(&d[*o as usize..(*o + *c) as usize]);
        }
        pages.push((w, h, bm));
        off = u32_at(off + 2 + n * 12) as usize;
    }
    pages
}

pub fn parse_pbm(d: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&d[..2], b"P4", "expected a binary PBM");
    let mut i = 2;
    let mut nums = Vec::new();
    while nums.len() < 2 {
        while d[i].is_ascii_whitespace() {
            i += 1;
        }
        let st = i;
        while d[i].is_ascii_digit() {
            i += 1;
        }
        nums.push(
            std::str::from_utf8(&d[st..i])
                .unwrap()
                .parse::<u32>()
                .unwrap(),
        );
    }
    i += 1;
    (nums[0], nums[1], d[i..].to_vec())
}

/// What a verification looked at, for the report.
#[derive(Debug)]
pub struct Verified {
    pub pages: usize,
    pub tiff_bytes: usize,
    pub pdf_bytes: usize,
}

/// Compares the TIFF and the PDF named in `fax` with the source pages, with libtiff and poppler.
pub fn verify_files(tag: &str, fax: &Value, src: &Path, pages: &[PageSpec]) -> Verified {
    let tif = PathBuf::from(
        fax["tiff"]
            .as_str()
            .unwrap_or_else(|| panic!("{tag}: no tiff in {fax}")),
    );
    let pdf = PathBuf::from(
        fax["pdf"]
            .as_str()
            .unwrap_or_else(|| panic!("{tag}: no pdf in {fax}")),
    );
    assert!(tif.is_file() && pdf.is_file(), "{tag}: {tif:?} / {pdf:?}");
    assert_eq!(tif.extension().unwrap(), "tif");
    assert_eq!(pdf.file_stem(), tif.file_stem());
    let work = scratch(&format!("verify-{tag}"));

    // ---- (a) libtiff
    let info = run("tiffinfo", &[s(&tif)]);
    let info = String::from_utf8_lossy(&info.stdout).into_owned();
    assert_eq!(
        info.matches("TIFF Directory").count(),
        pages.len(),
        "{tag}: {info}"
    );
    assert_eq!(
        info.matches("Compression Scheme: CCITT Group 4").count(),
        pages.len(),
        "{tag}"
    );
    run("tiffcmp", &["-t", s(src), s(&tif)]);
    // negative control: one flipped pixel in the reference must make tiffcmp -t fail
    let mut bad = std::fs::read(src).unwrap();
    bad[8 + 4000] ^= 0x10;
    let bad_path = work.join("bad-ref.tif");
    std::fs::write(&bad_path, &bad).unwrap();
    let neg = Command::new("tiffcmp")
        .args(["-t", s(&bad_path), s(&tif)])
        .output()
        .unwrap();
    assert!(
        !neg.status.success(),
        "{tag}: negative control: tiffcmp -t accepted a different image\n{}",
        String::from_utf8_lossy(&neg.stdout)
    );
    let raw = work.join("raw.tif");
    run("tiffcp", &["-c", "none", s(&tif), s(&raw)]);
    let decoded = read_uncompressed(&std::fs::read(&raw).unwrap());
    assert_eq!(
        decoded.len(),
        pages.len(),
        "{tag}: pages decoded by libtiff"
    );
    for (i, ((w, h, bm), spec)) in decoded.iter().zip(pages).enumerate() {
        assert_eq!(
            (*w, *h),
            (spec.width, spec.height),
            "{tag}: libtiff size of page {i}"
        );
        let want = render_bitmap(spec, i as u32);
        assert!(
            *bm == want,
            "{tag}: libtiff decoded page {i} differently from the source"
        );
        // control for the byte compare: the same bitmap with one bit flipped must not match
        let mut flipped = want.clone();
        flipped[1000] ^= 1;
        assert!(*bm != flipped, "{tag}: byte-compare control");
        let res = info.split("Resolution:").nth(i + 1).unwrap_or("");
        let want_res = format!("204, {}", spec.y_dpi);
        assert!(
            res.trim_start().starts_with(&want_res),
            "{tag}: page {i} resolution {res:?}"
        );
    }

    // ---- (b) poppler
    let pi = run("pdfinfo", &[s(&pdf)]);
    let pi = String::from_utf8_lossy(&pi.stdout).into_owned();
    assert!(
        pi.lines().any(|l| l.starts_with("Pages:")
            && l.split_whitespace().last() == Some(&pages.len().to_string())),
        "{tag}: pdfinfo\n{pi}"
    );
    for (i, spec) in pages.iter().enumerate() {
        let n = (i + 1).to_string();
        let prefix = work.join(format!("pg{i}"));
        run(
            "pdftoppm",
            &[
                "-mono",
                "-rx",
                "204",
                "-ry",
                &spec.y_dpi.to_string(),
                "-f",
                &n,
                "-l",
                &n,
                s(&pdf),
                s(&prefix),
            ],
        );
        let out = std::fs::read_dir(&work)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|f| {
                f.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&format!("pg{i}-"))
            })
            .expect("pdftoppm output");
        let (w, h, data) = parse_pbm(&std::fs::read(&out).unwrap());
        assert_eq!(
            (w, h),
            (spec.width, spec.height),
            "{tag}: poppler size of page {i}"
        );
        let want = render_bitmap(spec, i as u32);
        assert!(
            data == want,
            "{tag}: poppler raster of page {i} differs from the source"
        );
        let mut flipped = want.clone();
        flipped[1000] ^= 1;
        assert!(data != flipped, "{tag}: poppler compare control");
    }
    Verified {
        pages: pages.len(),
        tiff_bytes: std::fs::metadata(&tif).unwrap().len() as usize,
        pdf_bytes: std::fs::metadata(&pdf).unwrap().len() as usize,
    }
}

/// The fields every successful E case checks. Returns the `fax` object.
pub fn assert_success(
    tag: &str,
    rep: &UacReport,
    res: &Value,
    npages: usize,
    tsi: &str,
    path: &str,
) -> Value {
    let run = rep.fax.as_ref().expect("fax run");
    assert!(!run.timed_out, "{tag}: gateway timed out: {run:?}");
    assert_eq!(
        run.caller_code,
        Some(0),
        "{tag}: SpanDSP says {:?} ({})",
        run.caller_code,
        run.caller_text
    );
    assert_eq!(res["call_id"], rep.call_id.as_str(), "{tag}");
    assert_eq!(res["outcome"]["status"], "completed", "{tag}: {res:#}");
    assert_eq!(res["t38"]["path"], path, "{tag}");
    let fax = &res["fax"];
    assert_eq!(fax["code"], 0, "{tag}: {fax:#}");
    assert_eq!(fax["code_name"], "ok", "{tag}");
    assert_eq!(fax["pages"], npages, "{tag}");
    assert_eq!(fax["remote_ident"], tsi, "{tag}");
    assert_eq!(fax["bit_rate"], 14_400, "{tag}");
    assert_eq!(
        fax["bad_lines_per_page"],
        serde_json::json!(vec![0; npages]),
        "{tag}"
    );
    assert_eq!(fax["partial"], false, "{tag}");
    assert!(fax["store_error"].is_null(), "{tag}");
    fax.clone()
}

pub fn summary(tag: &str, rep: &UacReport, res: &Value, v: &Verified) -> String {
    let run = rep.fax.as_ref().unwrap();
    let f = &res["fax"];
    format!(
        "{tag}: SpanDSP={} ({}) | result.json code={} pages={} tsi={} rate={} dur={}ms | tiff {} B pdf {} B pages-compared={} | wall {} ms | udptl gw tx={} rx={} dropped={}+{} gaps={} | server udptl rx={} tx={} lost={} rec_red={}",
        run.caller_code.unwrap(),
        run.caller_text,
        f["code"],
        f["pages"],
        f["remote_ident"],
        f["bit_rate"],
        f["duration_ms"],
        v.tiff_bytes,
        v.pdf_bytes,
        v.pages,
        run.wall_ms,
        run.tx_datagrams,
        run.rx_datagrams,
        run.tx_dropped,
        run.rx_dropped,
        run.rx_gaps,
        f["udptl"]["rx_datagrams"],
        f["udptl"]["tx_datagrams"],
        f["udptl"]["lost"],
        f["udptl"]["recovered_redundancy"],
    )
}

/// `<data>/faxes/YYYY/MM/DD/<call-id>.tif`
pub fn assert_path_layout(tag: &str, f: &Server, call_id: &str, fax: &Value) {
    let tif = PathBuf::from(fax["tiff"].as_str().unwrap());
    let rel = tif
        .strip_prefix(&f.data_dir)
        .unwrap_or_else(|_| panic!("{tag}: {tif:?} not under {:?}", f.data_dir));
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_str().unwrap().to_string())
        .collect();
    assert_eq!(parts.len(), 5, "{tag}: {parts:?}");
    assert_eq!(parts[0], "faxes");
    assert!(
        parts[1].len() == 4 && parts[1].parse::<u32>().is_ok(),
        "{tag}: year {parts:?}"
    );
    assert!(
        parts[2].len() == 2 && parts[3].len() == 2,
        "{tag}: {parts:?}"
    );
    assert_eq!(
        parts[4],
        format!("{}.tif", fax_in::capture::dir_name(call_id)),
        "{tag}"
    );
}
