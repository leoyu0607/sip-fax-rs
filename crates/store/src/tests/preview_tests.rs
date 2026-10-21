//! P01-P04 at the store level: the cached PNG preview service on a real database and real
//! files (the pixel-exact comparison with libtiff / libpng is `crates/t4/tests/png_oracle.rs`).

use super::*;
use crate::preview::{PngKind, Preview, PreviewError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const DAY: i64 = 86_400_000;

fn data_dir(env: &Env) -> PathBuf {
    let d = env.dir.path().join("data");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn row_bytes(w: u32) -> usize {
    (w as usize).div_ceil(8)
}

/// Deterministic page: text-like stripes and a diagonal, so rows differ from each other.
fn page_bitmap(w: u32, h: u32, salt: u32) -> Vec<u8> {
    let mut bm = vec![0u8; row_bytes(w) * h as usize];
    for y in 0..h {
        for x in 0..w {
            let on = (x + y * 3 + salt) % 17 < 3 || (y % 40 < 4) || x == (y * 2 + salt) % w;
            if on {
                bm[y as usize * row_bytes(w) + (x >> 3) as usize] |= 0x80 >> (x & 7);
            }
        }
    }
    bm
}

struct PageSpec {
    width: u32,
    height: u32,
    y_dpi: u32,
    bitmap: Vec<u8>,
}

fn tiff_bytes(pages: &[PageSpec]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = pages
        .iter()
        .map(|p| t4::encode_mmr(&p.bitmap, p.width).unwrap())
        .collect();
    let ps: Vec<t4::Page> = pages
        .iter()
        .zip(&encoded)
        .map(|(p, g4)| t4::Page {
            width: p.width,
            height: p.height,
            x_dpi: 204,
            y_dpi: p.y_dpi,
            g4,
            bad_lines: 0,
            longest_bad_run: 0,
        })
        .collect();
    t4::write_tiff(
        &ps,
        &t4::TiffMeta {
            software: "preview test",
            datetime: "2026:10:21 08:30:00",
            image_description: None,
        },
    )
    .unwrap()
}

/// Insert a finished fax whose TIFF holds `pages`. Returns (id, tiff path).
async fn mk_fax(env: &Env, call: &str, ended_ms: i64, pages: &[PageSpec]) -> (i64, PathBuf) {
    mk_fax_rec(env, call, ended_ms, pages, pages.len() as i32).await
}

/// Like [`mk_fax`] but the record claims `rec_pages` pages.
async fn mk_fax_rec(
    env: &Env,
    call: &str,
    ended_ms: i64,
    pages: &[PageSpec],
    rec_pages: i32,
) -> (i64, PathBuf) {
    let rel = format!("faxes/{call}.tif");
    let tiff = data_dir(env).join(&rel);
    std::fs::create_dir_all(tiff.parent().unwrap()).unwrap();
    std::fs::write(&tiff, tiff_bytes(pages)).unwrap();
    let mut f = ff(ended_ms, FaxStatus::Received, rec_pages);
    f.tiff_path = Some(rel);
    f.pdf_path = None;
    f.pdf_size = None;
    let (id, _) = env
        .store
        .upsert_finished(&nf(call, "t", ended_ms - 10_000), &f)
        .await
        .unwrap();
    (id, tiff)
}

fn two_pages() -> Vec<PageSpec> {
    vec![
        PageSpec {
            width: 1728,
            height: 200,
            y_dpi: 98,
            bitmap: page_bitmap(1728, 200, 1),
        },
        PageSpec {
            width: 1728,
            height: 300,
            y_dpi: 196,
            bitmap: page_bitmap(1728, 300, 2),
        },
    ]
}

/// (width, height, bit depth, packed rows) of a PNG file, decoded with the `png` crate.
fn read_png(path: &Path) -> (u32, u32, ::png::BitDepth, Vec<u8>) {
    let f = std::fs::File::open(path).unwrap();
    let mut dec = ::png::Decoder::new(io::BufReader::new(f));
    dec.set_transformations(::png::Transformations::IDENTITY);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, ::png::ColorType::Grayscale);
    buf.truncate(info.buffer_size());
    (info.width, info.height, info.bit_depth, buf)
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

// ---------------------------------------------------------------- P01
db_case!(p01_page_png_layout_and_content, p01_page_png_body);
async fn p01_page_png_body(env: &Env) {
    let pages = two_pages();
    let (id, _) = mk_fax(env, "p01", T0, &pages).await;
    let dd = data_dir(env);
    let pv = Preview::new(env.dyn_store(), &dd);

    // page 1 is standard resolution (98 dpi): rows doubled
    let full1 = pv.page_png(id, 1, PngKind::Full).await.unwrap();
    assert_eq!(
        full1,
        dd.join("cache/png").join(id.to_string()).join("1-full.png")
    );
    let (w, h, depth, px) = read_png(&full1);
    assert_eq!((w, h, depth), (1728, 400, ::png::BitDepth::One));
    let line = row_bytes(1728);
    for y in 0..200usize {
        let src = &pages[0].bitmap[y * line..(y + 1) * line];
        for dup in 0..2 {
            let got = &px[(2 * y + dup) * line..(2 * y + dup + 1) * line];
            for x in 0..1728usize {
                let black = got[x >> 3] & (0x80 >> (x & 7)) == 0;
                let want = src[x >> 3] & (0x80 >> (x & 7)) != 0;
                assert_eq!(black, want, "page 1 ({x},{y}) copy {dup}");
            }
        }
    }

    // page 2 is fine resolution (196 dpi): as is
    let full2 = pv.page_png(id, 2, PngKind::Full).await.unwrap();
    let (w, h, depth, px) = read_png(&full2);
    assert_eq!((w, h, depth), (1728, 300, ::png::BitDepth::One));
    for y in 0..300usize {
        for x in 0..1728usize {
            let black = px[y * line + (x >> 3)] & (0x80 >> (x & 7)) == 0;
            let want = pages[1].bitmap[y * line + (x >> 3)] & (0x80 >> (x & 7)) != 0;
            assert_eq!(black, want, "page 2 ({x},{y})");
        }
    }

    // thumbnail: 300 wide, 8-bit, height from the corrected aspect ratio
    let thumb1 = pv.page_png(id, 1, PngKind::Thumb).await.unwrap();
    assert_eq!(thumb1.file_name().unwrap(), "1-thumb.png");
    let (w, h, depth, px) = read_png(&thumb1);
    assert_eq!((w, depth), (300, ::png::BitDepth::Eight));
    assert_eq!(h, (400u32 * 300 + 864) / 1728); // 400 corrected rows, rounded
    assert_eq!(px.len(), (w * h) as usize);
    assert!(
        px.iter().any(|v| *v < 128) && px.iter().any(|v| *v > 200),
        "thumbnail has both dark and light areas"
    );
    assert_eq!(pv.conversions(), 3);
}

// ---------------------------------------------------------------- P03
db_case!(p03_second_request_reads_the_cache, p03_cache_body);
async fn p03_cache_body(env: &Env) {
    let (id, tiff) = mk_fax(env, "p03a", T0, &two_pages()).await;
    let pv = Preview::new(env.dyn_store(), data_dir(env));
    let first = pv.page_png(id, 1, PngKind::Full).await.unwrap();
    assert_eq!(pv.conversions(), 1);
    let bytes = std::fs::read(&first).unwrap();
    let mtime = std::fs::metadata(&first).unwrap().modified().unwrap();

    let second = pv.page_png(id, 1, PngKind::Full).await.unwrap();
    assert_eq!(second, first);
    assert_eq!(
        pv.conversions(),
        1,
        "the second request must not convert again"
    );
    assert_eq!(std::fs::read(&second).unwrap(), bytes);
    // a hit only refreshes the modification time (the "last used" mark of the cache limit)
    assert!(std::fs::metadata(&second).unwrap().modified().unwrap() >= mtime);

    // The TIFF is not needed any more for a cached page ...
    std::fs::remove_file(&tiff).unwrap();
    assert_eq!(pv.page_png(id, 1, PngKind::Full).await.unwrap(), first);
    assert_eq!(pv.conversions(), 1);
    // ... but another page / kind is a new conversion, and now fails for lack of the TIFF.
    assert!(matches!(
        pv.page_png(id, 1, PngKind::Thumb).await,
        Err(PreviewError::TiffMissing(_))
    ));
    // A new service instance (restart) still finds the cache on disk.
    let pv2 = Preview::new(env.dyn_store(), data_dir(env));
    assert_eq!(pv2.page_png(id, 1, PngKind::Full).await.unwrap(), first);
    assert_eq!(pv2.conversions(), 0);
}

db_case!(p03_concurrent_requests_convert_once, p03_concurrent_body);
async fn p03_concurrent_body(env: &Env) {
    // A large page so that the conversion takes long enough for the requests to overlap.
    let big = vec![PageSpec {
        width: 2432,
        height: 3000,
        y_dpi: 196,
        bitmap: page_bitmap(2432, 3000, 5),
    }];
    let (id, _) = mk_fax(env, "p03b", T0, &big).await;
    let pv = Arc::new(Preview::new(env.dyn_store(), data_dir(env)));
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = Vec::new();
    for i in 0..16 {
        let (pv, barrier) = (pv.clone(), barrier.clone());
        let kind = if i % 2 == 0 {
            PngKind::Full
        } else {
            PngKind::Thumb
        };
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            (kind, pv.page_png(id, 1, kind).await)
        }));
    }
    let mut paths = std::collections::HashMap::new();
    for t in tasks {
        let (kind, r) = t.await.unwrap();
        let p = r.unwrap();
        assert!(p.exists());
        assert_eq!(paths.entry(kind).or_insert_with(|| p.clone()), &p);
    }
    assert_eq!(
        pv.conversions(),
        2,
        "8 concurrent requests per image kind must produce one conversion per kind"
    );
    assert!(pv.in_flight() == 0, "single-flight table is cleaned up");
    let dir = data_dir(env).join("cache/png").join(id.to_string());
    assert_eq!(names(&dir), ["1-full.png", "1-thumb.png"]);
    // the eight-fold case of the plan, on one kind only, on a fresh page
    let pv = Arc::new(Preview::new(env.dyn_store(), data_dir(env)));
    std::fs::remove_dir_all(&dir).unwrap();
    let rs = futures_join_all((0..8).map(|_| {
        let pv = pv.clone();
        tokio::spawn(async move { pv.page_png(id, 1, PngKind::Full).await })
    }))
    .await;
    assert!(rs.iter().all(|r| r.is_ok()));
    assert_eq!(pv.conversions(), 1);
}

async fn futures_join_all<T>(it: impl Iterator<Item = tokio::task::JoinHandle<T>>) -> Vec<T> {
    let mut out = Vec::new();
    for h in it.collect::<Vec<_>>() {
        out.push(h.await.unwrap());
    }
    out
}

db_case!(
    p03_interrupted_conversion_leaves_no_png,
    p03_interrupted_body
);
async fn p03_interrupted_body(env: &Env) {
    let (id, _) = mk_fax(env, "p03c", T0, &two_pages()).await;
    let dd = data_dir(env);
    let dir = dd.join("cache/png").join(id.to_string());
    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let pv = Preview::new(env.dyn_store(), &dd).with_fault_hook(move |part: &Path| {
        let n = c2.fetch_add(1, Ordering::SeqCst);
        // the finished .part file is on disk when the "crash" happens
        assert!(part.exists(), "hook runs after the .part file was written");
        match n {
            0 => Err(io::Error::other("injected I/O failure")),
            1 => panic!("injected crash"),
            _ => Ok(()),
        }
    });

    // 1: the write fails: error, no .png, no .part
    let r = pv.page_png(id, 1, PngKind::Full).await;
    assert!(matches!(r, Err(PreviewError::Io(_))), "{r:?}");
    assert!(
        !names(&dir)
            .iter()
            .any(|n| n.ends_with(".png") || n.ends_with(".part")),
        "leftovers after a failed conversion: {:?}",
        names(&dir)
    );

    // 2: the conversion panics: a clean error, still no .png, no .part
    let r = pv.page_png(id, 1, PngKind::Full).await;
    assert!(matches!(r, Err(PreviewError::Task(_))), "{r:?}");
    assert!(
        !names(&dir)
            .iter()
            .any(|n| n.ends_with(".png") || n.ends_with(".part")),
        "leftovers after a panicking conversion: {:?}",
        names(&dir)
    );

    // 3: a process killed half way leaves a garbage .part behind: it is never served and gets
    // replaced by the next conversion
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1-full.png.part"), b"\x89PNG half writ").unwrap();
    assert!(!dir.join("1-full.png").exists());
    let ok = pv.page_png(id, 1, PngKind::Full).await.unwrap();
    assert_eq!(names(&dir), ["1-full.png"]);
    let (w, h, _, _) = read_png(&ok); // a complete, decodable PNG
    assert_eq!((w, h), (1728, 400));
    assert_eq!(pv.conversions(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

// ---------------------------------------------------------------- P04
db_case!(p04_page_out_of_range, p04_range_body);
async fn p04_range_body(env: &Env) {
    let (id, _) = mk_fax(env, "p04a", T0, &two_pages()).await;
    let dd = data_dir(env);
    let pv = Preview::new(env.dyn_store(), &dd);
    for page in [0u32, 3, 4, 1000, u32::MAX] {
        for kind in [PngKind::Full, PngKind::Thumb] {
            let r = pv.page_png(id, page, kind).await;
            match r {
                Err(PreviewError::PageOutOfRange { page: p, pages }) => {
                    assert_eq!(p, page);
                    assert_eq!(pages, 2, "page {page}");
                }
                other => panic!("page {page}: expected PageOutOfRange, got {other:?}"),
            }
        }
    }
    let msg = pv
        .page_png(id, 3, PngKind::Full)
        .await
        .unwrap_err()
        .to_string();
    assert!(msg.contains("page 3") && msg.contains("2 pages"), "{msg}");
    assert!(
        names(&dd.join("cache/png").join(id.to_string())).is_empty(),
        "no cache file for an invalid page"
    );
    assert_eq!(
        pv.conversions(),
        0,
        "bogus page numbers never reach the TIFF"
    );

    // The record's page count can be wrong (it says 5, the file has 2): the TIFF decides.
    let (id5, _) = mk_fax_rec(env, "p04a2", T0, &two_pages(), 5).await;
    for page in [3u32, 5] {
        match pv.page_png(id5, page, PngKind::Full).await {
            Err(PreviewError::PageOutOfRange { page: p, pages }) => {
                assert_eq!((p, pages), (page, 2))
            }
            other => panic!("page {page}: expected PageOutOfRange {{ pages: 2 }}, got {other:?}"),
        }
    }
    assert_eq!(pv.conversions(), 2);
    assert!(names(&dd.join("cache/png").join(id5.to_string())).is_empty());
    // the valid pages still work
    pv.page_png(id, 2, PngKind::Thumb).await.unwrap();
}

db_case!(p04_purged_files_and_cache_cleanup, p04_purged_body);
async fn p04_purged_body(env: &Env) {
    let now = T0 + 100 * DAY;
    let (id, tiff) = mk_fax(env, "p04b", now - 10 * DAY, &two_pages()).await;
    let dd = data_dir(env);
    let pv = Preview::new(env.dyn_store(), &dd);
    let png = pv.page_png(id, 1, PngKind::Full).await.unwrap();
    pv.page_png(id, 2, PngKind::Thumb).await.unwrap();
    let dir = png.parent().unwrap().to_path_buf();
    assert_eq!(names(&dir).len(), 2);

    env.clock.set(t(now));
    let policy = RetentionPolicy {
        files_days: 7,
        records_days: 0,
    };
    let rep = Purger::new(env.dyn_store(), &dd, env.clock.clone())
        .run(policy, false)
        .await
        .unwrap();
    assert_eq!(rep.files_purged, 1, "{rep:?}");
    assert!(!tiff.exists());
    assert!(!dir.exists(), "the purge removed the PNG cache directory");

    let rec = env.store.get(id).await.unwrap().unwrap();
    let at = rec.files_purged_at.expect("marked purged");
    for kind in [PngKind::Full, PngKind::Thumb] {
        match pv.page_png(id, 1, kind).await {
            Err(PreviewError::Purged { id: i, at: a }) => assert_eq!((i, a), (id, at)),
            other => panic!("expected Purged, got {other:?}"),
        }
    }
    assert!(!dir.exists(), "nothing is recreated for a purged fax");
    assert!(
        pv.page_png(id, 1, PngKind::Full)
            .await
            .unwrap_err()
            .to_string()
            .contains("purged")
    );
}

db_case!(p04_damaged_missing_and_unusable_tiff, p04_tiff_body);
async fn p04_tiff_body(env: &Env) {
    let dd = data_dir(env);
    let pv = Preview::new(env.dyn_store(), &dd);

    // unknown id
    assert!(matches!(
        pv.page_png(987_654, 1, PngKind::Full).await,
        Err(PreviewError::NotFound(987_654))
    ));

    // garbage, empty, truncated, bit-flipped files
    let good = tiff_bytes(&two_pages());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("garbage", b"this is not a tiff file at all".to_vec()),
        ("empty", Vec::new()),
        ("header-only", good[..8].to_vec()),
        ("truncated-mid-image", good[..good.len() / 3].to_vec()),
        ("truncated-before-ifd", good[..good.len() - 40].to_vec()),
        ("zeros", vec![0u8; 4096]),
    ];
    for (name, bytes) in cases {
        let (id, tiff) = mk_fax(env, &format!("p04-{name}"), T0, &two_pages()).await;
        std::fs::write(&tiff, &bytes).unwrap();
        for kind in [PngKind::Full, PngKind::Thumb] {
            match pv.page_png(id, 1, kind).await {
                Err(PreviewError::BadTiff(why)) => assert!(!why.is_empty(), "{name}"),
                other => panic!("{name}: expected BadTiff, got {other:?}"),
            }
        }
        let dir = dd.join("cache/png").join(id.to_string());
        assert!(
            !names(&dir).iter().any(|n| n.ends_with(".png")),
            "{name}: no PNG for a damaged TIFF"
        );
    }

    // only page 2 damaged: page 1 still works, page 2 is a clear error
    let (id, tiff) = mk_fax(env, "p04-page2", T0, &two_pages()).await;
    let mut b = std::fs::read(&tiff).unwrap();
    let ifd2 = {
        // second IFD: follow the chain from the first
        let first = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(b[first..first + 2].try_into().unwrap()) as usize;
        u32::from_le_bytes(
            b[first + 2 + n * 12..first + 6 + n * 12]
                .try_into()
                .unwrap(),
        ) as usize
    };
    // StripByteCounts of page 2 -> far beyond the end of the file
    let n2 = u16::from_le_bytes(b[ifd2..ifd2 + 2].try_into().unwrap()) as usize;
    for i in 0..n2 {
        let e = ifd2 + 2 + i * 12;
        if u16::from_le_bytes(b[e..e + 2].try_into().unwrap()) == 279 {
            b[e + 8..e + 12].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
        }
    }
    std::fs::write(&tiff, &b).unwrap();
    // the whole file fails to parse (the reader validates every IFD up front)
    assert!(matches!(
        pv.page_png(id, 1, PngKind::Full).await,
        Err(PreviewError::BadTiff(_))
    ));

    // TIFF deleted from disk
    let (id, tiff) = mk_fax(env, "p04-missing", T0, &two_pages()).await;
    std::fs::remove_file(&tiff).unwrap();
    match pv.page_png(id, 1, PngKind::Full).await {
        Err(PreviewError::TiffMissing(p)) => assert_eq!(p, tiff),
        other => panic!("expected TiffMissing, got {other:?}"),
    }

    // record without a TIFF (e.g. voice call)
    let (id, _) = env
        .store
        .upsert_finished(
            &nf("p04-nofile", "t", T0),
            &FaxFinal {
                tiff_path: None,
                tiff_size: None,
                pdf_path: None,
                pdf_size: None,
                ..ff(T0 + 1000, FaxStatus::NoFax, 0)
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        pv.page_png(id, 1, PngKind::Full).await,
        Err(PreviewError::NoFile(i)) if i == id
    ));

    // a stored path that escapes data_dir is refused, not read
    let outside = env.dir.path().join("secret.tif");
    std::fs::write(&outside, tiff_bytes(&two_pages())).unwrap();
    let (id, _) = env
        .store
        .upsert_finished(
            &nf("p04-escape", "t", T0),
            &FaxFinal {
                tiff_path: Some("../secret.tif".into()),
                ..ff(T0 + 1000, FaxStatus::Received, 2)
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        pv.page_png(id, 1, PngKind::Full).await,
        Err(PreviewError::UnsafePath(_))
    ));
}

// ---------------------------------------------------------------- R01 (M4a)
db_case!(r01_purge_during_conversion_leaves_no_png, r01_body);
async fn r01_body(env: &Env) {
    let now = T0 + 100 * DAY;
    let (id, tiff) = mk_fax(env, "r01", now - 10 * DAY, &two_pages()).await;
    let dd = data_dir(env);
    let (reached_tx, reached_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go_rx = std::sync::Mutex::new(go_rx);
    let reached_tx = std::sync::Mutex::new(reached_tx);
    let pv = Arc::new(
        Preview::new(env.dyn_store(), &dd).with_fault_hook(move |_p: &Path| {
            reached_tx.lock().unwrap().send(()).unwrap();
            go_rx.lock().unwrap().recv().unwrap();
            Ok(())
        }),
    );
    let pv2 = pv.clone();
    let req = tokio::spawn(async move { pv2.page_png(id, 1, PngKind::Full).await });
    tokio::task::spawn_blocking(move || reached_rx.recv().unwrap())
        .await
        .unwrap();
    // the conversion has written its .part file; now the purge runs completely
    env.clock.set(t(now));
    let rep = Purger::new(env.dyn_store(), &dd, env.clock.clone())
        .run(
            RetentionPolicy {
                files_days: 7,
                records_days: 0,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(rep.files_purged, 1);
    assert!(!tiff.exists());
    go_tx.send(()).unwrap();
    let r = req.await.unwrap();
    assert!(
        matches!(r, Err(PreviewError::Purged { .. })),
        "410 for the request: {r:?}"
    );
    let dir = dd.join("cache/png").join(id.to_string());
    assert!(
        !dir.exists() || names(&dir).is_empty(),
        "no orphan PNG: {:?}",
        if dir.exists() { names(&dir) } else { vec![] }
    );
    // reverse: without a purge a request succeeds and the PNG stays
    let (id2, _) = mk_fax(env, "r01b", now, &two_pages()).await;
    let pv3 = Preview::new(env.dyn_store(), &dd);
    let p = pv3.page_png(id2, 1, PngKind::Full).await.unwrap();
    assert!(p.exists());
}

// ---------------------------------------------------------------- R02 (M4a)
db_case!(r02_cache_limit_evicts_least_recently_used, r02_body);
async fn r02_body(env: &Env) {
    let (id, _) = mk_fax(env, "r02", T0, &two_pages()).await;
    let dd = data_dir(env);
    let set_age = |p: &Path, secs_ago: i64| {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        let when: std::time::SystemTime = (t(T0) - chrono::Duration::seconds(secs_ago)).into();
        f.set_modified(when).unwrap();
    };
    env.clock.set(t(T0));
    let unlimited = Preview::new(env.dyn_store(), &dd).with_clock(env.clock.clone());
    let mut paths = Vec::new();
    for (page, kind) in [
        (1, PngKind::Full),
        (1, PngKind::Thumb),
        (2, PngKind::Full),
        (2, PngKind::Thumb),
    ] {
        paths.push(unlimited.page_png(id, page, kind).await.unwrap());
    }
    let sizes: Vec<u64> = paths
        .iter()
        .map(|p| std::fs::metadata(p).unwrap().len())
        .collect();
    // ages: [0] 1000 s (oldest), [1] 900, [2] 800, [3] 10 s (inside the protection window)
    for (p, age) in paths.iter().zip([1000, 900, 800, 10]) {
        set_age(p, age);
    }
    let total: u64 = sizes.iter().sum();
    let limit = total - sizes[1] - sizes[2];
    let pv = Preview::new(env.dyn_store(), &dd)
        .with_clock(env.clock.clone())
        .with_cache_limit(limit, std::time::Duration::from_secs(60));
    // a hit on the oldest page makes it recently used
    pv.page_png(id, 1, PngKind::Full).await.unwrap();
    assert_eq!(pv.conversions(), 0, "served from the cache");
    let rep = pv.trim_cache().await.unwrap();
    assert!(rep.kept_bytes <= limit, "{rep:?}");
    assert!(paths[0].exists(), "the page that was just used stays");
    assert!(
        !paths[1].exists() && !paths[2].exists(),
        "the least recently used go first"
    );
    assert!(
        paths[3].exists(),
        "a page used within the window is never deleted"
    );
    // soft limit: what is left is protected, nothing more is deleted even above the limit
    let tiny = Preview::new(env.dyn_store(), &dd)
        .with_clock(env.clock.clone())
        .with_cache_limit(1, std::time::Duration::from_secs(60));
    let rep = tiny.trim_cache().await.unwrap();
    assert_eq!(rep.removed_files, 0, "{rep:?}");
    assert!(paths[0].exists() && paths[3].exists());
    // an hour later nothing is protected any more
    env.clock.advance(chrono::Duration::hours(1));
    let rep = tiny.trim_cache().await.unwrap();
    assert_eq!(rep.removed_files, 2);
    assert!(
        !dd.join("cache/png").join(id.to_string()).exists(),
        "the empty directory is removed"
    );
    // a trimmed page is converted again on demand
    let again = pv.page_png(id, 2, PngKind::Full).await.unwrap();
    assert!(again.exists());
    assert_eq!(pv.conversions(), 1);
}
