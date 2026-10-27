use super::*;
use fax_modem::hdlc::HdlcEvent;
use fax_modem::v27ter::testtx::Tx as V27Tx;
use fax_modem::v29::testtx::Tx as V29Tx;

/// Runs the engine for `ms` with `input` (silence when it runs out); returns what it sent.
fn run(e: &mut AudioEngine, input: &[i16], ms: u64) -> Vec<i16> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for _ in 0..ms / 20 {
        let mut rx = [0i16; 160];
        let n = input.len().saturating_sub(pos).min(160);
        rx[..n].copy_from_slice(&input[pos..pos + n]);
        pos += n;
        let mut tx = [0i16; 160];
        e.process(&rx, &mut tx);
        out.extend_from_slice(&tx);
    }
    out
}

/// The frames (T.30 notation) in a stretch of audio.
fn frames_in(audio: &[i16]) -> Vec<Vec<u8>> {
    let mut rx = V21Rx::new();
    let mut h = HdlcRx::new();
    rx.feed(audio)
        .into_iter()
        .filter_map(|b| match h.bit(b) {
            Some(HdlcEvent::Frame { data, fcs_ok: true }) => Some(reverse_octets(&data)),
            _ => None,
        })
        .collect()
}

#[test]
fn answers_with_ced_then_csi_and_dis_on_v21() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let out = run(&mut e, &[], 6_000);
    // 200 ms silence, then CED
    assert!(out[..1600].iter().all(|&s| s == 0));
    let ced = &out[1600..1600 + 24_000];
    let level = fax_modem::level::dbm0(fax_modem::level::block_mean_square(ced));
    assert!((level + 12.0).abs() < 0.2, "{level}");
    // then frames: CSI and DIS, the DIS with the final control field
    let frames = frames_in(&out[1600 + 24_000..]);
    let fcfs: Vec<u8> = frames.iter().map(|f| f[2]).collect();
    assert!(fcfs.contains(&t30::fcf::DIS), "{frames:02x?}");
    let dis = frames.iter().find(|f| f[2] == t30::fcf::DIS).unwrap();
    assert_eq!(dis[1], t30::frame::CONTROL_FINAL);
    assert_eq!(e.state(), t30::State::WaitCommand);
    // audio.log: our frame by name, with its octets (no station id: no CSI)
    let log = e.drain_log();
    assert!(
        log.iter().any(|l| l.starts_with("V.21 tx DIS FF C8 ")),
        "{log:#?}"
    );
}

#[test]
fn a_dcs_from_the_peer_is_heard_after_our_burst() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run(&mut e, &[], 6_000);
    // the peer: TSI and DCS (V.29 9600, standard), in T.30 notation, on V.21
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let dcs = vec![0xFF, 0xC8, t30::fcf::DCS, 0x00, 0x46, 0x00];
    let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
    let mut tx = V21Tx::new(-15.0);
    let mut peer = vec![0i16; 800];
    peer.extend(tx.modulate(&hdlc::encode_burst(&wire, 38, 2)));
    let _ = run(&mut e, &peer, 3_000);
    // FIF 00 46 00 is V.17 14400: not received by this receiver
    let log = e.drain_log();
    assert!(
        log.iter()
            .any(|l| l.contains("DCS selects") && l.contains("not received")),
        "the DCS reached T.30"
    );
    // every V.21 frame is in the log (audio.log): ours sent, the peer's received
    for want in [
        "V.21 tx DIS FF C8 ",
        "V.21 rx TSI FF C0 ",
        "V.21 rx DCS FF C8 ",
    ] {
        assert!(
            log.iter().any(|l| l.starts_with(want)),
            "no {want:?} in {log:#?}"
        );
    }
    let dcs = log.iter().find(|l| l.starts_with("V.21 rx DCS")).unwrap();
    assert_eq!(dcs, "V.21 rx DCS FF C8 41 00 46 00", "all octets, in hex");
}

/// The DIS the engine sends advertises V.27ter and V.29, and not V.17 (ADR-061).
#[test]
fn the_dis_offers_v27ter_and_v29_but_not_v17() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let out = run(&mut e, &[], 6_000);
    let frames = frames_in(&out[1600 + 24_000..]);
    let dis = frames
        .iter()
        .find(|f| f[2] == t30::fcf::DIS)
        .unwrap_or_else(|| panic!("no DIS in {frames:02x?}"));
    let dis = t30::Dis::parse(&dis[3..]);
    assert!(
        dis.v27ter() && dis.v29() && !dis.v17() && !dis.v33(),
        "DIS modems: {dis:?}"
    );
}

/// The DCS information field for V.27ter 4800 bit/s: bit 12 set (T.30 Table 2).
fn dcs_fif_v27ter_4800() -> Vec<u8> {
    let fif = vec![0x00, 0x10, 0x00];
    assert_eq!(
        t30::caps::parse_rate(&fif).unwrap(),
        (t30::caps::Modem::V27ter, 4800)
    );
    fif
}

/// The DCS information field for V.29 at `rate` bit/s: bit 11 (9600), bits 11 and 12 (7200).
fn dcs_fif_v29(rate: u32) -> Vec<u8> {
    let fif = if rate == 9600 {
        vec![0x00, 0x20, 0x00]
    } else {
        vec![0x00, 0x30, 0x00]
    };
    assert_eq!(
        t30::caps::parse_rate(&fif).unwrap(),
        (t30::caps::Modem::V29, rate)
    );
    fif
}

/// A V.27ter 4800 sender of the TCF and the image.
fn v27ter_sender() -> impl Fn(&[bool]) -> Vec<i16> {
    |bits| {
        V27Tx {
            rate: v27ter::Rate::B4800,
            level_dbm0: -15.0,
            long: true,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        }
        .modulate(bits)
    }
}

/// A V.29 sender of the TCF and the image at `rate`.
fn v29_sender(rate: v29::Rate) -> impl Fn(&[bool]) -> Vec<i16> {
    move |bits| {
        V29Tx {
            rate,
            level_dbm0: -15.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        }
        .modulate(bits)
    }
}

/// `n` pseudo-random bits (xorshift64, a fixed seed): about half of them are ones.
fn random_bits(n: usize) -> Vec<bool> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x & 1 != 0
        })
        .collect()
}

/// The peer's frames as one V.21 burst, then 3 s of silence.
fn send_v21(e: &mut AudioEngine, frames: &[Vec<u8>]) {
    let wire: Vec<Vec<u8>> = frames.iter().map(|f| reverse_octets(f)).collect();
    let mut v21 = V21Tx::new(-15.0);
    let mut peer = vec![0i16; 800];
    peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
    let _ = run(e, &peer, 3_000);
}

#[test]
fn the_last_dcs_selects_the_receiver_from_its_own_fif() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run(&mut e, &[], 6_000);
    let dcs = |fif: &[u8]| {
        let mut f = vec![0xFF, 0xC8, t30::fcf::DCS];
        f.extend_from_slice(fif);
        f
    };
    let fif_2400 = vec![0x00, 0x00, 0x00];
    assert_eq!(
        t30::caps::parse_rate(&fif_2400).unwrap(),
        (t30::caps::Modem::V27ter, 2400)
    );
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    send_v21(&mut e, &[tsi, dcs(&dcs_fif_v27ter_4800())]);
    send_v21(&mut e, &[dcs(&fif_2400)]);
    let log = e.drain_log();
    let at = |s: &str| log.iter().position(|l| l == s);
    assert!(
        matches!((at("V.27ter 4800 receiver ready"), at("V.27ter 2400 receiver ready")),
                (Some(a), Some(b)) if a < b),
        "{log:#?}"
    );
}

#[test]
fn a_dcs_of_a_calling_terminal_carries_the_x_bit_and_still_selects_the_receiver() {
    // A calling terminal sets X (the first bit sent) in the FCF after it has seen a DIS:
    // `FF C8 C1 ...`. SpanDSP does; M5c test M01 found the receiver missing it.
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run(&mut e, &[], 6_000);
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI | 0x80, b' ', b' ', b'1'];
    let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS | 0x80];
    dcs.extend(dcs_fif_v27ter_4800());
    send_v21(&mut e, &[tsi, dcs]);
    let log = e.drain_log();
    assert!(
        log.iter().any(|l| l == "V.27ter 4800 receiver ready"),
        "{log:#?}"
    );
}

/// A call up to and through the TCF: the answer, the peer's TSI and a DCS with `fif`, then
/// `tcf` sent by `transmit` and silence. Returns the frames the engine sent after the TCF and
/// the log.
fn tcf_call(
    fif: &[u8],
    transmit: &dyn Fn(&[bool]) -> Vec<i16>,
    tcf: &[bool],
) -> (Vec<Vec<u8>>, Vec<String>) {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run(&mut e, &[], 5_000);
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
    dcs.extend_from_slice(fif);
    let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
    let mut v21 = V21Tx::new(-15.0);
    let mut peer = vec![0i16; 800];
    peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
    let _ = run(&mut e, &peer, 3_000);
    let mut log = e.drain_log();
    let input = transmit(tcf);
    // whole 20 ms blocks, and 4 s of silence after the signal for our answer
    let blocks = input.len() / 160 + 1 + 200;
    let out = run(&mut e, &input, blocks as u64 * 20);
    log.extend(e.drain_log());
    (frames_in(&out), log)
}

/// The TCF `tcf` on V.27ter 4800 bit/s: its FCFs and the log.
fn v27ter_tcf(tcf: &[bool]) -> (Vec<u8>, Vec<String>) {
    let (frames, log) = tcf_call(&dcs_fif_v27ter_4800(), &v27ter_sender(), tcf);
    (frames.iter().map(|f| f[2]).collect(), log)
}

/// The TCF `tcf` on V.29 at `rate` bit/s: its FCFs and the log.
fn v29_tcf(rate: u32, tcf: &[bool]) -> (Vec<u8>, Vec<String>) {
    let modem = if rate == 9600 {
        v29::Rate::B9600
    } else {
        v29::Rate::B7200
    };
    let (frames, log) = tcf_call(&dcs_fif_v29(rate), &v29_sender(modem), tcf);
    (frames.iter().map(|f| f[2]).collect(), log)
}

#[test]
fn a_v27ter_tcf_of_zeros_is_answered_with_cfr() {
    let tcf = vec![false; 7_200]; // 1.5 s at 4800 bit/s
    let (fcfs, log) = v27ter_tcf(&tcf);
    let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
    assert!(
        log.iter().any(|l| l == "V.27ter 4800 receiver ready"),
        "{log:#?}"
    );
    assert!(
        fcfs.contains(&t30::fcf::CFR),
        "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
    );
}

#[test]
fn a_v27ter_tcf_of_random_bits_is_answered_with_ftt() {
    let (fcfs, log) = v27ter_tcf(&random_bits(7_200));
    let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
    assert!(
        fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
        "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
    );
}

#[test]
fn a_v29_9600_tcf_of_zeros_is_answered_with_cfr() {
    let tcf = vec![false; 14_400]; // 1.5 s at 9600 bit/s
    let (fcfs, log) = v29_tcf(9_600, &tcf);
    let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
    assert!(
        log.iter().any(|l| l == "V.29 9600 receiver ready"),
        "{log:#?}"
    );
    assert!(
        fcfs.contains(&t30::fcf::CFR),
        "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
    );
}

#[test]
fn a_v29_9600_tcf_of_random_bits_is_answered_with_ftt() {
    let (fcfs, log) = v29_tcf(9_600, &random_bits(14_400));
    let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
    assert!(
        fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
        "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
    );
}

#[test]
fn a_v29_7200_tcf_of_zeros_is_answered_with_cfr() {
    let tcf = vec![false; 10_800]; // 1.5 s at 7200 bit/s
    let (fcfs, log) = v29_tcf(7_200, &tcf);
    let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
    assert!(
        log.iter().any(|l| l == "V.29 7200 receiver ready"),
        "{log:#?}"
    );
    assert!(
        fcfs.contains(&t30::fcf::CFR),
        "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
    );
}

/// M5e ⑤: the trunk may offer ECM, the audio path does not.
#[test]
fn the_dis_never_offers_ecm_or_mmr() {
    let mut cfg = AudioConfig::default();
    cfg.t30.ecm = true;
    cfg.t30.mmr = true;
    let mut e = AudioEngine::new(cfg);
    e.start();
    let out = run(&mut e, &[], 6_000);
    let frames = frames_in(&out[1600 + 24_000..]);
    let dis = frames
        .iter()
        .find(|f| f[2] == t30::fcf::DIS)
        .unwrap_or_else(|| panic!("no DIS in {frames:02x?}"));
    let dis = t30::Dis::parse(&dis[3..]);
    assert!(!dis.ecm && !dis.mmr, "DIS: {dis:?}");
}

/// M5e ⑥: a DCS for V.17 (not offered) and its TCF: FTT right after the TCF, not after T2.
#[test]
fn a_tcf_on_a_modem_without_a_receiver_is_answered_with_ftt_at_once() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run(&mut e, &[], 6_000);
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    // V.17 14400
    let fif = vec![0x00, 0x46, 0x00];
    assert_eq!(
        t30::caps::parse_rate(&fif).unwrap(),
        (t30::caps::Modem::V17, 14400)
    );
    let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
    dcs.extend(fif);
    let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
    let mut v21 = V21Tx::new(-15.0);
    let mut peer = vec![0i16; 800];
    peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
    // 75 ms, then 1.5 s of "V.17" (any signal: there is no receiver for it)
    peer.extend(vec![0i16; 600]);
    let tcf_end = peer.len();
    peer.extend(
        V29Tx {
            rate: v29::Rate::B9600,
            level_dbm0: -15.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        }
        .modulate(&vec![false; 14_400]),
    );
    let tcf_end = tcf_end + (peer.len() - tcf_end);
    let out = run(&mut e, &peer, (tcf_end as u64 / 8) + 3_000);
    let log = e.drain_log();
    // the FTT starts (preamble) within 1 s of the end of the TCF (T2 is 6 s)
    let window = &out[tcf_end..tcf_end + 8 * 2_500];
    let fcfs: Vec<u8> = frames_in(window).iter().map(|f| f[2]).collect();
    assert!(fcfs.contains(&t30::fcf::FTT), "{fcfs:02x?} {log:#?}");
    let first_sound = window.iter().position(|&s| s != 0).unwrap_or(usize::MAX);
    assert!(
        first_sound < 8 * 1_000,
        "FTT {} ms after the TCF",
        first_sound / 8
    );
}

/// M5e ⑦: the scrambled ones of V.29 Segment 4 are not counted in the TCF.
#[test]
fn a_v29_tcf_starts_after_segment_4() {
    let (_, log) = v29_tcf(9_600, &vec![false; 14_400]);
    let line = log
        .iter()
        .find(|l| l.starts_with("TCF:"))
        .unwrap_or_else(|| panic!("{log:#?}"));
    // "TCF: n octets (need m), z of t bits zero": the ones left are the end of the
    // carrier (the test transmitter's tail of 8 symbols and the 10 ms it takes to hear the
    // end, about 72 bits); Segment 4 alone would add 192
    let nums: Vec<u64> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|w| w.parse().ok())
        .collect();
    let (zero, total) = (nums[2], nums[3]);
    assert!(total - zero <= 100, "{line}");
}

/// M5e ④: a 20 ms packet lost in the middle of a TCF of zeros: the receiver keeps going and
/// the TCF is still good (before, the gap ended the carrier and the TCF was too short).
#[test]
fn a_lost_packet_in_the_middle_of_a_tcf_does_not_end_it() {
    for rate in [9_600u32, 7_200] {
        let modem = if rate == 9600 {
            v29::Rate::B9600
        } else {
            v29::Rate::B7200
        };
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
        let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
        dcs.extend(dcs_fif_v29(rate));
        send_v21(&mut e, &[tsi, dcs]);
        let input = v29_sender(modem)(&vec![false; rate as usize * 3 / 2]);
        let mut out = Vec::new();
        let blocks = input.len() / 160 + 1 + 200;
        // a block about 0.9 s into the TCF is lost
        let lost = (8 * 900 + 400 + 8 * 300) / 160;
        for b in 0..blocks {
            let mut rx = [0i16; 160];
            let from = (b * 160).min(input.len());
            let n = input.len().saturating_sub(from).min(160);
            rx[..n].copy_from_slice(&input[from..from + n]);
            let mut tx = [0i16; 160];
            if b == lost {
                e.process_lost(160, &mut tx);
            } else {
                e.process(&rx, &mut tx);
            }
            out.extend_from_slice(&tx);
        }
        let fcfs: Vec<u8> = frames_in(&out).iter().map(|f| f[2]).collect();
        let log = e.drain_log();
        assert!(
            fcfs.contains(&t30::fcf::CFR),
            "{rate}: {fcfs:02x?} {:?}",
            log.iter().find(|l| l.starts_with("TCF:"))
        );
    }
}

/// M5e ④: an outage longer than `max_lost_skip_ms` is silence, so the signal ends.
#[test]
fn a_long_outage_counts_as_silence() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run(&mut e, &[], 6_000);
    let mut v21 = V21Tx::new(-15.0);
    let flags = v21.modulate(&hdlc::encode_burst(&[], 38, 0));
    let _ = run(
        &mut e,
        &flags[..flags.len() / 160 * 160],
        flags.len() as u64 / 8,
    );
    assert!(e.peer_on, "the flags are heard");
    let mut tx = [0i16; 160];
    for _ in 0..3 {
        e.process_lost(160, &mut tx);
    }
    assert!(e.peer_on, "60 ms lost: skipped");
    for _ in 0..2 {
        e.process_lost(160, &mut tx);
    }
    assert!(!e.peer_on, "100 ms lost: silence, the signal has ended");
}

#[test]
fn a_v29_7200_tcf_of_random_bits_is_answered_with_ftt() {
    let (fcfs, log) = v29_tcf(7_200, &random_bits(10_800));
    let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
    assert!(
        fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
        "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
    );
}
