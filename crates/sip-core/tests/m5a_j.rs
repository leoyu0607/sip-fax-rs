//! M5a (test plan `docs/test-plans/M5.md` §3): J01–J05 and J07, the receive jitter buffer.
//! (J06, the sender's 20 ms pacing, belongs to M5e.)
//!
//! A virtual clock drives the buffer the way fax-in will: every 20 ms (tick `k` is at
//! `20 000 · k` µs) the packets that have arrived by then are pushed, then 160 samples are pulled.
//! Packets carry random non-zero samples, so a misplaced or invented sample shows. All randomness
//! comes from a fixed-seed generator, three seeds per case.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use sip_core::rtp::{JitterBuffer, JitterConfig, Law, Pulled, RtpPacket};

const BLOCK: usize = 160;
const SEEDS: [u64; 3] = [0x5EED_0001, 0x5EED_0002, 0x5EED_0003];

// -------------------------------------------------------------------------------- test helpers

/// xorshift64*: small, fixed, no dependencies.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo + 1) as u64)) as i64
    }
    fn chance(&mut self, percent: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 * 100.0 < percent
    }
    /// A non-zero sample.
    fn sample(&mut self) -> i16 {
        loop {
            let s = (self.next() >> 40) as i16;
            if s != 0 {
                return s;
            }
        }
    }
}

fn random_stream(rng: &mut Rng, packets: usize) -> Vec<i16> {
    (0..packets * BLOCK).map(|_| rng.sample()).collect()
}

/// One packet on its way to the receiver.
#[derive(Clone)]
struct Arrival {
    at_us: i64,
    ssrc: u32,
    ts: u32,
    samples: Vec<i16>,
    /// Index of the packet in its stream (for the expectations only).
    idx: usize,
}

/// What a run produced.
struct Played {
    /// Samples of all blocks pulled while playing (gaps are zeros in here).
    out: Vec<i16>,
    /// Gap per block pulled while playing.
    gaps: Vec<u32>,
    /// Tick of the first block pulled while playing.
    first_play_tick: Option<usize>,
    /// Largest `buffered()` seen after a push or a pull.
    max_buffered: u32,
    /// `buffered()` just before the first block that played.
    buffered_at_start: u32,
    /// Blocks pulled while playing.
    playing_blocks: usize,
    jb: JitterBuffer,
}

/// Runs `ticks` ticks of 20 ms; `arrivals` need not be sorted.
fn run(cfg: JitterConfig, mut arrivals: Vec<Arrival>, ticks: usize) -> Played {
    arrivals.sort_by_key(|a| a.at_us); // stable: equal times keep their order
    let mut jb = JitterBuffer::new(cfg);
    let mut next = 0usize;
    let mut p = Played {
        out: Vec::new(),
        gaps: Vec::new(),
        first_play_tick: None,
        max_buffered: 0,
        buffered_at_start: 0,
        playing_blocks: 0,
        jb: jb.clone(),
    };
    for k in 0..ticks {
        let now = 20_000 * k as i64;
        while next < arrivals.len() && arrivals[next].at_us <= now {
            let a = &arrivals[next];
            jb.push(a.ssrc, a.ts, a.samples.clone());
            p.max_buffered = p.max_buffered.max(jb.buffered());
            next += 1;
        }
        let before = jb.buffered();
        let mut block = [0i16; BLOCK];
        let r = jb.pull(&mut block);
        p.max_buffered = p.max_buffered.max(jb.buffered());
        if r.playing {
            if p.first_play_tick.is_none() {
                p.first_play_tick = Some(k);
                p.buffered_at_start = before;
            }
            p.out.extend_from_slice(&block);
            p.gaps.push(r.gap);
            p.playing_blocks += 1;
        } else {
            assert!(block.iter().all(|&s| s == 0), "silence before playout");
            assert_eq!(r.gap, 0);
        }
    }
    p.jb = jb;
    p
}

/// Packets `0..n` of a stream, arriving `delay_us(n)` after their nominal time.
fn stream(
    ssrc: u32,
    ts0: u32,
    src: &[i16],
    start_us: i64,
    mut delay_us: impl FnMut(usize) -> i64,
) -> Vec<Arrival> {
    src.chunks(BLOCK)
        .enumerate()
        .map(|(n, c)| Arrival {
            at_us: start_us + 20_000 * n as i64 + delay_us(n),
            ssrc,
            ts: ts0.wrapping_add((BLOCK * n) as u32),
            samples: c.to_vec(),
            idx: n,
        })
        .collect()
}

fn cfg(start_blocks: u32, max_blocks: u32) -> JitterConfig {
    JitterConfig {
        start_depth: start_blocks * BLOCK as u32,
        max_depth: max_blocks * BLOCK as u32,
        resync_after: 16_000,
    }
}

/// No loss of any kind in the first `blocks` blocks (what the buffer plays after the stream's
/// end is silence, reported as gaps, and is not looked at).
fn assert_clean(p: &Played, blocks: usize) {
    let s = p.jb.stats();
    assert_eq!(s.late, 0, "late");
    assert_eq!(s.duplicate, 0, "duplicate");
    assert_eq!(s.resyncs, 0, "resyncs");
    assert_eq!(s.dropped, 0, "dropped");
    assert!(p.gaps[..blocks].iter().all(|&g| g == 0), "gaps");
}

// ----------------------------------------------------------------------------------------- J01

#[test]
fn j01_in_order_20_ms_output_equals_input_delayed_by_the_start_depth() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for start_blocks in [1u32, 2, 3, 4, 8] {
            let n = 150;
            let src = random_stream(&mut rng, n);
            let ssrc = rng.next() as u32;
            let ts0 = rng.next() as u32;
            let p = run(
                cfg(start_blocks, 40),
                stream(ssrc, ts0, &src, 0, |_| 0),
                n + 20,
            );
            // every sample out, in order, no gap, nothing invented
            assert_eq!(
                &p.out[..src.len()],
                &src[..],
                "seed {seed:#x} depth {start_blocks}"
            );
            assert_clean(&p, n);
            assert_eq!(p.jb.stats().packets, n as u64);
            // Playout starts as soon as `start_depth` samples are buffered: the buffer held exactly
            // that much when the first block was taken; since a tick pushes before it pulls, the
            // first block is played in tick `start_blocks - 1`, i.e. the output lags the input by
            // `start_depth - 20 ms` of audio and the buffer keeps `start_depth - 20 ms` behind.
            assert_eq!(p.buffered_at_start, start_blocks * BLOCK as u32);
            assert_eq!(p.first_play_tick, Some(start_blocks as usize - 1));
            assert!(
                p.max_buffered <= start_blocks * BLOCK as u32,
                "{}",
                p.max_buffered
            );
        }
    }
}

#[test]
fn j01_the_depth_is_a_sample_count_not_a_packet_count() {
    // 10 ms packets (80 samples) with a 60 ms start depth: starts with 6 packets in the buffer
    let mut rng = Rng::new(SEEDS[0]);
    let src: Vec<i16> = (0..80 * 100).map(|_| rng.sample()).collect();
    let mut arrivals = Vec::new();
    for (n, c) in src.chunks(80).enumerate() {
        arrivals.push(Arrival {
            at_us: 10_000 * n as i64,
            ssrc: 1,
            ts: 80 * n as u32,
            samples: c.to_vec(),
            idx: n,
        });
    }
    let p = run(cfg(3, 40).with_start(480), arrivals, 60);
    assert_eq!(&p.out[..src.len()], &src[..]);
    assert_clean(&p, src.len() / BLOCK);
    // two 10 ms packets per tick: the first tick with at least 480 samples buffered has 560
    assert_eq!(p.buffered_at_start, 560);
    assert_eq!(p.first_play_tick, Some(3));
}

trait WithStart {
    fn with_start(self, s: u32) -> Self;
}
impl WithStart for JitterConfig {
    fn with_start(mut self, s: u32) -> Self {
        self.start_depth = s;
        self
    }
}

#[test]
fn j01_nothing_plays_before_the_start_depth_and_pull_is_silent() {
    let mut jb = JitterBuffer::new(cfg(3, 40));
    let mut b = [1i16; BLOCK];
    assert_eq!(jb.pull(&mut b), Pulled::default());
    assert!(b.iter().all(|&s| s == 0));
    jb.push(1, 0, vec![5; BLOCK]);
    jb.push(1, 160, vec![5; BLOCK]);
    assert_eq!(jb.buffered(), 320);
    assert_eq!(
        jb.pull(&mut b),
        Pulled {
            playing: false,
            gap: 0
        }
    );
    jb.push(1, 320, vec![5; BLOCK]);
    assert_eq!(
        jb.pull(&mut b),
        Pulled {
            playing: true,
            gap: 0
        }
    );
    assert!(b.iter().all(|&s| s == 5));
    assert_eq!(jb.stats().gap_samples, 0);
    // empty packets are ignored
    jb.push(1, 480, vec![]);
    assert_eq!(jb.stats().packets, 3);
    // an empty output slice is fine
    let mut none: [i16; 0] = [];
    let _ = jb.pull(&mut none);
}

// ----------------------------------------------------------------------------------------- J02

#[test]
fn j02_jitter_of_40_ms_and_5_percent_reordering_give_the_input_back_sample_for_sample() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        let n = 1500; // 30 s
        let src = random_stream(&mut rng, n);
        let mut a = stream(7, rng.next() as u32, &src, 0, |_| {
            rng.range(-40_000, 40_000)
        });
        // 5 % reordering: a packet swaps its arrival time with the next one
        let mut swapped = 0;
        for i in 0..n - 1 {
            if rng.chance(5.0) {
                let t = a[i].at_us;
                a[i].at_us = a[i + 1].at_us;
                a[i + 1].at_us = t;
                swapped += 1;
            }
        }
        assert!(swapped > 30, "{swapped}");
        // The buffer has to be deeper than the spread of the arrivals (80 ms + 20 ms of swaps).
        let p = run(cfg(8, 80), a, n + 60);
        assert_eq!(&p.out[..src.len()], &src[..], "seed {seed:#x}");
        assert_clean(&p, n);
        assert_eq!(p.jb.stats().packets, n as u64);
        assert!(p.max_buffered <= 80 * BLOCK as u32);
    }
}

#[test]
fn j02_with_a_depth_below_the_jitter_nothing_is_misplaced_or_invented() {
    // The default G.711 depth (60 ms) is far below +-100 ms of jitter: packets arrive after their
    // time, the buffer runs dry and plays silence. What it plays is still only what arrived:
    // every block is silence reported as a gap, or a whole packet, and the packets come out in
    // their order, none twice.
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        let n = 1500;
        let src = random_stream(&mut rng, n);
        let a = stream(7, 1000, &src, 0, |_| rng.range(-100_000, 100_000));
        let p = run(JitterConfig::G711, a, n + 100);
        let s = p.jb.stats();
        let mut cursor = 0usize;
        let mut played = 0usize;
        let mut gap_blocks = 0usize;
        for (b, &g) in p.gaps.iter().enumerate() {
            let got = &p.out[b * BLOCK..(b + 1) * BLOCK];
            if g != 0 {
                assert_eq!(g as usize, BLOCK, "seed {seed:#x} block {b}");
                assert!(got.iter().all(|&x| x == 0));
                gap_blocks += 1;
                continue;
            }
            let idx = (cursor..n.min(cursor + 100))
                .find(|&i| got == &src[i * BLOCK..(i + 1) * BLOCK])
                .unwrap_or_else(|| panic!("seed {seed:#x}: block {b} is no packet after {cursor}"));
            cursor = idx + 1;
            played += 1;
        }
        eprintln!("{seed:#x}: {s:?}, played {played} of {n}, {gap_blocks} gap blocks");
        // the jitter really did hurt, and (almost) everything that arrived was played
        assert!(gap_blocks > 10, "{gap_blocks}");
        assert_eq!(s.resyncs, 0);
        assert_eq!(s.dropped, 0);
        assert_eq!(s.packets + s.late + s.duplicate, n as u64);
        assert_eq!(
            played as u64 + u64::from(p.jb.buffered()) / BLOCK as u64,
            s.packets
        );
    }
}

// ----------------------------------------------------------------------------------------- J03

/// Loss pattern: independent (`burst = 1`) or runs of `burst` packets starting with probability
/// `percent / burst` (so the average loss stays `percent`). The first and last 8 packets arrive.
fn loss(rng: &mut Rng, n: usize, percent: f64, burst: usize) -> Vec<bool> {
    let mut lost = vec![false; n];
    let mut i = 8;
    while i < n - 8 {
        if rng.chance(percent / burst as f64) {
            for l in lost.iter_mut().skip(i).take(burst) {
                *l = true;
            }
            i += burst;
        } else {
            i += 1;
        }
    }
    for l in lost.iter_mut().skip(n - 8) {
        *l = false;
    }
    lost
}

fn j03_case(seed: u64, percent: f64, burst: usize) {
    let mut rng = Rng::new(seed);
    let n = 5000; // 100 s
    let src = random_stream(&mut rng, n);
    let lost = loss(&mut rng, n, percent, burst);
    let lost_count = lost.iter().filter(|&&l| l).count();
    assert!(lost_count > 0);
    let a: Vec<Arrival> = stream(3, rng.next() as u32, &src, 0, |_| {
        rng.range(-10_000, 10_000)
    })
    .into_iter()
    .filter(|a| !lost[a.idx])
    .collect();
    let p = run(cfg(6, 60), a, n + 40);
    let s = p.jb.stats();
    let what = format!("seed {seed:#x} loss {percent}% burst {burst}");
    // Every 20 ms block is either the packet itself or 20 ms of silence reported as one gap of
    // 160 samples: the packets that follow a loss are exactly where their timestamps put them.
    for (b, &g) in p.gaps.iter().take(n).enumerate() {
        let got = &p.out[b * BLOCK..(b + 1) * BLOCK];
        if lost[b] {
            assert_eq!(g, BLOCK as u32, "{what}: block {b} is a gap");
            assert!(got.iter().all(|&x| x == 0), "{what}: block {b} silent");
        } else {
            assert_eq!(g, 0, "{what}: block {b}");
            assert_eq!(got, &src[b * BLOCK..(b + 1) * BLOCK], "{what}: block {b}");
        }
    }
    assert_eq!(
        p.gaps[..n].iter().map(|&g| u64::from(g)).sum::<u64>(),
        (lost_count * BLOCK) as u64,
        "{what}"
    );
    assert_eq!(s.late, 0, "{what}");
    assert_eq!(s.resyncs, 0, "{what}");
    assert_eq!(s.dropped, 0, "{what}");
    assert_eq!(s.packets, (n - lost_count) as u64, "{what}");
}

#[test]
fn j03_independent_loss_of_1_percent_is_silence_of_exactly_the_lost_20_ms() {
    for seed in SEEDS {
        j03_case(seed, 1.0, 1);
    }
}

#[test]
fn j03_independent_loss_of_3_percent_is_silence_of_exactly_the_lost_20_ms() {
    for seed in SEEDS {
        j03_case(seed, 3.0, 1);
    }
}

#[test]
fn j03_bursts_of_3_lost_packets_1_and_3_percent() {
    for seed in SEEDS {
        j03_case(seed, 1.0, 3);
        j03_case(seed, 3.0, 3);
    }
}

#[test]
fn j03_a_burst_of_many_lost_packets_the_gap_is_reported_per_block_and_alignment_survives() {
    let mut rng = Rng::new(SEEDS[1]);
    let n = 400;
    let src = random_stream(&mut rng, n);
    // 50 packets (1 s) missing in the middle: no packets for 1 s, then the stream resumes with its
    // own timestamps (not a resync: 8000 samples is below `resync_after`)
    let a: Vec<Arrival> = stream(3, 5000, &src, 0, |_| 0)
        .into_iter()
        .filter(|a| !(100..150).contains(&a.idx))
        .collect();
    let p = run(cfg(6, 60), a, n + 20);
    assert_eq!(
        p.gaps[..n].iter().map(|&g| u64::from(g)).sum::<u64>(),
        50 * BLOCK as u64
    );
    assert_eq!(p.jb.stats().resyncs, 0);
    assert_eq!(p.jb.stats().late, 0);
    for b in 0..n {
        let got = &p.out[b * BLOCK..(b + 1) * BLOCK];
        if (100..150).contains(&b) {
            assert!(got.iter().all(|&x| x == 0));
            assert_eq!(p.gaps[b], BLOCK as u32);
        } else {
            assert_eq!(got, &src[b * BLOCK..(b + 1) * BLOCK], "block {b}");
        }
    }
}

#[test]
fn j03_the_first_packets_lost_playout_starts_at_the_first_timestamp_that_arrived() {
    let mut rng = Rng::new(SEEDS[2]);
    let n = 100;
    let src = random_stream(&mut rng, n);
    let a: Vec<Arrival> = stream(3, 77_000, &src, 0, |_| 0)
        .into_iter()
        .filter(|a| a.idx >= 2)
        .collect();
    let p = run(cfg(3, 40), a, n + 10);
    assert_clean(&p, n - 2);
    assert_eq!(&p.out[..(n - 2) * BLOCK], &src[2 * BLOCK..]);
}

#[test]
fn j03_a_duplicate_and_a_late_packet_are_dropped_and_counted_not_played() {
    let mut rng = Rng::new(SEEDS[0]);
    let n = 60;
    let src = random_stream(&mut rng, n);
    let mut a = stream(3, 0, &src, 0, |_| 0);
    // packet 10 arrives twice (second copy 5 ms later); packet 20 arrives 200 ms late
    let mut dup = a[10].clone();
    dup.at_us += 5_000;
    a.push(dup);
    a[20].at_us += 200_000;
    let p = run(cfg(3, 40), a, n + 20);
    let s = p.jb.stats();
    assert_eq!(s.duplicate, 1);
    assert_eq!(s.late, 1);
    // packet 20 is a gap; everything else is in place
    assert_eq!(p.gaps[..n].iter().sum::<u32>(), BLOCK as u32);
    assert_eq!(p.gaps[20], BLOCK as u32);
    for b in (0..n).filter(|&b| b != 20) {
        assert_eq!(
            &p.out[b * BLOCK..(b + 1) * BLOCK],
            &src[b * BLOCK..(b + 1) * BLOCK],
            "{b}"
        );
    }
}

// ----------------------------------------------------------------------------------------- J04

#[test]
fn j04_timestamp_wrap_in_order_and_out_of_order_has_no_gap() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        // wrap before the first packet, in the middle of a packet (-100), on a boundary (-160),
        // and well before
        for before_wrap in [1u32, 100, 160, 161, 800, 5 * 160 + 3] {
            let n = 80;
            let src = random_stream(&mut rng, n);
            let ts0 = 0u32.wrapping_sub(before_wrap);
            // in order
            let p = run(cfg(3, 40), stream(5, ts0, &src, 0, |_| 0), n + 10);
            assert_eq!(
                &p.out[..src.len()],
                &src[..],
                "in order, {before_wrap} before the wrap"
            );
            assert_clean(&p, n);
            // with jitter and reordering across the wrap
            let mut a = stream(5, ts0, &src, 0, |_| rng.range(-30_000, 30_000));
            for i in 0..n - 1 {
                if rng.chance(10.0) {
                    let t = a[i].at_us;
                    a[i].at_us = a[i + 1].at_us;
                    a[i + 1].at_us = t;
                }
            }
            let p = run(cfg(8, 60), a, n + 20);
            assert_eq!(
                &p.out[..src.len()],
                &src[..],
                "jitter, {before_wrap} before the wrap"
            );
            assert_clean(&p, n);
        }
    }
}

#[test]
fn j04_a_packet_from_before_the_wrap_arriving_after_it_is_late_not_a_resync() {
    let mut rng = Rng::new(SEEDS[0]);
    let n = 60;
    let src = random_stream(&mut rng, n);
    let ts0 = 0u32.wrapping_sub(10 * 160);
    let mut a = stream(5, ts0, &src, 0, |_| 0);
    // packet 5 (before the wrap) arrives only when packet 14 (after it) is due
    a[5].at_us += 9 * 20_000;
    let p = run(cfg(3, 40), a, n + 10);
    let s = p.jb.stats();
    assert_eq!(s.late, 1);
    assert_eq!(s.resyncs, 0);
    assert_eq!(p.gaps[..n].iter().sum::<u32>(), BLOCK as u32);
    assert_eq!(p.gaps[5], BLOCK as u32);
    assert_eq!(&p.out[6 * BLOCK..n * BLOCK], &src[6 * BLOCK..]);
}

#[test]
fn j04_sequence_number_wrap_through_the_wire_format_and_both_laws() {
    // seq 65530.. wraps at packet 6, timestamp wraps at packet 12; the packets travel as bytes
    for law in [Law::Ulaw, Law::Alaw] {
        let mut rng = Rng::new(SEEDS[1]);
        let n = 60;
        let pcm: Vec<i16> = (0..n * BLOCK).map(|_| rng.sample()).collect();
        // what the receiver must produce: the codec round trip of the source
        let mut codes = Vec::new();
        law.encode(&pcm, &mut codes);
        let mut want = Vec::new();
        law.decode(&codes, &mut want);
        let seq0 = 65_530u16;
        let ts0 = 0u32.wrapping_sub(12 * 160);
        let mut wire: Vec<(i64, Vec<u8>)> = codes
            .chunks(BLOCK)
            .enumerate()
            .map(|(i, c)| {
                let pkt = RtpPacket::new(
                    law.payload_type(),
                    seq0.wrapping_add(i as u16),
                    ts0.wrapping_add((BLOCK * i) as u32),
                    0xABCD,
                    c.to_vec(),
                );
                (20_000 * i as i64 + rng.range(-20_000, 20_000), pkt.encode())
            })
            .collect();
        wire.sort_by_key(|w| w.0);
        let mut jb = JitterBuffer::new(cfg(6, 60));
        let mut out = Vec::new();
        let mut gaps_in_stream = 0;
        let mut next = 0;
        for k in 0..n + 20 {
            while next < wire.len() && wire[next].0 <= 20_000 * k as i64 {
                let p = RtpPacket::decode(&wire[next].1).unwrap();
                let mut s = Vec::new();
                law.decode(&p.payload, &mut s);
                jb.push(p.ssrc, p.timestamp, s);
                next += 1;
            }
            let mut b = [0i16; BLOCK];
            let r = jb.pull(&mut b);
            if r.playing {
                if out.len() < want.len() {
                    gaps_in_stream += r.gap;
                }
                out.extend_from_slice(&b);
            }
        }
        assert_eq!(&out[..want.len()], &want[..], "{law:?}");
        assert_eq!(gaps_in_stream, 0);
        assert_eq!(jb.stats().late, 0);
        assert_eq!(jb.stats().resyncs, 0);
    }
}

// ----------------------------------------------------------------------------------------- J05

/// Plays stream A for `a_packets`, then stream B (arrival continues at the same pace).
fn two_streams(
    rng: &mut Rng,
    a_ssrc: u32,
    a_ts0: u32,
    b_ssrc: u32,
    b_ts0: u32,
) -> (Vec<i16>, Vec<i16>, Vec<Arrival>) {
    let (na, nb) = (60, 60);
    let a_src = random_stream(rng, na);
    let b_src = random_stream(rng, nb);
    let mut arr = stream(a_ssrc, a_ts0, &a_src, 0, |_| 0);
    arr.extend(stream(b_ssrc, b_ts0, &b_src, 20_000 * na as i64, |_| 0));
    (a_src, b_src, arr)
}

/// The output is a prefix of A, then (not playing) nothing, then B from its first sample.
fn assert_a_then_b(p: &Played, a_src: &[i16], b_src: &[i16], what: &str) {
    let s = p.jb.stats();
    assert_eq!(s.resyncs, 1, "{what}: exactly one resync");
    // B is in the output, whole, in place, without a gap; what came before is A, in place
    let at = p
        .out
        .windows(b_src.len().min(BLOCK * 4))
        .position(|w| w == &b_src[..w.len()])
        .unwrap_or_else(|| panic!("{what}: B not found in the output"));
    assert_eq!(&p.out[at..at + b_src.len()], b_src, "{what}: B whole");
    assert_eq!(at % BLOCK, 0, "{what}: B starts on a block");
    assert!(at >= BLOCK, "{what}");
    assert_eq!(&p.out[..at], &a_src[..at], "{what}: A before it");
    // A is cut at the switch: at most the buffer depth of its tail is lost
    assert!(
        at + 3 * BLOCK * 2 >= a_src.len(),
        "{what}: lost too much of A ({at})"
    );
    assert!(
        p.gaps[..(at + b_src.len()) / BLOCK].iter().all(|&g| g == 0),
        "{what}: no gap in A or B"
    );
    assert_eq!(s.late, 0, "{what}");
}

#[test]
fn j05_a_new_ssrc_starts_over_once_and_the_new_stream_plays_from_its_first_sample() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for b_ts0 in [rng.next() as u32, 5_000_000, 100] {
            let (a, b, arr) = two_streams(&mut rng, 11, 1_000, 22, b_ts0);
            let p = run(cfg(3, 40), arr, 140);
            assert_a_then_b(&p, &a, &b, &format!("seed {seed:#x} ts {b_ts0}"));
        }
    }
}

#[test]
fn j05_a_new_ssrc_whose_timestamps_continue_the_old_ones_is_also_one_resync() {
    let mut rng = Rng::new(SEEDS[0]);
    let (a, b, arr) = two_streams(&mut rng, 11, 1_000, 22, 1_000 + 60 * 160);
    let p = run(cfg(3, 40), arr, 140);
    assert_a_then_b(&p, &a, &b, "continuing timestamps");
}

#[test]
fn j05_a_timestamp_jump_beyond_resync_after_forward_and_backward() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for (a_ts0, b_ts0) in [
            (1_000u32, 1_000u32 + 60 * 160 + 16_001), // forward, just past the limit
            (3_000_000, 1_000_000),                   // backward
            (10, 0x4000_0000),                        // forward by a billion
            (0xF000_0000, 0x1000_0000),               // across the wrap, forward by 2^29
        ] {
            let (a, b, arr) = two_streams(&mut rng, 11, a_ts0, 11, b_ts0);
            let p = run(cfg(3, 40), arr, 140);
            assert_a_then_b(&p, &a, &b, &format!("seed {seed:#x} {a_ts0}->{b_ts0}"));
        }
    }
}

#[test]
fn j05_a_jump_below_resync_after_is_a_gap_not_a_resync() {
    let mut rng = Rng::new(SEEDS[2]);
    let na = 60;
    let nb = 60;
    let a_src = random_stream(&mut rng, na);
    let b_src = random_stream(&mut rng, nb);
    // 1 s of silence in the timestamps (8000 samples), the same SSRC
    let mut arr = stream(5, 0, &a_src, 0, |_| 0);
    arr.extend(stream(
        5,
        (na * BLOCK + 8000) as u32,
        &b_src,
        20_000 * (na as i64 + 50),
        |_| 0,
    ));
    let p = run(cfg(3, 400), arr, 200);
    let s = p.jb.stats();
    assert_eq!(s.resyncs, 0);
    assert_eq!(p.gaps[..na + 50 + nb].iter().sum::<u32>(), 8000);
    assert_eq!(&p.out[..na * BLOCK], &a_src[..]);
    assert_eq!(
        &p.out[na * BLOCK + 8000..(na + nb) * BLOCK + 8000],
        &b_src[..]
    );
}

#[test]
fn j05_a_jump_before_playout_started_heals_with_one_resync_at_the_next_packet() {
    // Same SSRC, one packet, then a far timestamp: playout has not started, so the jump is only
    // noticed once the buffer plays, at the next push. That push restarts the buffer; the packet
    // that was already buffered (the first of the new stream) goes with the old one, the rest of
    // the new stream comes out whole.
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        let b_src = random_stream(&mut rng, 40);
        let mut arr = vec![Arrival {
            at_us: 0,
            ssrc: 5,
            ts: 1000,
            samples: vec![9; BLOCK],
            idx: 0,
        }];
        arr.extend(stream(5, 10_000_000, &b_src, 20_000, |_| 0));
        let p = run(cfg(3, 40), arr, 60);
        assert_eq!(p.jb.stats().resyncs, 1, "seed {seed:#x}");
        let at = p
            .out
            .windows(BLOCK)
            .position(|w| w == &b_src[BLOCK..2 * BLOCK])
            .expect("the new stream plays");
        let n = 39 - 1;
        assert_eq!(
            &p.out[at..at + n * BLOCK],
            &b_src[BLOCK..BLOCK + n * BLOCK],
            "seed {seed:#x}"
        );
        assert!(p.gaps[at / BLOCK..at / BLOCK + n].iter().all(|&g| g == 0));
    }
}

#[test]
fn j05_after_a_resync_the_counters_keep_counting_and_the_buffer_is_usable_again() {
    let mut jb = JitterBuffer::new(cfg(2, 40));
    for round in 0..5u32 {
        let ssrc = 100 + round;
        for k in 0..6u32 {
            jb.push(
                ssrc,
                7 * round * 100_000 + k * 160,
                vec![round as i16 + 1; BLOCK],
            );
            let mut b = [0i16; BLOCK];
            let r = jb.pull(&mut b);
            if r.playing {
                assert!(
                    b.iter().all(|&s| s == round as i16 + 1),
                    "round {round} tick {k}"
                );
            }
        }
    }
    assert_eq!(jb.stats().resyncs, 4);
}

// ----------------------------------------------------------------------------------------- J07

/// Samples that identify their own position: `(i mod 30000) + 1`.
fn counting(i: u64) -> i16 {
    ((i % 30_000) + 1) as i16
}

struct Drift {
    max_buffered: u32,
    jb: JitterBuffer,
    played_real: u64,
    playing_blocks: u64,
    /// Places where two consecutive non-gap output samples are not consecutive in the source.
    jumps: u64,
    buffered_end: u32,
}

/// The sender's clock is `ppm` parts per million fast (+) or slow (-) against the receiver's;
/// `minutes` of virtual time, each packet also arrives `+-jitter_us` off its time.
fn drift(ppm: f64, minutes: u32, jitter_us: i64, seed: u64, config: JitterConfig) -> Drift {
    let mut rng = Rng::new(seed);
    let ticks = (minutes as usize) * 60 * 50;
    let period_us = 20_000.0 / (1.0 + ppm / 1e6);
    let n = (ticks as f64 * 20_000.0 / period_us) as usize + 20;
    let arrivals: Vec<(i64, u32, Vec<i16>)> = (0..n)
        .map(|i| {
            let t = (i as f64 * period_us) as i64 + rng.range(-jitter_us, jitter_us.max(0));
            let s: Vec<i16> = (0..BLOCK as u64)
                .map(|j| counting(i as u64 * 160 + j))
                .collect();
            (t, (i * BLOCK) as u32, s)
        })
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| arrivals[i].0);
    let mut jb = JitterBuffer::new(config);
    let mut d = Drift {
        max_buffered: 0,
        jb: jb.clone(),
        played_real: 0,
        playing_blocks: 0,
        jumps: 0,
        buffered_end: 0,
    };
    let mut next = 0;
    let mut prev: Option<i16> = None;
    for k in 0..ticks {
        let now = 20_000 * k as i64;
        while next < n && arrivals[order[next]].0 <= now {
            let a = &arrivals[order[next]];
            jb.push(1, a.1, a.2.clone());
            d.max_buffered = d.max_buffered.max(jb.buffered());
            next += 1;
        }
        let mut b = [0i16; BLOCK];
        let r = jb.pull(&mut b);
        d.max_buffered = d.max_buffered.max(jb.buffered());
        if r.playing {
            d.playing_blocks += 1;
            for &s in &b {
                if s == 0 {
                    continue; // silence: the source index carries on across it
                }
                d.played_real += 1;
                if let Some(p) = prev
                    && s != (p % 30_000) + 1
                {
                    d.jumps += 1;
                }
                prev = Some(s);
            }
        }
    }
    d.buffered_end = jb.buffered();
    d.jb = jb;
    d
}

fn check_drift(d: &Drift, config: JitterConfig, what: &str) {
    let s = d.jb.stats();
    assert!(
        d.max_buffered <= config.max_depth,
        "{what}: buffer {} > {}",
        d.max_buffered,
        config.max_depth
    );
    assert_eq!(s.resyncs, 0, "{what}");
    // Every accepted sample is played, dropped by the trim, or still in the buffer.
    assert_eq!(
        s.packets * BLOCK as u64,
        d.played_real + s.dropped + u64::from(d.buffered_end),
        "{what}: sample accounting"
    );
    // The signal is cut only where the buffer says so: a trim (`dropped`) or a late packet; a
    // sender that is slow or merely has run dry loses nothing (the wait is silence).
    assert!(
        d.jumps <= s.dropped / BLOCK as u64 + s.late,
        "{what}: {} jumps, dropped {}, late {}",
        d.jumps,
        s.dropped,
        s.late
    );
    if s.dropped == 0 && s.late == 0 {
        assert_eq!(d.jumps, 0, "{what}: nothing may be lost");
    }
    assert_eq!(
        d.playing_blocks * BLOCK as u64,
        d.played_real + s.gap_samples,
        "{what}"
    );
}

#[test]
fn j07_plus_and_minus_200_ppm_for_10_minutes_the_buffer_stays_inside_its_limits() {
    let c = JitterConfig::G711;
    for seed in SEEDS {
        for ppm in [200.0, -200.0] {
            let d = drift(ppm, 10, 0, seed, c);
            let what = format!("seed {seed:#x} {ppm} ppm");
            check_drift(&d, c, &what);
            let s = d.jb.stats();
            eprintln!(
                "{what}: max buffered {} samples, dropped {}, gap {}, late {}, jumps {}",
                d.max_buffered, s.dropped, s.gap_samples, s.late, d.jumps
            );
            // 10 minutes of 200 ppm is 120 ms of audio: far less than one glitch per minute
            assert!(s.dropped + s.gap_samples <= 4 * 160 * 2, "{what}");
            assert!(d.jumps <= 4, "{what}");
        }
    }
}

#[test]
fn j07_a_buffer_that_ran_dry_waits_for_its_depth_and_plays_the_late_packet() {
    // The defect J07 found: playout kept running by the clock after the buffer ran dry, so the
    // position was ahead of every packet to come, each of them arrived late and was dropped,
    // for good (a sender clock 200 ppm slow made every block a gap after 2.5 minutes).
    let mut jb = JitterBuffer::new(cfg(3, 40));
    let pkt = |n: u32| vec![(n + 1) as i16; BLOCK];
    for n in 0..3 {
        jb.push(1, n * 160, pkt(n));
    }
    let mut b = [0i16; BLOCK];
    for n in 0..3 {
        assert_eq!(
            jb.pull(&mut b),
            Pulled {
                playing: true,
                gap: 0
            }
        );
        assert_eq!(b, pkt(n).as_slice());
    }
    // dry: nothing buffered; the block is silence and a gap
    assert_eq!(
        jb.pull(&mut b),
        Pulled {
            playing: true,
            gap: 160
        }
    );
    assert!(b.iter().all(|&x| x == 0));
    // packet 3 arrives just after its slot: it is not late, it is played, but only after the
    // buffer has its depth again (packets 3, 4, 5)
    jb.push(1, 3 * 160, pkt(3));
    assert_eq!(
        jb.pull(&mut b),
        Pulled {
            playing: true,
            gap: 160
        }
    );
    jb.push(1, 4 * 160, pkt(4));
    assert_eq!(
        jb.pull(&mut b),
        Pulled {
            playing: true,
            gap: 160
        }
    );
    jb.push(1, 5 * 160, pkt(5));
    for n in 3..6 {
        assert_eq!(
            jb.pull(&mut b),
            Pulled {
                playing: true,
                gap: 0
            },
            "packet {n}"
        );
        assert_eq!(b, pkt(n).as_slice());
    }
    assert_eq!(jb.stats().late, 0);
    assert_eq!(jb.stats().resyncs, 0);
    // a loss that outlasts the buffer resumes at the first packet that comes, not before
    jb.push(1, 9 * 160, pkt(9));
    jb.push(1, 10 * 160, pkt(10));
    jb.push(1, 11 * 160, pkt(11));
    let mut played = Vec::new();
    for _ in 0..6 {
        let r = jb.pull(&mut b);
        played.push((r.gap, b[0]));
    }
    // blocks 6, 7, 8 are the three lost packets (silence), then 9, 10, 11
    assert_eq!(
        played,
        vec![(160, 0), (160, 0), (160, 0), (0, 10), (0, 11), (0, 12)]
    );
}

#[test]
fn j07_with_jitter_on_top_of_the_drift() {
    let c = JitterConfig {
        start_depth: 960,
        max_depth: 3200,
        resync_after: 16_000,
    };
    for seed in SEEDS {
        for ppm in [200.0, -200.0] {
            let d = drift(ppm, 10, 15_000, seed, c);
            check_drift(&d, c, &format!("seed {seed:#x} {ppm} ppm + jitter"));
        }
    }
}

#[test]
fn j07_a_much_faster_or_slower_sender_is_trimmed_or_gapped_but_never_overflows() {
    let c = JitterConfig::G711;
    for ppm in [2000.0, -2000.0, 10_000.0, -10_000.0] {
        let d = drift(ppm, 10, 0, SEEDS[0], c);
        let what = format!("{ppm} ppm");
        check_drift(&d, c, &what);
        let s = d.jb.stats();
        if ppm > 0.0 {
            assert!(s.dropped > 0, "{what}: a fast sender must be trimmed");
        } else {
            assert!(s.gap_samples > 0, "{what}: a slow sender runs dry");
        }
    }
}

// ------------------------------------------------------------------------------------ proptest

fn runner(seed: u8, cases: u32) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

#[derive(Debug, Clone)]
enum Op {
    Push {
        ssrc: u8,
        ts: u32,
        len: usize,
        v: i16,
    },
    Pull(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (
            0u8..3,
            prop_oneof![
                any::<u32>(),
                (0u32..4000).prop_map(|t| t * 40),
                (0u32..100).prop_map(|t| 0u32.wrapping_sub(t * 40)),
            ],
            0usize..400,
            any::<i16>()
        )
            .prop_map(|(ssrc, ts, len, v)| Op::Push { ssrc, ts, len, v }),
        (0usize..500).prop_map(Op::Pull),
    ]
}

#[test]
fn j_proptest_any_sequence_of_pushes_and_pulls_never_panics_and_keeps_its_invariants() {
    for seed in [1u8, 2, 3] {
        runner(seed, 400)
            .run(&proptest::collection::vec(op(), 0..120), |ops| {
                let c = JitterConfig {
                    start_depth: 320,
                    max_depth: 1600,
                    resync_after: 16_000,
                };
                let mut jb = JitterBuffer::new(c);
                let mut last = jb.stats();
                let mut playing_at_resyncs: Option<u64> = None;
                for o in ops {
                    match o {
                        Op::Push { ssrc, ts, len, v } => {
                            let before = jb.stats().packets;
                            jb.push(u32::from(ssrc), ts, vec![v; len]);
                            // while playing, a push that is accepted leaves at most max_depth
                            if jb.stats().packets > before
                                && playing_at_resyncs == Some(jb.stats().resyncs)
                            {
                                prop_assert!(jb.buffered() <= c.max_depth, "{}", jb.buffered());
                            }
                        }
                        Op::Pull(n) => {
                            let mut b = vec![1i16; n];
                            let r = jb.pull(&mut b);
                            prop_assert!(r.gap as usize <= n);
                            if !r.playing {
                                prop_assert_eq!(r.gap, 0);
                                prop_assert!(b.iter().all(|&s| s == 0));
                            } else {
                                playing_at_resyncs = Some(jb.stats().resyncs);
                            }
                        }
                    }
                    let s = jb.stats();
                    prop_assert!(s.packets >= last.packets && s.late >= last.late);
                    prop_assert!(s.duplicate >= last.duplicate && s.resyncs >= last.resyncs);
                    prop_assert!(s.gap_samples >= last.gap_samples && s.dropped >= last.dropped);
                    last = s;
                }
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn j_proptest_any_order_within_the_depth_gives_the_stream_back() {
    // 40 packets, shuffled inside windows of 4 (so never more than 3 packets = 60 ms out of place),
    // a depth of 6 packets, random packet sizes of 80 or 160 samples that tile the timeline
    for seed in [1u8, 2, 3] {
        runner(seed, 200)
            .run(
                &(
                    proptest::collection::vec(prop_oneof![Just(80usize), Just(160usize)], 40..60),
                    any::<u32>(),
                    any::<u64>(),
                ),
                |(sizes, ts0, shuffle_seed)| {
                    let mut rng = Rng::new(shuffle_seed);
                    let mut src = Vec::new();
                    let mut pk: Vec<(u32, Vec<i16>)> = Vec::new();
                    let mut ts = ts0;
                    for &sz in &sizes {
                        let s: Vec<i16> = (0..sz).map(|_| rng.sample()).collect();
                        src.extend_from_slice(&s);
                        pk.push((ts, s));
                        ts = ts.wrapping_add(sz as u32);
                    }
                    // shuffle inside windows of 4
                    for w in pk.chunks_mut(4) {
                        for i in (1..w.len()).rev() {
                            let j = rng.range(0, i as i64) as usize;
                            w.swap(i, j);
                        }
                    }
                    let mut jb = JitterBuffer::new(JitterConfig {
                        start_depth: 960,
                        max_depth: 6400,
                        resync_after: 16_000,
                    });
                    let mut out: Vec<i16> = Vec::new();
                    let mut pushed = 0usize;
                    let mut it = pk.into_iter();
                    // keep 1600 samples ahead of what was played: more than the 3 packets
                    // (480 samples) that sit out of place
                    for _ in 0..2000 {
                        if out.len() >= src.len() {
                            break;
                        }
                        while pushed < out.len() + 1600 {
                            let Some((ts, s)) = it.next() else { break };
                            pushed += s.len();
                            jb.push(1, ts, s);
                        }
                        let mut b = [0i16; 160];
                        let r = jb.pull(&mut b);
                        if r.playing {
                            out.extend_from_slice(&b);
                        }
                    }
                    prop_assert!(out.len() >= src.len());
                    prop_assert_eq!(&out[..src.len()], &src[..]);
                    prop_assert_eq!(jb.stats().late, 0);
                    prop_assert_eq!(jb.stats().resyncs, 0);
                    Ok(())
                },
            )
            .unwrap();
    }
}
