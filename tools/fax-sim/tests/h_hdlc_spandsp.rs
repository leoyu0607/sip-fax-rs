//! M5b tests H01 and H02 (test plan `docs/test-plans/M5.md` section 5): our HDLC framing
//! (`fax_modem::hdlc`) against SpanDSP's `hdlc_tx` / `hdlc_rx`, the independent implementation.
//!
//! * H01: our `encode_burst` produces the same bit stream as SpanDSP's `hdlc_tx` (flags, bit
//!   stuffing, FCS-16), over every 2-octet frame (all stuffing boundaries), crafted frames and
//!   random T.30-like frames in three seeds.
//! * H02: our `HdlcRx` decodes SpanDSP's bit streams (1 s of flags, several frames) to the same
//!   frames, `fcs_ok` right; SpanDSP's `hdlc_rx` decodes ours the same way.

mod m5b_common;

use fax_modem::hdlc::{FLAG, HdlcEvent, HdlcRx, encode_burst};
use m5b_common::{Rng, spandsp_hdlc_bits, spandsp_hdlc_rx, t30_like_frame};

const SEEDS: [u64; 3] = [0x5B01_0001, 0x5B01_0002, 0x5B01_0003];

/// 1 s of flags at 300 bit/s: 300 bits = 37.5 octets (T.30 5.3.1: 1 s +- 15 %). The engine sends
/// 38.
const PREAMBLE: usize = 38;

fn flag_bits(n: usize) -> Vec<bool> {
    (0..n)
        .flat_map(|_| (0..8).map(|i| FLAG >> i & 1 != 0))
        .collect()
}

/// `ours` and `theirs` are the same stream, except that `theirs` may end with more flag bits
/// (the closing flags of `spandsp_hdlc_bits` end where SpanDSP asks for its next octet).
fn assert_same_bits(ours: &[bool], theirs: &[bool], what: &str) {
    let common = ours.len().min(theirs.len());
    if let Some(i) = (0..common).find(|&i| ours[i] != theirs[i]) {
        panic!(
            "{what}: first difference at bit {i} of {} (ours) / {} (SpanDSP)",
            ours.len(),
            theirs.len()
        );
    }
    assert!(
        theirs.len() >= ours.len(),
        "{what}: ours is {} bits, SpanDSP only {}",
        ours.len(),
        theirs.len()
    );
    let tail = &theirs[ours.len()..];
    assert!(
        tail == &flag_bits(1 + tail.len() / 8)[..tail.len()],
        "{what}: SpanDSP sends {} more bits that are not flags",
        tail.len()
    );
}

/// Frames that put the stuffing boundaries (five ones at the end or the start of an octet, an
/// octet that is a flag, runs across octets) into the data.
fn crafted_frames() -> Vec<Vec<u8>> {
    vec![
        vec![0xFF, 0x03, 0x1F],             // five ones at the end of the low bits
        vec![0xF0, 0x01],                   // four ones at the top, one at the bottom: five
        vec![0xE0, 0x03, 0xFF],             // three at the top then two at the bottom
        vec![0x7E],                         // the flag octet as data
        vec![0x7E, 0x7E, 0x7E],             // three of them
        vec![0xFF; 40],                     // ones everywhere
        vec![0x00; 10],                     // zeros
        vec![0x7F, 0xFE, 0xFF, 0x80, 0x01], // runs of 6 and 7 ones across octets
        vec![0xF8, 0x1F, 0xF8, 0x1F],       // exactly five, again and again
        vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8, 0x80, 0x80, 0x00],
        vec![0xFF, 0xC8, 0x5F],
        (0..=255u8).collect(),
        vec![0xAA; 255],
    ]
}

#[test]
fn h01_every_two_octet_frame_has_the_bits_spandsp_makes() {
    // all 65 536 two-octet frames in one burst: every combination of the end of one octet and the
    // start of the next, and of the FCS that follows.
    let frames: Vec<Vec<u8>> = (0..=0xFFFFu32)
        .map(|v| vec![(v >> 8) as u8, v as u8])
        .collect();
    let ours = encode_burst(&frames, PREAMBLE, 2);
    let theirs = spandsp_hdlc_bits(&frames, PREAMBLE as i32, 3);
    assert_same_bits(&ours, &theirs, "all two-octet frames");
}

#[test]
fn h01_crafted_frames_one_by_one_and_together() {
    let frames = crafted_frames();
    for (i, f) in frames.iter().enumerate() {
        let one = std::slice::from_ref(f);
        assert_same_bits(
            &encode_burst(one, PREAMBLE, 2),
            &spandsp_hdlc_bits(one, PREAMBLE as i32, 3),
            &format!("crafted frame {i} ({} octets)", f.len()),
        );
    }
    assert_same_bits(
        &encode_burst(&frames, PREAMBLE, 2),
        &spandsp_hdlc_bits(&frames, PREAMBLE as i32, 3),
        "all crafted frames in one burst",
    );
}

#[test]
fn h01_random_frames_in_three_seeds() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        // T.30-like frames
        let frames: Vec<Vec<u8>> = (0..500).map(|i| t30_like_frame(&mut rng, i)).collect();
        assert_same_bits(
            &encode_burst(&frames, PREAMBLE, 2),
            &spandsp_hdlc_bits(&frames, PREAMBLE as i32, 3),
            &format!("seed {seed:#x}: T.30-like frames"),
        );
        // frames of 1 to 260 random octets, with the preamble lengths of the plan and others
        let frames: Vec<Vec<u8>> = (0..300)
            .map(|_| {
                let n = 1 + rng.below(260);
                rng.bytes(n)
            })
            .collect();
        for pre in [1usize, 2, 38, 43] {
            assert_same_bits(
                &encode_burst(&frames, pre, 2),
                &spandsp_hdlc_bits(&frames, pre as i32, 3),
                &format!("seed {seed:#x}: random frames, {pre} preamble flags"),
            );
        }
    }
}

/// The frame events of `bits` through our receiver, with whether the preamble was reported first.
fn ours_receive(bits: &[bool]) -> (bool, Vec<(Vec<u8>, bool)>) {
    let mut rx = HdlcRx::new();
    let mut preamble_first = false;
    let mut frames = Vec::new();
    for &b in bits {
        match rx.bit(b) {
            Some(HdlcEvent::Preamble) if frames.is_empty() => preamble_first = true,
            Some(HdlcEvent::Frame { data, fcs_ok }) => frames.push((data, fcs_ok)),
            _ => {}
        }
    }
    (preamble_first, frames)
}

#[test]
fn h02_our_receiver_decodes_spandsp_bursts_with_a_one_second_preamble() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for burst in 0..40 {
            let n = 1 + rng.below(4);
            let frames: Vec<Vec<u8>> = (0..n)
                .map(|i| t30_like_frame(&mut rng, burst + i))
                .collect();
            let bits = spandsp_hdlc_bits(&frames, PREAMBLE as i32, 3);
            let (preamble, got) = ours_receive(&bits);
            assert!(preamble, "seed {seed:#x} burst {burst}: no Preamble event");
            let want: Vec<(Vec<u8>, bool)> = frames.iter().map(|f| (f.clone(), true)).collect();
            assert_eq!(got, want, "seed {seed:#x} burst {burst}");
        }
    }
}

#[test]
fn h02_one_long_stream_of_many_bursts_and_idle_flags() {
    // bursts back to back with 0 to 20 idle flags between them (SpanDSP's own closing flags), the
    // way a peer that keeps its carrier up sends them
    let mut rng = Rng::new(SEEDS[0]);
    let mut bits = Vec::new();
    let mut want = Vec::new();
    for burst in 0..100 {
        let frames: Vec<Vec<u8>> = (0..3)
            .map(|i| t30_like_frame(&mut rng, burst + i))
            .collect();
        // SpanDSP's own burst, cut where ours ends (a whole flag; H01 shows they are the same
        // up to there): its closing flags end mid-octet, which would join the next burst badly
        let closing = 1 + rng.below(20);
        let end = encode_burst(&frames, PREAMBLE, closing).len();
        let theirs = spandsp_hdlc_bits(&frames, PREAMBLE as i32, closing as i32 + 1);
        bits.extend_from_slice(&theirs[..end]);
        want.extend(frames.into_iter().map(|f| (f, true)));
    }
    let (_, got) = ours_receive(&bits);
    assert_eq!(got, want);
}

#[test]
fn h02_fcs_verdicts_agree_with_spandsp_on_damaged_streams() {
    // flip one bit somewhere in the frames of a SpanDSP burst: whatever our receiver says about
    // each frame (data and `fcs_ok`), SpanDSP's receiver, run on the same damaged bits, says the
    // same about the frames both of them see. Frames only one side sees (a flipped bit can make
    // or break a flag) are not compared, but must not be reported as good by us if they are not
    // the frames that were sent.
    let mut rng = Rng::new(SEEDS[1]);
    let mut agreed = 0usize;
    let mut bad_seen = 0usize;
    for trial in 0..400 {
        let frames: Vec<Vec<u8>> = (0..3)
            .map(|i| t30_like_frame(&mut rng, trial + i))
            .collect();
        let mut bits = spandsp_hdlc_bits(&frames, 8, 2);
        let i = 64 + rng.below(bits.len() - 64 - 16);
        bits[i] = !bits[i];
        let (_, ours) = ours_receive(&bits);
        let theirs = spandsp_hdlc_rx(&bits);
        // what we call good is a frame that was sent, and SpanDSP does not call it bad
        for (d, ok) in &ours {
            if *ok {
                assert!(
                    frames.contains(d),
                    "trial {trial}: a good frame that was never sent: {d:02X?}"
                );
            } else {
                bad_seen += 1;
            }
        }
        let their_good: Vec<&Vec<u8>> = theirs.iter().filter(|t| t.1).map(|t| &t.0).collect();
        let our_good: Vec<&Vec<u8>> = ours.iter().filter(|t| t.1).map(|t| &t.0).collect();
        assert_eq!(
            our_good, their_good,
            "trial {trial}: the good frames differ (bit {i} flipped)"
        );
        agreed += 1;
    }
    assert_eq!(agreed, 400);
    assert!(
        bad_seen > 100,
        "only {bad_seen} bad frames: the damage test did nothing"
    );
    eprintln!("H02 damaged streams: 400 trials, {bad_seen} frames reported bad by us");
}

#[test]
fn h02_spandsp_decodes_our_bursts_too() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        let frames: Vec<Vec<u8>> = (0..200).map(|i| t30_like_frame(&mut rng, i)).collect();
        let theirs = spandsp_hdlc_rx(&encode_burst(&frames, PREAMBLE, 2));
        let want: Vec<(Vec<u8>, bool)> = frames.into_iter().map(|f| (f, true)).collect();
        assert_eq!(theirs, want, "seed {seed:#x}");
    }
}
