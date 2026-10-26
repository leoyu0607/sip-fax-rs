//! M5b (test plan `docs/test-plans/M5.md` section 5): H01-H03, the HDLC framing of the T.30
//! control channel, without SpanDSP (the SpanDSP comparison is `tools/fax-sim/tests/
//! h_hdlc_spandsp.rs`).
//!
//! * H01: the sender against a second, differently written encoder (a serial CRC register and a
//!   bit-by-bit stuffer) and against fixed vectors.
//! * H02: the receiver on well-formed streams: preambles of 1 s, many frames, shared and extra
//!   flags, garbage before the first flag.
//! * H03: damage: wrong FCS, abort, short and odd-length frames, overlong frames; proptest on
//!   arbitrary bit streams (never panics, output bounded). The memory bound is in
//!   `m5b_h03_memory.rs` (it needs its own allocator).

use fax_modem::hdlc::{FLAG, HdlcEvent, HdlcRx, MAX_FRAME, encode_burst, fcs16};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

const SEEDS: [u64; 3] = [0x5B01_1001, 0x5B01_1002, 0x5B01_1003];

/// xorshift64*.
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
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
    /// A frame: random octets, a quarter of the frames with long runs of ones.
    fn frame(&mut self, max: usize) -> Vec<u8> {
        let n = 1 + self.below(max);
        let mut f = self.bytes(n);
        if self.below(4) == 0 {
            for b in f.iter_mut() {
                if self.below(3) != 0 {
                    *b = 0xFF;
                }
            }
        }
        f
    }
}

fn flag_bits(n: usize) -> Vec<bool> {
    (0..n)
        .flat_map(|_| (0..8).map(|i| FLAG >> i & 1 != 0))
        .collect()
}

fn octet_bits(data: &[u8]) -> Vec<bool> {
    data.iter()
        .flat_map(|&b| (0..8).map(move |i| b >> i & 1 != 0))
        .collect()
}

/// The receiver's events over `bits` (a fresh receiver).
fn receive(bits: &[bool]) -> Vec<HdlcEvent> {
    let mut rx = HdlcRx::new();
    bits.iter().filter_map(|&b| rx.bit(b)).collect()
}

fn frames_of(ev: &[HdlcEvent]) -> Vec<(Vec<u8>, bool)> {
    ev.iter()
        .filter_map(|e| match e {
            HdlcEvent::Frame { data, fcs_ok } => Some((data.clone(), *fcs_ok)),
            _ => None,
        })
        .collect()
}

fn good(f: &[Vec<u8>]) -> Vec<(Vec<u8>, bool)> {
    f.iter().map(|d| (d.clone(), true)).collect()
}

// ------------------------------------------------------------------------------ the reference

/// The FCS as the standard describes the circuit: a 16-bit shift register preset to ones, the
/// data bits (each octet least significant bit first) shifted in through x^12 + x^5 + 1 at the
/// input, the register inverted at the end and sent from its x^15 end.
fn ref_fcs_bits(data: &[u8]) -> Vec<bool> {
    let mut reg: u16 = 0xFFFF;
    for bit in octet_bits(data) {
        let feedback = (reg >> 15 & 1 != 0) ^ bit;
        reg <<= 1;
        if feedback {
            reg ^= 0x1021;
        }
    }
    let reg = !reg;
    (0..16).map(|i| reg >> (15 - i) & 1 != 0).collect()
}

/// One frame as a stuffed bit string: after five ones in a row of the data and FCS, a zero.
fn ref_frame_bits(data: &[u8]) -> Vec<bool> {
    let mut raw = octet_bits(data);
    raw.extend(ref_fcs_bits(data));
    let mut out = Vec::new();
    let mut run = 0;
    for b in raw {
        out.push(b);
        run = if b { run + 1 } else { 0 };
        if run == 5 {
            out.push(false);
            run = 0;
        }
    }
    out
}

fn ref_burst(frames: &[Vec<u8>], pre: usize, post: usize) -> Vec<bool> {
    let mut out = flag_bits(pre.max(1));
    for (i, f) in frames.iter().enumerate() {
        if i > 0 {
            out.extend(flag_bits(1));
        }
        out.extend(ref_frame_bits(f));
    }
    out.extend(flag_bits(post.max(1)));
    out
}

// ------------------------------------------------------------------------------------- H01

#[test]
fn h01_fcs_matches_the_published_check_value_and_the_circuit() {
    // CRC-16/X-25 (the HDLC FCS) of "123456789" is 0x906E (CRC RevEng catalogue)
    assert_eq!(fcs16(b"123456789"), 0x906E);
    // the circuit of the standard (a shift register, sent from its x^15 end) gives the same 16
    // bits as ours (the low octet first, each octet least significant bit first)
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..500 {
            let f = rng.frame(300);
            let fcs = fcs16(&f);
            assert_eq!(octet_bits(&[fcs as u8, (fcs >> 8) as u8]), ref_fcs_bits(&f));
        }
    }
}

#[test]
fn h01_fixed_vectors() {
    // a flag, 0x7E as data (stuffed), a flag: 0x7E = 0111 1110 sent LSB first is 0 1 1 1 1 1 1 0:
    // the stuffed zero goes after the fifth one
    let f = vec![0x7Eu8];
    let bits = ref_frame_bits(&f);
    let s: String = bits.iter().map(|&b| if b { '1' } else { '0' }).collect();
    // data 01111110, then the FCS bits; the first 8 + 1 are 0 1 1 1 1 1 [0] 1 0
    assert!(s.starts_with("011111010"), "{s}");
    // 4 octets of ones: 11111 0 11111 0 11111 0 11111 0 11 ...
    let f = vec![0xFFu8; 4];
    let s: String = ref_frame_bits(&f)
        .iter()
        .map(|&b| if b { '1' } else { '0' })
        .collect();
    assert!(s.starts_with("11111011111011111011111011"), "{s}");
    // the burst of an empty list: just the flags
    assert_eq!(
        encode_burst(&[], 3, 2),
        flag_bits(3)
            .into_iter()
            .chain(flag_bits(2))
            .collect::<Vec<_>>()
    );
}

#[test]
fn h01_the_sender_equals_the_reference_encoder() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..300 {
            let n = 1 + rng.below(5);
            let frames: Vec<Vec<u8>> = (0..n).map(|_| rng.frame(MAX_FRAME)).collect();
            let pre = rng.below(45);
            let post = rng.below(4);
            assert_eq!(
                encode_burst(&frames, pre, post),
                ref_burst(&frames, pre, post),
                "seed {seed:#x}, {} frames, {pre} preamble flags",
                frames.len()
            );
        }
    }
}

#[test]
fn h01_every_two_octet_frame_every_stuffing_boundary() {
    // 65 536 frames: every combination of how one octet ends and the next begins, and of the FCS
    // after them
    let frames: Vec<Vec<u8>> = (0..=0xFFFFu32)
        .map(|v| vec![(v >> 8) as u8, v as u8])
        .collect();
    assert!(encode_burst(&frames, 38, 2) == ref_burst(&frames, 38, 2));
}

#[test]
fn h01_no_six_ones_in_a_row_outside_the_flags() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..200 {
            let f = rng.frame(MAX_FRAME);
            // one frame between 4 flags on each side: strip the flags and look for six ones
            let bits = encode_burst(std::slice::from_ref(&f), 4, 4);
            let body = &bits[32..bits.len() - 32];
            let mut run = 0;
            for &b in body {
                run = if b { run + 1 } else { 0 };
                assert!(run <= 5, "six ones in a row inside a frame");
            }
        }
    }
}

// ------------------------------------------------------------------------------------- H02

#[test]
fn h02_a_second_of_flags_then_frames() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..100 {
            let n = 1 + rng.below(5);
            let frames: Vec<Vec<u8>> = (0..n).map(|_| rng.frame(MAX_FRAME)).collect();
            // 1 s of flags = 300 bits = 38 flags (37.5)
            let ev = receive(&encode_burst(&frames, 38, 2));
            assert_eq!(ev[0], HdlcEvent::Preamble, "seed {seed:#x}");
            assert_eq!(ev.iter().filter(|e| **e == HdlcEvent::Preamble).count(), 1);
            assert_eq!(frames_of(&ev), good(&frames), "seed {seed:#x}");
            assert!(
                !ev.contains(&HdlcEvent::Abort),
                "an abort in a clean burst: {ev:?}"
            );
        }
    }
}

#[test]
fn h02_the_longest_frame_and_every_length_up_to_20() {
    let mut rng = Rng::new(SEEDS[0]);
    for len in (1..=20).chain([MAX_FRAME - 1, MAX_FRAME]) {
        let f = rng.bytes(len);
        let ev = receive(&encode_burst(std::slice::from_ref(&f), 8, 2));
        assert_eq!(frames_of(&ev), good(&[f]), "length {len}");
    }
}

#[test]
fn h02_extra_flags_between_frames_and_garbage_before_the_first_flag() {
    let mut rng = Rng::new(SEEDS[1]);
    for _ in 0..100 {
        let frames: Vec<Vec<u8>> = (0..4).map(|_| rng.frame(40)).collect();
        // garbage without a flag in it (a frame body has no six ones in a row), then flags
        // between the frames: 1 to 6
        let mut bits: Vec<bool> = ref_frame_bits(&rng.frame(30)).into_iter().skip(3).collect();
        // (a frame's tail alone, without its opening flag: the receiver is not in a frame)
        for f in &frames {
            bits.extend(flag_bits(1 + rng.below(6)));
            bits.extend(ref_frame_bits(f));
        }
        bits.extend(flag_bits(2));
        let got = frames_of(&receive(&bits));
        assert_eq!(got, good(&frames));
    }
}

#[test]
fn h02_the_stream_in_any_chunking_and_after_a_reset() {
    // the receiver keeps no notion of chunks; reset() forgets a half frame
    let frames = vec![
        vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8],
        vec![0xFF, 0xC8, 0x5F],
    ];
    let bits = encode_burst(&frames, 38, 2);
    let mut rx = HdlcRx::new();
    // half way through the first frame the carrier goes away
    for &b in &bits[..38 * 8 + 20] {
        rx.bit(b);
    }
    rx.reset();
    let ev: Vec<_> = bits.iter().filter_map(|&b| rx.bit(b)).collect();
    assert_eq!(frames_of(&ev), good(&frames));
}

// ------------------------------------------------------------------------------------- H03

#[test]
fn h03_a_wrong_fcs_is_reported_not_good_and_never_a_wrong_good_frame() {
    // flip every single bit of the frame region of a burst, one at a time: the FCS notices (the
    // receiver never reports a frame that was not sent as good). A flip can also make or break
    // a flag or an abort; those give other events, never a good frame with other data.
    let frames = vec![vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8, 0x80, 0xFF, 0x1F]];
    let clean = encode_burst(&frames, 3, 2);
    let mut counted = (0, 0);
    for i in 24..clean.len() - 16 {
        let mut bits = clean.clone();
        bits[i] = !bits[i];
        for (data, ok) in frames_of(&receive(&bits)) {
            if ok {
                panic!("bit {i} flipped: a good frame {data:02X?}");
            }
            counted.0 += 1;
        }
        counted.1 += 1;
    }
    assert!(counted.0 >= counted.1 / 2, "{counted:?}");
}

#[test]
fn h03_a_wrong_fcs_value_gives_fcs_ok_false_with_the_data() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..200 {
            let f = rng.frame(100);
            // the frame with a wrong FCS: send f + FCS ^ 0x0001..0xFFFF as if it were data
            let wrong = fcs16(&f) ^ (1 + rng.below(0xFFFF) as u16);
            let mut body = f.clone();
            body.extend([wrong as u8, (wrong >> 8) as u8]);
            // the burst of a "frame" that is `body` would add its own FCS; build the bits by
            // hand: flag, stuffed body, flag
            let mut bits = flag_bits(3);
            let mut run = 0;
            for b in octet_bits(&body) {
                bits.push(b);
                run = if b { run + 1 } else { 0 };
                if run == 5 {
                    bits.push(false);
                    run = 0;
                }
            }
            bits.extend(flag_bits(2));
            assert_eq!(frames_of(&receive(&bits)), vec![(f, false)]);
        }
    }
}

#[test]
fn h03_abort_seven_ones_drops_the_frame_and_the_next_frame_is_fine() {
    let mut rng = Rng::new(SEEDS[2]);
    for _ in 0..100 {
        let f = rng.frame(30);
        let next = rng.frame(30);
        let body = ref_frame_bits(&f);
        // cut the frame somewhere in the middle (not within the last 3 bits) and send 7..15 ones
        let cut = 8 + rng.below(body.len() - 12);
        let mut bits = flag_bits(4);
        bits.extend(&body[..cut]);
        bits.extend(std::iter::repeat_n(true, 7 + rng.below(9)));
        // then idle (ones), flags, the next frame
        bits.extend(flag_bits(2));
        bits.extend(ref_frame_bits(&next));
        bits.extend(flag_bits(2));
        let ev = receive(&bits);
        assert!(ev.contains(&HdlcEvent::Abort), "{ev:?}");
        assert_eq!(frames_of(&ev), good(&[next]), "the aborted frame came out");
    }
}

#[test]
fn h03_abort_when_no_frame_is_open_is_not_reported_and_the_receiver_recovers() {
    let f = vec![0xFF, 0x13, 0x80];
    let mut bits = vec![true; 40];
    bits.extend(flag_bits(2));
    bits.extend(ref_frame_bits(&f));
    bits.extend(flag_bits(1));
    let ev = receive(&bits);
    assert!(!ev.contains(&HdlcEvent::Abort));
    assert_eq!(frames_of(&ev), good(&[f]));
}

#[test]
fn h03_short_frames_are_never_good() {
    // an octet stream of 0, 1 and 2 octets between flags (shorter than an FCS plus one octet)
    for n in 0..=2usize {
        for v in [0x00u8, 0xFF, 0xA5, 0x13] {
            let mut bits = flag_bits(3);
            let mut run = 0;
            for b in octet_bits(&vec![v; n]) {
                bits.push(b);
                run = if b { run + 1 } else { 0 };
                if run == 5 {
                    bits.push(false);
                    run = 0;
                }
            }
            bits.extend(flag_bits(2));
            let got = frames_of(&receive(&bits));
            assert!(
                got.iter().all(|(_, ok)| !ok),
                "{n} octets of {v:#x}: {got:?}"
            );
        }
    }
    // a frame of an FCS alone (no data) is not good either
    let got = frames_of(&receive(&encode_burst(&[vec![]], 3, 2)));
    assert!(got.iter().all(|(_, ok)| !ok), "{got:?}");
    // while one octet of data is (the shortest good frame)
    let got = frames_of(&receive(&encode_burst(&[vec![0x42]], 3, 2)));
    assert_eq!(got, vec![(vec![0x42], true)]);
}

#[test]
fn h03_a_frame_that_is_not_a_whole_number_of_octets_is_never_good() {
    let mut rng = Rng::new(SEEDS[0]);
    for extra in 1..8usize {
        for _ in 0..30 {
            let f = rng.frame(20);
            let mut raw = octet_bits(&f);
            raw.extend(ref_fcs_bits(&f));
            // `extra` more bits (zeros and ones) before the closing flag
            for k in 0..extra {
                raw.push(k % 2 == 0);
            }
            let mut bits = flag_bits(3);
            let mut run = 0;
            for b in raw {
                bits.push(b);
                run = if b { run + 1 } else { 0 };
                if run == 5 {
                    bits.push(false);
                    run = 0;
                }
            }
            bits.extend(flag_bits(2));
            let got = frames_of(&receive(&bits));
            assert!(got.iter().all(|(_, ok)| !ok), "{extra} extra bits: {got:?}");
        }
    }
}

#[test]
fn h03_overlong_frames_are_dropped_to_bad_and_bounded() {
    let mut rng = Rng::new(SEEDS[1]);
    // MAX_FRAME is good, one more is not
    let f = rng.bytes(MAX_FRAME);
    assert_eq!(
        frames_of(&receive(&encode_burst(std::slice::from_ref(&f), 3, 2))),
        good(&[f])
    );
    for len in [MAX_FRAME + 1, MAX_FRAME + 2, 400, 1000, 70_000] {
        let f = rng.bytes(len);
        let got = frames_of(&receive(&encode_burst(&[f, vec![0xFF, 0x03, 0x42]], 3, 2)));
        assert_eq!(got.len(), 2, "length {len}: {got:?}");
        assert!(!got[0].1, "length {len} reported good");
        assert!(
            got[0].0.len() <= MAX_FRAME,
            "length {len}: {} octets kept",
            got[0].0.len()
        );
        // and the receiver is fine afterwards
        assert_eq!(got[1], (vec![0xFF, 0x03, 0x42], true), "length {len}");
    }
}

#[test]
fn h03_noise_before_a_burst_does_not_spoil_it() {
    // random bits (no knowledge of flags in them), then a clean burst: the frames of the burst
    // all come out good (the noise may produce bad frames of its own)
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for _ in 0..50 {
            let noise: Vec<bool> = (0..rng.below(3000)).map(|_| rng.next() & 1 != 0).collect();
            let frames: Vec<Vec<u8>> = (0..3).map(|_| rng.frame(30)).collect();
            let mut bits = noise;
            bits.extend(encode_burst(&frames, 38, 2));
            let got = frames_of(&receive(&bits));
            let tail: Vec<_> = got[got.len() - 3..].to_vec();
            assert_eq!(tail, good(&frames));
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

/// Bit streams: arbitrary bits, runs, flags and frames cut and spliced.
fn bit_stream() -> BoxedStrategy<Vec<bool>> {
    let flags = (1usize..12).prop_map(flag_bits);
    let ones = (1usize..40).prop_map(|n| vec![true; n]);
    let zeros = (1usize..40).prop_map(|n| vec![false; n]);
    let junk = proptest::collection::vec(any::<bool>(), 0..80);
    let frame = proptest::collection::vec(any::<u8>(), 0..40).prop_map(|d| ref_frame_bits(&d));
    let long = (any::<u8>(), 200usize..400).prop_map(|(v, n)| ref_frame_bits(&vec![v; n]));
    proptest::collection::vec(prop_oneof![flags, ones, zeros, junk, frame, long], 0..60)
        .prop_map(|parts| parts.concat())
        .boxed()
}

fn check_events(ev: &[HdlcEvent]) {
    for e in ev {
        if let HdlcEvent::Frame { data, fcs_ok } = e {
            assert!(data.len() <= MAX_FRAME, "{} octets", data.len());
            if *fcs_ok {
                assert!(!data.is_empty());
                // what is called good is good: it decodes again from a fresh burst
                let again = frames_of(&receive(&encode_burst(std::slice::from_ref(data), 3, 2)));
                assert_eq!(again, vec![(data.clone(), true)]);
            }
        }
    }
}

#[test]
fn h03_proptest_any_bit_stream_gives_no_panic_and_bounded_events() {
    for seed in 1..=3u8 {
        let mut runner = runner(seed, 300);
        runner
            .run(&bit_stream(), |bits| {
                let ev = receive(&bits);
                check_events(&ev);
                // at most one event per two bits, in any case
                prop_assert!(ev.len() <= bits.len() / 2 + 1);
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn h03_proptest_purely_arbitrary_bits() {
    for seed in 1..=3u8 {
        let mut runner = runner(seed, 300);
        runner
            .run(
                &proptest::collection::vec(any::<bool>(), 0..20_000),
                |bits| {
                    check_events(&receive(&bits));
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn h03_proptest_frames_survive_the_round_trip_in_any_company() {
    for seed in 1..=3u8 {
        let mut runner = runner(seed, 200);
        let strategy = (
            proptest::collection::vec(proptest::collection::vec(any::<u8>(), 1..=MAX_FRAME), 1..5),
            0usize..60,
            1usize..5,
        );
        runner
            .run(&strategy, |(frames, pre, post)| {
                let ev = receive(&encode_burst(&frames, pre, post));
                prop_assert_eq!(frames_of(&ev), good(&frames));
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn h03_proptest_damaged_bursts_never_give_a_wrong_good_frame_in_one_flip() {
    for seed in 1..=3u8 {
        let mut runner = runner(seed, 300);
        let strategy = (
            proptest::collection::vec(any::<u8>(), 1..60),
            any::<proptest::sample::Index>(),
        );
        runner
            .run(&strategy, |(frame, idx)| {
                let clean = encode_burst(std::slice::from_ref(&frame), 3, 3);
                // inside the frame: after the 3 flags and before the 3 closing flags
                let i = 24 + idx.index(clean.len() - 48);
                let mut bits = clean;
                bits[i] = !bits[i];
                for (data, ok) in frames_of(&receive(&bits)) {
                    prop_assert!(!ok, "a good frame {:02X?} after flipping bit {}", data, i);
                }
                Ok(())
            })
            .unwrap();
    }
}
