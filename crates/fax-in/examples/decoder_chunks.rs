//! M2c leftover: throughput of the T.4 decoder fed with T.38-sized chunks.
//!
//! Run: `cargo run --release -p fax-in --example decoder_chunks`
//!
//! Decodes the same pages one-shot and in chunks of 1..300 octets (what `hdlc-data` /
//! `t4-non-ecm-data` fields look like: 1 octet at V.21 pacing, ~54 octets per 30 ms packet at
//! 14400 bit/s, up to 300 unpaced) and reports the time against the time the data needs on the
//! line.

use std::time::Instant;
use t4::{Coding, Decoder, DecoderConfig, Encoder};

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// Text-like page: lines of glyph-sized black runs on white.
fn text_page(rows: usize) -> Vec<u8> {
    let mut seed = 0x1234_5678_9ABC_DEF1u64;
    let mut v = vec![0u8; rows * 216];
    for y in 0..rows {
        if (y / 20) % 3 == 2 {
            continue; // blank line spacing
        }
        for x in 4..212 {
            if rng(&mut seed) % 5 < 2 {
                v[y * 216 + x] = (rng(&mut seed) & 0xFF) as u8 & (rng(&mut seed) & 0xFF) as u8;
            }
        }
    }
    v
}

/// Worst case for the per-row restart: every pixel random (rows of ~1.5 KB).
fn noise_page(rows: usize) -> Vec<u8> {
    let mut seed = 0xDEAD_BEEF_0BAD_F00Du64;
    (0..rows * 216).map(|_| rng(&mut seed) as u8).collect()
}

fn encode(bm: &[u8], rows: usize, coding: Coding) -> Vec<u8> {
    let mut e = Encoder::new(1728, coding).unwrap();
    for y in 0..rows {
        e.push_row(&bm[y * 216..(y + 1) * 216]).unwrap();
    }
    e.finish()
}

fn decode(data: &[u8], coding: Coding, chunk: usize) -> (f64, u32) {
    let mut cfg = DecoderConfig::new(1728, coding);
    cfg.max_rows = 5000;
    let mut d = Decoder::new(cfg).unwrap();
    let mut sink_rows = 0u32;
    let t = Instant::now();
    for c in data.chunks(chunk) {
        d.push(c, &mut |_| sink_rows += 1);
    }
    let s = d.finish(&mut |_| sink_rows += 1);
    (
        t.elapsed().as_secs_f64() * 1000.0,
        s.stats.rows.max(sink_rows),
    )
}

fn main() {
    let rows = 2200; // an A4 page at fine resolution
    for (label, bm) in [
        ("text-like", text_page(rows)),
        ("random noise", noise_page(rows)),
    ] {
        for (cname, coding) in [
            ("MH", Coding::Mh),
            ("MR", Coding::Mr { k: 4 }),
            ("MMR", Coding::Mmr),
        ] {
            let data = encode(&bm, rows, coding);
            let line_s = data.len() as f64 * 8.0 / 14400.0;
            println!(
                "\n{label} {cname}: {} bytes ({:.1} s on a 14400 bit/s line)",
                data.len(),
                line_s
            );
            println!("  chunk   decode ms   x real time");
            for chunk in [1usize, 2, 4, 8, 16, 32, 54, 100, 300, usize::MAX] {
                let (ms, r) = decode(&data, coding, chunk);
                assert!(r > 0);
                let name = if chunk == usize::MAX {
                    "whole".to_string()
                } else {
                    chunk.to_string()
                };
                println!("  {name:>5}  {ms:>9.2}   {:>10.5}", ms / 1000.0 / line_s);
            }
        }
    }
}
