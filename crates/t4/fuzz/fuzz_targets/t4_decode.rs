#![no_main]
//! Feeds arbitrary bytes to the T.4 / T.6 decoder (first byte selects coding, width, chunking
//! and options). Whatever it decodes must re-encode as MMR and decode to the same pixels.

use libfuzzer_sys::fuzz_target;
use t4::{Coding, Decoder, DecoderConfig, decode_page, encode_mmr, row_bytes};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, data)) = data.split_first() else { return };
    let coding = match sel & 3 {
        0 => Coding::Mh,
        1 => Coding::Mr { k: 2 },
        2 => Coding::Mr { k: 4 },
        _ => Coding::Mmr,
    };
    let width = match (sel >> 2) & 3 {
        0 => 1728,
        1 => 2048,
        2 => 2432,
        _ => 1 + u32::from(sel >> 4) * 37,
    };
    let mut cfg = DecoderConfig::new(width, coding);
    cfg.eol_required = sel & 0x80 == 0 || coding == Coding::Mmr;
    cfg.max_rows = 300;

    // One-shot and 8..=71 byte chunks must agree (1-byte chunks are quadratic in the row size
    // and are covered by the unit tests instead).
    let Ok(page) = decode_page(data, cfg) else { return };
    let chunk = 8 + usize::from(sel >> 3) % 64;
    let mut dec = Decoder::new(cfg).unwrap();
    let mut out = Vec::new();
    for c in data.chunks(chunk) {
        dec.push(c, &mut |r| out.extend_from_slice(r));
    }
    let summary = dec.finish(&mut |r| out.extend_from_slice(r));
    assert_eq!(out, page.bitmap, "chunked decode differs");
    assert_eq!(summary, page.summary);

    let s = page.summary.stats;
    assert!(s.rows <= 300 && s.bad_lines <= s.rows && s.longest_bad_run <= s.bad_lines);
    assert_eq!(page.bitmap.len(), s.rows as usize * row_bytes(width));

    // Round trip through our own MMR encoder.
    if s.rows > 0 {
        let g4 = encode_mmr(&page.bitmap, width).unwrap();
        let again = decode_page(&g4, DecoderConfig::new(width, Coding::Mmr)).unwrap();
        assert_eq!(again.bitmap, page.bitmap, "MMR re-encode round trip");
        assert_eq!(again.summary.stats.rows, s.rows);
    }
});
