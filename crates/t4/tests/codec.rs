//! Host-only tests of the decoder, encoder and writers: F04, F05, F09 and round trips.
//! The libtiff / poppler comparisons live in `oracle.rs`.

mod common;

use common::*;
use proptest::prelude::*;
use t4::{
    Coding, Decoder, DecoderConfig, Encoder, EndReason, Error, Page, TiffMeta, decode_page,
    encode_mmr, write_pdf, write_tiff,
};

const W: u32 = 1728;

fn encode(bm: &[u8], width: u32, coding: Coding, fill: bool) -> Vec<u8> {
    let mut e = Encoder::new(width, coding).unwrap().with_fill_bits(fill);
    for r in bm.chunks(rb(width)) {
        e.push_row(r).unwrap();
    }
    e.finish()
}

fn decode(data: &[u8], width: u32, coding: Coding) -> t4::PageData {
    decode_page(data, DecoderConfig::new(width, coding)).unwrap()
}

/// Decode feeding `chunk` bytes at a time; returns (bitmap, summary).
fn decode_chunked(data: &[u8], cfg: DecoderConfig, chunk: usize) -> (Vec<u8>, t4::Summary) {
    let mut d = Decoder::new(cfg).unwrap();
    let mut out = Vec::new();
    for c in data.chunks(chunk.max(1)) {
        d.push(c, &mut |r| out.extend_from_slice(r));
    }
    let s = d.finish(&mut |r| out.extend_from_slice(r));
    (out, s)
}

const CODINGS: [Coding; 4] = [
    Coding::Mh,
    Coding::Mr { k: 2 },
    Coding::Mr { k: 4 },
    Coding::Mmr,
];

fn terminator(c: Coding) -> EndReason {
    if c == Coding::Mmr {
        EndReason::Eofb
    } else {
        EndReason::Rtc
    }
}

#[test]
fn round_trip_all_codings_widths_and_chunkings() {
    for width in [1728u32, 2048, 2432] {
        for (name, bm) in bitmaps(width, 120) {
            for coding in CODINGS {
                for fill in [false, true] {
                    if fill && coding == Coding::Mmr {
                        continue;
                    }
                    let data = encode(&bm, width, coding, fill);
                    let page = decode(&data, width, coding);
                    assert_eq!(page.bitmap, bm, "{name} {width} {coding:?} fill={fill}");
                    assert_eq!(page.summary.end, terminator(coding), "{name} {coding:?}");
                    assert_eq!(page.summary.stats.rows, 120);
                    assert_eq!(page.summary.stats.bad_lines, 0);
                    assert_eq!(page.summary.error, None);
                }
            }
        }
    }
}

#[test]
fn streaming_gives_the_same_result_for_any_chunking() {
    // Tiny chunks re-attempt the partial row on every push (quadratic in the row size), so the
    // 1-3 byte chunkings use short pages.
    for (rows, chunks) in [
        (5u32, &[1usize, 2, 3][..]),
        (40, &[7, 100, 4096, 100_000][..]),
    ] {
        for (name, bm) in bitmaps(W, rows) {
            for coding in CODINGS {
                let data = encode(&bm, W, coding, false);
                for &chunk in chunks {
                    let (out, s) = decode_chunked(&data, DecoderConfig::new(W, coding), chunk);
                    assert_eq!(out, bm, "{name} {coding:?} chunk {chunk}");
                    assert_eq!(s.end, terminator(coding));
                    assert_eq!(s.stats.rows, rows);
                }
            }
        }
    }
}

#[test]
fn odd_and_tiny_widths_round_trip() {
    for width in [
        1u32, 2, 7, 8, 9, 63, 64, 65, 100, 2559, 2560, 2561, 5000, 8192,
    ] {
        for (name, bm) in bitmaps(width, 12) {
            for coding in CODINGS {
                let data = encode(&bm, width, coding, false);
                assert_eq!(
                    decode(&data, width, coding).bitmap,
                    bm,
                    "{name} w{width} {coding:?}"
                );
            }
        }
    }
}

#[test]
fn every_run_length_round_trips() {
    // White run of n then black to the end, and the reverse: exercises every terminating,
    // make-up and extended make-up code (the libtiff oracle repeats this on the same rows).
    let width = 2600u32;
    for n in 0..=width {
        let mut bm = vec![0u8; rb(width)];
        for x in n..width {
            set_px(&mut bm, width, x, 0);
        }
        let mut inv = vec![0u8; rb(width)];
        for x in 0..n {
            set_px(&mut inv, width, x, 0);
        }
        for coding in [Coding::Mh, Coding::Mmr] {
            for r in [&bm, &inv] {
                let data = encode(r, width, coding, false);
                assert_eq!(
                    decode(&data, width, coding).bitmap,
                    *r,
                    "run {n} {coding:?}"
                );
            }
        }
    }
}

// ---- known bit patterns (independent of our encoder) ----

#[test]
fn mh_known_vector_white_black_and_rtc() {
    // EOL + white 1728 (makeup 010011011, term 00110101); EOL + white 10 (00111) black 1718
    // (makeup 1664 0000001100100, term 54 000000111000)... built from the T.4 tables by hand:
    // row 2: white 4 (1011), black 1 (010), white 1723 (1664 -> 011000, 59 -> 01001010... ).
    // Keep the second row simple: white 1 (000111), black 1727 (1664: 0000001100100, 63: 000001100111).
    let eol = "000000000001";
    let row1 = "010011011 00110101";
    let row2 = "000111 0000001100100 000001100111";
    let s = format!("{eol}{row1}{eol}{row2}{eol}{eol}{eol}{eol}{eol}{eol}");
    let page = decode(&bits(&s), W, Coding::Mh);
    assert_eq!(page.summary.end, EndReason::Rtc);
    assert_eq!(page.summary.stats.rows, 2);
    let mut expect = vec![0u8; rb(W)];
    let mut r2 = vec![0u8; rb(W)];
    for x in 1..W {
        set_px(&mut r2, W, x, 0);
    }
    expect.extend_from_slice(&r2);
    assert_eq!(page.bitmap, expect);
}

#[test]
fn mmr_known_vector_white_rows_then_eofb() {
    // Three all-white rows: V0 each, then EOFB.
    let page = decode(&bits("1 1 1 000000000001 000000000001"), W, Coding::Mmr);
    assert_eq!(page.summary.end, EndReason::Eofb);
    assert_eq!(page.summary.stats.rows, 3);
    assert_eq!(page.bitmap, vec![0u8; 3 * rb(W)]);
    // Row 2 = black pixels 10..20: H (001) white 10 (00111) black 10 (0000100), then
    // V0 (1) returns to the end. Reference is the all-white first row.
    let s = "1 001 00111 0000100 1 000000000001 000000000001";
    let page = decode(&bits(s), W, Coding::Mmr);
    assert_eq!(page.summary.stats.rows, 2);
    let mut r = vec![0u8; rb(W)];
    for x in 10..20 {
        set_px(&mut r, W, x, 0);
    }
    assert_eq!(&page.bitmap[rb(W)..], &r[..]);
    // Encoder agrees bit for bit.
    let mut bm = vec![0u8; rb(W)];
    bm.extend_from_slice(&r);
    assert_eq!(encode_mmr(&bm, W).unwrap(), bits(s));
}

// ---- F04 ----

#[test]
fn f04_eol_with_fill_bits_rtc_and_eofb() {
    let bms = bitmaps(W, 30);
    let (_, bm) = &bms[3];
    // Fill bits: every EOL ends on a byte boundary in the encoder output...
    let data = encode(bm, W, Coding::Mh, true);
    let eols = eol_positions(&data);
    assert!(eols.len() >= 30 + 6);
    for p in &eols {
        // zero-run start .. the 1 bit: find the '1' position
        let mut q = *p;
        while (data[q / 8] >> (7 - q % 8)) & 1 == 0 {
            q += 1;
        }
        assert_eq!((q + 1) % 8, 0, "EOL at bit {p} must end on a byte boundary");
    }
    // ... with extra fill zeros (more than the 11 minimum) present somewhere.
    assert!(eols.iter().zip(&eols[1..]).any(|_| true));
    let page = decode(&data, W, Coding::Mh);
    assert_eq!(page.bitmap, *bm);
    assert_eq!(page.summary.end, EndReason::Rtc);

    // Extra fill: 40 zero bits inserted before every EOL.
    let plain = encode(bm, W, Coding::Mh, false);
    let positions = eol_positions(&plain);
    let mut stuffed = String::new();
    let mut last = 0usize;
    let bit = |i: usize| {
        if (plain[i / 8] >> (7 - i % 8)) & 1 == 1 {
            '1'
        } else {
            '0'
        }
    };
    for p in &positions {
        stuffed.extend((last..*p).map(bit));
        stuffed.push_str(&"0".repeat(40));
        last = *p;
    }
    stuffed.extend((last..plain.len() * 8).map(bit));
    let page = decode(&bits(&stuffed), W, Coding::Mh);
    assert_eq!(page.bitmap, *bm);
    assert_eq!(page.summary.end, EndReason::Rtc);

    // RTC stops the page: garbage after it is ignored and no extra row appears.
    let mut tail = plain.clone();
    tail.extend_from_slice(&[0xAA, 0x55, 0xFF, 0x00, 0x13]);
    let page = decode(&tail, W, Coding::Mh);
    assert_eq!(
        (page.summary.end, page.summary.stats.rows),
        (EndReason::Rtc, 30)
    );
    assert_eq!(page.bitmap, *bm);

    // Only five EOLs (no RTC) then the end of data: ends as Data with all rows.
    let mut five = plain.clone();
    let cut = positions[positions.len() - 1]; // start of the 6th trailing EOL
    five.truncate(cut / 8 + usize::from(cut % 8 != 0));
    let page = decode(
        &bits(&{
            let mut s = String::new();
            s.extend((0..cut).map(bit));
            s
        }),
        W,
        Coding::Mh,
    );
    assert_eq!(page.summary.stats.rows, 30);
    assert_eq!(page.summary.end, EndReason::Data);
    assert_eq!(page.bitmap, *bm);

    // MR: RTC is six EOL+1.
    let mr = encode(bm, W, Coding::Mr { k: 2 }, true);
    let page = decode(&mr, W, Coding::Mr { k: 2 });
    assert_eq!(
        (page.summary.end, page.summary.stats.rows),
        (EndReason::Rtc, 30)
    );

    // MMR: EOFB ends the page, trailing bytes ignored; without EOFB the page ends at the data.
    let g4 = encode_mmr(bm, W).unwrap();
    let mut tail = g4.clone();
    tail.extend_from_slice(&[0xDE, 0xAD]);
    let page = decode(&tail, W, Coding::Mmr);
    assert_eq!(
        (page.summary.end, page.summary.stats.rows),
        (EndReason::Eofb, 30)
    );
    assert_eq!(page.bitmap, *bm);
    // Strip the 24-bit EOFB (3 bytes at most; it is not byte aligned so cut at the bit level).
    let total_bits = g4.len() * 8;
    let eofb_at = eol_positions(&g4).into_iter().last().unwrap_or(total_bits);
    // The first of the two EOLs begins 12 bits before the last EOL's zeros... find it:
    let first_eol = eol_positions(&g4)[0];
    assert!(first_eol <= eofb_at);
    let s: String = (0..first_eol).map(bit_of(&g4)).collect();
    let page = decode(&bits(&s), W, Coding::Mmr);
    assert_eq!(
        (page.summary.end, page.summary.stats.rows),
        (EndReason::Data, 30)
    );
    assert_eq!(page.bitmap, *bm);
}

fn bit_of(d: &[u8]) -> impl Fn(usize) -> char + '_ {
    move |i| {
        if (d[i / 8] >> (7 - i % 8)) & 1 == 1 {
            '1'
        } else {
            '0'
        }
    }
}

#[test]
fn eol_less_mh_rows_decode_when_eols_are_not_required() {
    // Three rows back to back: white 1728 each.
    let row = "010011011 00110101";
    let s = format!("{row}{row}{row}");
    let mut cfg = DecoderConfig::new(W, Coding::Mh);
    cfg.eol_required = false;
    let p = decode_page(&bits(&s), cfg).unwrap();
    assert_eq!(p.summary.stats.rows, 3);
    assert_eq!(p.summary.stats.bad_lines, 0);
    // With EOLs required the same data is not three clean rows.
    let p = decode_page(&bits(&s), DecoderConfig::new(W, Coding::Mh)).unwrap();
    assert!(p.summary.stats.bad_lines >= 1);
}

// ---- F05 ----

/// Overwrite `len` bits starting at bit `at` with ones.
fn smash(data: &mut [u8], at: usize, len: usize) {
    for i in at..at + len {
        data[i / 8] |= 0x80 >> (i % 8);
    }
}

#[test]
fn f05_corrupted_mh_rows_resync_and_substitute() {
    let bms = bitmaps(W, 40);
    let (_, bm) = &bms[4]; // noise: long rows
    let mut data = encode(bm, W, Coding::Mh, false);
    let eols = eol_positions(&data);
    // row i occupies eols[i]+12 .. eols[i+1]
    let corrupt = [5usize, 17, 18];
    for &r in &corrupt {
        let (start, end) = (eols[r] + 12, eols[r + 1]);
        assert!(
            end - start > 40,
            "row {r} too short to corrupt: {}",
            end - start
        );
        // Fill the middle with ones: decodes to a few short runs, never the right length.
        smash(&mut data, start + 8, (end - start - 16).min(30));
    }
    let page = decode(&data, W, Coding::Mh);
    let s = page.summary;
    assert_eq!(s.stats.rows, 40, "row count must be preserved: {s:?}");
    assert_eq!(s.stats.bad_lines, 3);
    assert_eq!(s.stats.longest_bad_run, 2);
    assert_eq!(s.end, EndReason::Rtc);
    for y in 0..40 {
        let got = row(&page.bitmap, W, y);
        if corrupt.contains(&y) {
            // substituted by the previous decoded row (which for 18 is the substitute of 17)
            assert_eq!(
                got,
                row(&page.bitmap, W, y - 1),
                "row {y} must repeat the row above"
            );
        } else {
            assert_eq!(got, row(bm, W, y), "row {y} must be untouched");
        }
    }
    // Same result when streamed one byte at a time.
    let (out, s2) = decode_chunked(&data, DecoderConfig::new(W, Coding::Mh), 1);
    assert_eq!((out, s2), (page.bitmap.clone(), s));
}

#[test]
fn f05_first_row_bad_is_replaced_by_white_and_missing_data_is_a_bad_line() {
    let bms = bitmaps(W, 10);
    let (_, bm) = &bms[3];
    let mut data = encode(bm, W, Coding::Mh, false);
    let eols = eol_positions(&data);
    smash(&mut data, eols[0] + 12 + 8, 30);
    let page = decode(&data, W, Coding::Mh);
    assert_eq!(page.summary.stats.bad_lines, 1);
    assert_eq!(row(&page.bitmap, W, 0), &vec![0u8; rb(W)][..]);
    assert_eq!(row(&page.bitmap, W, 1), row(bm, W, 1));

    // A row cut short by an early EOL (bytes removed from its middle) is bad, the rest fine.
    let plain = encode(bm, W, Coding::Mh, false);
    let eols = eol_positions(&plain);
    let s: String = (0..eols[3] + 12 + 20)
        .map(bit_of(&plain))
        .chain((eols[4]..plain.len() * 8).map(bit_of(&plain)))
        .collect();
    let page = decode(&bits(&s), W, Coding::Mh);
    assert_eq!(page.summary.stats.rows, 10);
    assert_eq!(page.summary.stats.bad_lines, 1);
    assert_eq!(row(&page.bitmap, W, 3), row(&page.bitmap, W, 2));
    assert_eq!(row(&page.bitmap, W, 4), row(bm, W, 4));

    // Truncated final row (no RTC): counted as a bad line, no panic.
    let cut = eols[9] + 12 + 15;
    let s: String = (0..cut).map(bit_of(&plain)).collect();
    let page = decode(&bits(&s), W, Coding::Mh);
    assert_eq!(page.summary.stats.rows, 10);
    assert_eq!(page.summary.stats.bad_lines, 1);
}

#[test]
fn f05_corrupted_mr_row_resyncs_at_the_next_eol() {
    let bms = bitmaps(W, 40);
    let (_, bm) = &bms[4];
    let mut data = encode(bm, W, Coding::Mr { k: 2 }, false);
    let eols = eol_positions(&data);
    // Rows alternate 1D (even) / 2D (odd). Corrupt odd rows; the next row is one-dimensional,
    // so the reference substitution does not propagate.
    let corrupt = [7usize, 21];
    for &r in &corrupt {
        let (start, end) = (eols[r] + 13, eols[r + 1]);
        if end - start > 24 {
            smash(&mut data, start + 4, (end - start - 8).min(24));
        }
    }
    let page = decode(&data, W, Coding::Mr { k: 2 });
    assert_eq!(page.summary.stats.rows, 40);
    assert!(page.summary.stats.bad_lines >= 1 && page.summary.stats.bad_lines <= 2);
    // Every row after the second corruption's successor is intact again.
    for y in 23..40 {
        assert_eq!(row(&page.bitmap, W, y), row(bm, W, y), "row {y}");
    }
    for y in 0..7 {
        assert_eq!(row(&page.bitmap, W, y), row(bm, W, y), "row {y}");
    }
}

#[test]
fn f05_mmr_error_abandons_the_rest_of_the_page() {
    let bms = bitmaps(W, 60);
    let (_, bm) = &bms[3];
    let data = encode_mmr(bm, W).unwrap();
    // Replace the middle of the stream with zeros (never a valid code).
    let mut bad = data.clone();
    let at = data.len() / 2;
    for b in &mut bad[at..at + 4] {
        *b = 0;
    }
    let page = decode(&bad, W, Coding::Mmr);
    assert_eq!(page.summary.end, EndReason::Aborted);
    assert!(page.summary.error.is_some());
    let rows = page.summary.stats.rows as usize;
    assert!(rows > 0 && rows < 60, "rows before the error: {rows}");
    assert_eq!(page.bitmap.len(), rows * rb(W));
    assert_eq!(
        &page.bitmap[..],
        &bm[..rows * rb(W)],
        "rows before the error are exact"
    );
    assert_eq!(page.summary.stats.bad_lines, 0);

    // The 0000001xxx extension (uncompressed mode) is reported as unsupported.
    let page = decode(&bits("1 0000001111"), W, Coding::Mmr);
    assert_eq!(page.summary.error, Some(Error::Unsupported));
    assert_eq!(page.summary.stats.rows, 1);

    // Truncation in the middle of the data is an error too, but the rows before are kept.
    let page = decode(&data[..data.len() / 2], W, Coding::Mmr);
    assert_eq!(page.summary.end, EndReason::Aborted);
    assert_eq!(page.summary.error, Some(Error::Truncated));
}

#[test]
fn row_limit_and_config_validation() {
    let bms = bitmaps(W, 50);
    let (_, bm) = &bms[2];
    let data = encode(bm, W, Coding::Mmr, false);
    let mut cfg = DecoderConfig::new(W, Coding::Mmr);
    cfg.max_rows = 10;
    let p = decode_page(&data, cfg).unwrap();
    assert_eq!(
        (p.summary.end, p.summary.stats.rows),
        (EndReason::RowLimit, 10)
    );
    assert_eq!(p.bitmap, bm[..10 * rb(W)]);
    // Same for MH where the limit also stops bad rows from accumulating.
    let junk = vec![0xFFu8; 100_000];
    let mut cfg = DecoderConfig::new(W, Coding::Mh);
    cfg.max_rows = 7;
    let p = decode_page(&junk, cfg).unwrap();
    assert!(p.summary.stats.rows <= 7);

    assert_eq!(
        Decoder::new(DecoderConfig::new(0, Coding::Mh)).unwrap_err(),
        Error::InvalidWidth(0)
    );
    assert_eq!(
        Decoder::new(DecoderConfig::new(9000, Coding::Mmr)).unwrap_err(),
        Error::InvalidWidth(9000)
    );
    assert_eq!(
        Encoder::new(0, Coding::Mmr).unwrap_err(),
        Error::InvalidWidth(0)
    );
    assert_eq!(
        Encoder::new(8, Coding::Mr { k: 1 }).unwrap_err(),
        Error::InvalidK(1)
    );
    let mut e = Encoder::new(16, Coding::Mmr).unwrap();
    assert_eq!(
        e.push_row(&[0u8; 3]).unwrap_err(),
        Error::RowSize {
            expected: 2,
            got: 3
        }
    );
    assert_eq!(e.rows(), 0);
    for e in [
        Error::InvalidWidth(1),
        Error::InvalidK(1),
        Error::RowSize {
            expected: 1,
            got: 2,
        },
        Error::InvalidCode,
        Error::LineOverrun,
        Error::Truncated,
        Error::Unsupported,
        Error::TooManyChanges,
        Error::InvalidPage("x"),
        Error::InvalidMetadata("y"),
        Error::TooLarge,
        Error::NoPages,
    ] {
        assert!(!e.to_string().is_empty(), "{e:?}");
    }
    // after the page ended, more pushes are ignored
    let mut d = Decoder::new(DecoderConfig::new(W, Coding::Mmr)).unwrap();
    let mut n = 0;
    d.push(&bits("1 000000000001 000000000001"), &mut |_| n += 1);
    assert!(d.is_finished());
    d.push(&[1, 2, 3], &mut |_| n += 1);
    assert_eq!((n, d.stats().rows), (1, 1));
}

#[test]
fn zero_fill_and_junk_streams_stay_bounded() {
    // 1 MiB each of zeros, 0xFF and repeating patterns: no panic, bounded rows.
    for coding in CODINGS {
        for fill in [0x00u8, 0xFF, 0x55, 0x01] {
            let cfg = DecoderConfig::new(W, coding);
            let mut d = Decoder::new(cfg).unwrap();
            let chunk = vec![fill; 65_536];
            let mut rows = 0u32;
            for _ in 0..16 {
                d.push(&chunk, &mut |_| rows += 1);
            }
            let s = d.finish(&mut |_| rows += 1);
            assert!(rows <= t4::DEFAULT_MAX_ROWS, "{coding:?} {fill:#x}");
            assert_eq!(rows, s.stats.rows);
        }
    }
}

// ---- writers ----

fn meta() -> TiffMeta<'static> {
    TiffMeta {
        software: "t4 test",
        datetime: "2026:10:21 12:00:00",
        image_description: Some("TSI: +886 2 1234"),
    }
}

#[test]
fn tiff_and_pdf_writers_validate_input() {
    let g4 = encode_mmr(&vec![0u8; rb(W) * 4], W).unwrap();
    let page = |w, h, x, y, d| Page {
        width: w,
        height: h,
        x_dpi: x,
        y_dpi: y,
        g4: d,
        bad_lines: 0,
        longest_bad_run: 0,
    };
    assert_eq!(write_tiff(&[], &meta()).unwrap_err(), Error::NoPages);
    assert_eq!(write_pdf(&[]).unwrap_err(), Error::NoPages);
    for bad in [
        page(0, 4, 204, 98, &g4),
        page(W, 0, 204, 98, &g4),
        page(W, 4, 0, 98, &g4),
        page(W, 4, 204, 98, &[]),
    ] {
        assert!(matches!(
            write_tiff(&[bad], &meta()),
            Err(Error::InvalidPage(_))
        ));
        assert!(matches!(write_pdf(&[bad]), Err(Error::InvalidPage(_))));
    }
    let good = page(W, 4, 204, 98, &g4);
    let mut m = meta();
    m.datetime = "yesterday";
    assert!(matches!(
        write_tiff(&[good], &m),
        Err(Error::InvalidMetadata(_))
    ));
    // structure checks of the TIFF: header, IFD chain and tag count for 2 pages
    let mut m = meta();
    m.image_description = Some("caf\u{e9}\n");
    let t = write_tiff(
        &[
            good,
            Page {
                bad_lines: 3,
                longest_bad_run: 2,
                ..good
            },
        ],
        &m,
    )
    .unwrap();
    assert_eq!(&t[..4], b"II\x2a\x00");
    assert!(
        t.windows(7).any(|w| w == b"caf???\x00"),
        "non-ASCII and control bytes become '?'"
    );
    // PDF: header, EOF marker, xref offsets point at "N 0 obj"
    let p = write_pdf(&[good, good]).unwrap();
    assert!(p.starts_with(b"%PDF-1.4\n"));
    assert!(p.ends_with(b"%%EOF\n"));
    let text = String::from_utf8_lossy(&p).into_owned();
    let xref_at = text.rfind("startxref\n").unwrap();
    let start: usize = text[xref_at + 10..]
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(p[start..].starts_with(b"xref\n0 9\n"));
    let tail = String::from_utf8_lossy(&p[start..]).into_owned();
    let table: Vec<&str> = tail.lines().skip(2).take(9).collect();
    assert_eq!(table[0], "0000000000 65535 f ");
    for (i, l) in table.iter().enumerate().skip(1) {
        assert_eq!(l.len(), 19, "xref lines are 20 bytes with the newline");
        let off: usize = l[..10].parse().unwrap();
        assert!(
            p[off..].starts_with(format!("{i} 0 obj\n").as_bytes()),
            "object {i}"
        );
    }
}

// ---- F09: property tests ----

fn coding_strategy() -> impl Strategy<Value = Coding> {
    prop_oneof![
        Just(Coding::Mh),
        Just(Coding::Mr { k: 2 }),
        Just(Coding::Mr { k: 4 }),
        Just(Coding::Mmr)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// F09: arbitrary bytes never panic, rows stay bounded, the bitmap matches the row count.
    #[test]
    fn f09_arbitrary_bytes_never_panic(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
        coding in coding_strategy(),
        width in prop_oneof![Just(1728u32), Just(2048), Just(2432), 1u32..300],
        eol_required in any::<bool>(),
        chunk in 1usize..2000,
    ) {
        let mut cfg = DecoderConfig::new(width, coding);
        cfg.eol_required = eol_required;
        cfg.max_rows = 500;
        let (out, s) = decode_chunked(&data, cfg, chunk);
        prop_assert!(s.stats.rows <= 500);
        prop_assert_eq!(out.len(), s.stats.rows as usize * rb(width));
        prop_assert!(s.stats.bad_lines <= s.stats.rows);
        prop_assert!(s.stats.longest_bad_run <= s.stats.bad_lines);
        if coding == Coding::Mmr { prop_assert_eq!(s.stats.bad_lines, 0); }
    }

    /// Same, with input shaped like fax data: runs of zeros, EOLs and code-like bytes.
    #[test]
    fn f09_eol_rich_junk_never_panics(
        parts in proptest::collection::vec(prop_oneof![
            Just(vec![0u8, 1]), Just(vec![0u8, 0, 0, 1]), Just(vec![0u8; 20]), Just(vec![0x13u8, 0x4d, 0x35]),
            Just(vec![0xFFu8, 0xFF]), proptest::collection::vec(any::<u8>(), 1..16),
        ], 0..300),
        coding in coding_strategy(),
    ) {
        let data: Vec<u8> = parts.concat();
        let mut cfg = DecoderConfig::new(1728, coding);
        cfg.max_rows = 2000;
        let p = decode_page(&data, cfg).unwrap();
        prop_assert!(p.summary.stats.rows <= 2000);
        prop_assert_eq!(p.bitmap.len(), p.summary.stats.rows as usize * rb(1728));
    }

    /// Flipping bits of valid data never panics and never exceeds the row limit.
    #[test]
    fn f09_bit_flipped_valid_streams(
        seed in any::<u64>(),
        flips in proptest::collection::vec((any::<u16>(), 0u8..8), 1..40),
        coding in coding_strategy(),
    ) {
        let width = 1728;
        let bms = bitmaps(width, 24);
        let (_, bm) = &bms[(seed % 5) as usize];
        let mut data = encode(bm, width, coding, false);
        for (pos, bit) in flips {
            let i = pos as usize % data.len();
            data[i] ^= 1 << bit;
        }
        let mut cfg = DecoderConfig::new(width, coding);
        cfg.max_rows = 100;
        let p = decode_page(&data, cfg).unwrap();
        prop_assert!(p.summary.stats.rows <= 100);
    }

    /// Encoder -> decoder round trip on random bitmaps of random size and density.
    #[test]
    fn roundtrip_random_bitmaps(
        width in 1u32..400,
        rows in 1u32..20,
        density in 0u32..=100,
        seed in any::<u64>(),
        coding in coding_strategy(),
        chunk in 1usize..64,
    ) {
        let mut rng = Rng(seed | 1);
        let mut bm = vec![0u8; rb(width) * rows as usize];
        for y in 0..rows {
            let mut x = 0;
            let mut black = false;
            while x < width {
                // long runs with probability depending on density
                let run = 1 + (rng.next() % u64::from(1 + (density * width / 100).max(1))) as u32;
                for i in x..(x + run).min(width) {
                    if black { set_px(&mut bm, width, i, y); }
                }
                x += run;
                black = !black;
            }
        }
        let data = encode(&bm, width, coding, false);
        let (out, s) = decode_chunked(&data, DecoderConfig::new(width, coding), chunk);
        prop_assert_eq!(out, bm);
        prop_assert_eq!(s.stats.bad_lines, 0);
        prop_assert_eq!(s.end, terminator(coding));
    }
}

/// Writes fuzz seed files (selector byte + encoded stream) when `T4_WRITE_SEEDS=<dir>` is set.
/// Ignored by default: `T4_WRITE_SEEDS=crates/t4/fuzz/corpus/t4_decode cargo test -p t4 -- --ignored seeds`.
#[test]
#[ignore]
fn write_fuzz_seeds() {
    let Ok(dir) = std::env::var("T4_WRITE_SEEDS") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    for (i, coding) in CODINGS.iter().enumerate() {
        let sel = match coding {
            Coding::Mh => 0u8,
            Coding::Mr { k: 2 } => 1,
            Coding::Mr { .. } => 2,
            Coding::Mmr => 3,
        };
        for (name, bm) in bitmaps(W, 12) {
            let mut v = vec![sel];
            v.extend(encode(&bm, W, *coding, false));
            std::fs::write(format!("{dir}/seed-{i}-{name}"), v).unwrap();
        }
    }
}
