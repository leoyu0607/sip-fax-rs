//! Engine-level tests of `fax_in::t38engine` on a virtual clock (case ids R11..R19, continuing
//! the T.30 series of docs/test-plans/M2.md).
//!
//! The fax machine on the other end is a small scripted caller living in this file. It shares no
//! code with the engine: frames are written as hex, IFP packets are built by hand and the
//! engine's output is decoded again with its own tiny reassembler.

use fax_in::t38engine::{EngineConfig, Pacing, T38Engine, UdptlEngine};
use t30::State;
use t38::{DataType, ErrorRecovery, Field, FieldType, Ifp, Indicator, ReceiverConfig, T38Version};

// ---------------------------------------------------------------- the scripted caller

/// What the engine sent, reassembled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Heard {
    Ind(Indicator),
    Frame(Vec<u8>),
    SigEnd,
}

struct Bench {
    eng: T38Engine,
    now: u64,
    /// IFPs the caller will deliver: (due ms, packet), kept sorted by due time.
    inbox: Vec<(u64, Ifp)>,
    /// Everything the engine emitted, with the instant.
    raw: Vec<(u64, Ifp)>,
    heard: Vec<(u64, Heard)>,
    hdlc: Vec<u8>,
}

impl Bench {
    fn new(cfg: EngineConfig) -> Self {
        Self {
            eng: T38Engine::new(cfg),
            now: 0,
            inbox: Vec::new(),
            raw: Vec::new(),
            heard: Vec::new(),
            hdlc: Vec::new(),
        }
    }

    fn schedule(&mut self, at: u64, ifp: Ifp) {
        self.inbox.push((at, ifp));
        self.inbox.sort_by_key(|(t, _)| *t);
    }

    fn step(&mut self) {
        // deliver caller packets due now
        while self.inbox.first().is_some_and(|(t, _)| *t <= self.now) {
            let (_, ifp) = self.inbox.remove(0);
            self.eng.on_ifp(self.now, &ifp);
        }
        self.eng.advance(self.now);
        for t in self.eng.poll_timed() {
            self.raw.push((t.at_ms, t.ifp.clone()));
            self.reassemble(t.at_ms, &t.ifp);
        }
    }

    fn run_until(&mut self, end: u64) {
        while self.now <= end {
            self.step();
            self.now += 1;
        }
    }

    /// Steps until the engine has sent a frame with this FCF as the newest one, and stops at
    /// that instant (so that the caller can answer while the engine's burst tail runs).
    fn run_until_fcf(&mut self, fcf: u8, limit: u64) {
        let before = self.frames().len();
        while self.now <= limit
            && !(self.frames().len() > before && self.frame_fcfs().last() == Some(&fcf))
        {
            self.step();
            self.now += 1;
        }
        assert_eq!(
            self.frame_fcfs().last(),
            Some(&fcf),
            "frame {fcf:#04x} not heard by {limit} ms: {:#?}\n{:?}",
            self.eng.drain_log(),
            self.heard
                .iter()
                .filter(|(_, h)| !matches!(h, Heard::Ind(_)))
                .collect::<Vec<_>>()
        );
    }

    fn run_until_done(&mut self, limit: u64) {
        while self.now <= limit && !self.eng.is_done() {
            self.step();
            self.now += 1;
        }
        assert!(
            self.eng.is_done(),
            "engine not done by {limit} ms: state {:?}",
            self.eng.state()
        );
    }

    fn reassemble(&mut self, at: u64, ifp: &Ifp) {
        match ifp {
            Ifp::Indicator(i) => self.heard.push((at, Heard::Ind(*i))),
            Ifp::Data { fields, .. } => {
                for f in fields {
                    match f.field_type {
                        FieldType::HdlcData => self.hdlc.extend(&f.data),
                        FieldType::HdlcFcsOk => {
                            let fr = std::mem::take(&mut self.hdlc);
                            self.heard.push((at, Heard::Frame(fr)));
                        }
                        FieldType::HdlcFcsOkSigEnd => {
                            if !self.hdlc.is_empty() {
                                let fr = std::mem::take(&mut self.hdlc);
                                self.heard.push((at, Heard::Frame(fr)));
                                self.heard.push((at, Heard::SigEnd));
                            }
                        }
                        other => panic!("unexpected field {other:?}"),
                    }
                }
            }
        }
    }

    fn frames(&self) -> Vec<(u64, Vec<u8>)> {
        self.heard
            .iter()
            .filter_map(|(t, h)| match h {
                Heard::Frame(f) => Some((*t, f.clone())),
                _ => None,
            })
            .collect()
    }

    fn frame_fcfs(&self) -> Vec<u8> {
        self.frames().iter().map(|(_, f)| f[2]).collect()
    }

    // ---- caller side packet builders (each returns the time after the burst)

    /// A V.21 burst: preamble x3, one `hdlc-data` per frame, `fcs-OK`, final `fcs-OK-sig-end` x3.
    fn send_burst(&mut self, mut t: u64, frames: &[&[u8]]) -> u64 {
        for _ in 0..3 {
            self.schedule(t, Ifp::Indicator(Indicator::V21Preamble));
        }
        t += 1000;
        for (i, f) in frames.iter().enumerate() {
            self.schedule(t, data(DataType::V21, FieldType::HdlcData, f));
            if i + 1 < frames.len() {
                self.schedule(t, data(DataType::V21, FieldType::HdlcFcsOk, &[]));
            } else {
                for _ in 0..3 {
                    self.schedule(t, data(DataType::V21, FieldType::HdlcFcsOkSigEnd, &[]));
                }
            }
            t += 100;
        }
        for _ in 0..3 {
            self.schedule(t, Ifp::Indicator(Indicator::NoSignal));
        }
        t + 75
    }

    /// Image-modem signal: training indicator x3, data in `chunk` octet packets every 30 ms,
    /// `sig-end` x3, then `no-signal`.
    fn send_image(
        &mut self,
        mut t: u64,
        ind: Indicator,
        dt: DataType,
        data_bytes: &[u8],
        chunk: usize,
    ) -> u64 {
        for _ in 0..3 {
            self.schedule(t, Ifp::Indicator(ind));
        }
        t += 200;
        // The data goes out once; the signal end follows as empty copies (as SpanDSP and the
        // gateways do). A copy that repeated the data would be new data to any receiver.
        for c in data_bytes.chunks(chunk) {
            self.schedule(t, data(dt, FieldType::T4NonEcmData, c));
            t += 30;
        }
        for _ in 0..3 {
            self.schedule(t, data(dt, FieldType::T4NonEcmSigEnd, &[]));
        }
        for _ in 0..3 {
            self.schedule(t, Ifp::Indicator(Indicator::NoSignal));
        }
        t + 75
    }
}

fn data(dt: DataType, ft: FieldType, bytes: &[u8]) -> Ifp {
    Ifp::Data {
        data_type: dt,
        fields: vec![Field::new(ft, bytes.to_vec())],
    }
}

fn hexs(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|x| u8::from_str_radix(x, 16).unwrap())
        .collect()
}

fn cfg_default() -> EngineConfig {
    EngineConfig::default()
}

fn cfg_id() -> EngineConfig {
    let mut c = EngineConfig::default();
    c.t30.local_id = "0287654321".into();
    c
}

const DCS_STD_MH_14400: &str = "FF C8 C1 00 44 1E"; // receive (10), V.17 14400 (14), standard, MH, 0 ms
const EOP: &str = "FF C8 F4";
const MPS: &str = "FF C8 F2";
const DCN: &str = "FF C8 DF";

fn test_bitmap(rows: usize, seed: u32) -> Vec<u8> {
    let mut v = vec![0u8; 216 * rows];
    for y in 0..rows {
        for x in 0..216 {
            let k = (y as u32 * 31 + x as u32 * 7 + seed) % 11;
            v[y * 216 + x] = match k {
                0 => 0xFF,
                1 | 2 => 0xF0,
                3 => 0x0F,
                _ => 0x00,
            };
        }
    }
    v
}

fn mh(bm: &[u8], rows: usize) -> Vec<u8> {
    let mut e = t4::Encoder::new(1728, t4::Coding::Mh).unwrap();
    for y in 0..rows {
        e.push_row(&bm[y * 216..(y + 1) * 216]).unwrap();
    }
    e.finish()
}

fn decode_g4(page: &fax_in::t38engine::ReceivedPage) -> Vec<u8> {
    let p = t4::decode_page(
        &page.g4,
        t4::DecoderConfig::new(page.width, t4::Coding::Mmr),
    )
    .unwrap();
    assert_eq!(p.summary.stats.rows, page.height);
    assert_eq!(p.summary.stats.bad_lines, 0);
    p.bitmap
}

// ---------------------------------------------------------------- R11 pacing

#[test]
fn r11_first_burst_instants_are_exact() {
    let mut b = Bench::new(cfg_default());
    b.eng.start(0);
    b.run_until(5_000);
    // (ms, kind) of everything up to the end of the burst.
    let ind: Vec<(u64, Indicator)> = b
        .raw
        .iter()
        .filter_map(|(t, i)| match i {
            Ifp::Indicator(x) => Some((*t, *x)),
            _ => None,
        })
        .collect();
    // no-signal x3 at 0, CED x3 after 200 ms of silence, then (CED held 3 s => 3200) no-signal
    // x3, 75 ms later v21-preamble x3, and no-signal x3 once the burst is out.
    let mut want = vec![];
    want.extend([(0, Indicator::NoSignal); 3]);
    want.extend([(200, Indicator::Ced); 3]);
    want.extend([(3200, Indicator::NoSignal); 3]);
    want.extend([(3275, Indicator::V21Preamble); 3]);
    // DIS = FF C8 01 00 77 1E: after the 1 s preamble the first octet leaves at 4275 ms and one
    // octet follows every 8 bit / 300 bit/s = 26 666 us; the 3 sig-end copies leave with the
    // last octet's air time elapsed (4275000 + 6 * 26666 = 4434996 us); the burst ends after the
    // frame tail (2 stuffed + 35 bit = 123 333 us) and 100 ms of hold: 4658329 us.
    want.extend([(4658, Indicator::NoSignal); 3]);
    assert_eq!(ind, want);

    let octet_times: Vec<u64> = b
        .raw
        .iter()
        .filter_map(|(t, i)| match i {
            Ifp::Data { fields, .. } if fields[0].field_type == FieldType::HdlcData => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(octet_times, [4275, 4301, 4328, 4354, 4381, 4408]);
    let sig_end: Vec<u64> = b
        .raw
        .iter()
        .filter_map(|(t, i)| match i {
            Ifp::Data { fields, .. } if fields[0].field_type == FieldType::HdlcFcsOkSigEnd => {
                Some(*t)
            }
            _ => None,
        })
        .collect();
    assert_eq!(sig_end, [4434, 4434, 4434]);
    assert_eq!(b.frames(), vec![(4434, hexs("FF C8 01 00 77 1E"))]);
}

#[test]
fn r11_dis_is_retransmitted_t4_after_the_burst_ended() {
    let mut b = Bench::new(cfg_default());
    b.eng.start(0);
    // burst ends 4658.329 ms, so T4 (3 s) fires at 7658.329 ms: the second burst starts then,
    // 75 ms of silence later the preamble and 1 s later the first octet.
    b.run_until(7_657);
    assert_eq!(b.frames().len(), 1);
    b.run_until(7_658);
    // the no-signal x3 that opens the retransmission is not sent (already silent), only the
    // 75 ms of silence; the preamble goes at 7658.329 + 75 = 7733.329
    let pre: Vec<u64> = b
        .raw
        .iter()
        .filter(|(_, i)| *i == Ifp::Indicator(Indicator::V21Preamble))
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(pre, [3275, 3275, 3275]);
    b.run_until(7_733);
    let pre: Vec<u64> = b
        .raw
        .iter()
        .filter(|(_, i)| *i == Ifp::Indicator(Indicator::V21Preamble))
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(pre, [3275, 3275, 3275, 7733, 7733, 7733]);
}

#[test]
fn r11_three_retransmissions_then_dcn_and_failure_at_exact_instants() {
    let mut b = Bench::new(cfg_default());
    b.eng.start(0);
    b.run_until_done(60_000);
    let r = b.eng.result().unwrap();
    assert_eq!(r.code, t30::ResultCode::NoResponseToDis);
    // 4 DIS bursts and a DCN
    assert_eq!(b.frame_fcfs(), [0x01, 0x01, 0x01, 0x01, 0x5F]);
    // burst 1 ends 4658329; every further burst is 75 + 1000 + 6 * 26666 + 123333 + 100000 =
    // 1 458 329 us long and starts 3 s after the previous end.
    // ends: 4658329, 9116658, 13574987, 18033316; the DCN burst (3 octets: FF C8 5F, one stuffed
    // bit pair...) follows after T4 at 21033316.
    let dis_starts: Vec<u64> = b
        .raw
        .iter()
        .filter(|(_, i)| *i == Ifp::Indicator(Indicator::V21Preamble))
        .map(|(t, _)| *t)
        .step_by(3)
        .collect();
    assert_eq!(dis_starts, [3275, 7733, 12191, 16649, 21108]);
    assert_eq!(r.duration_ms, b.now.min(r.duration_ms));
}

#[test]
fn r11_without_pacing_everything_goes_out_in_the_start_instant() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(1_000);
    b.now = 1_000;
    b.run_until(1_000);
    assert!(b.raw.iter().all(|(t, _)| *t == 1_000), "{:?}", b.raw);
    // one whole-frame hdlc-data field, not octet by octet
    let fields: Vec<usize> = b
        .raw
        .iter()
        .filter_map(|(_, i)| match i {
            Ifp::Data { fields, .. } if fields[0].field_type == FieldType::HdlcData => {
                Some(fields[0].data.len())
            }
            _ => None,
        })
        .collect();
    assert_eq!(fields, [6]);
    // T4 still runs on the virtual clock: next DIS exactly 3 s later
    b.run_until(4_000);
    assert_eq!(b.frames().len(), 2);
    assert_eq!(b.frames()[1].0, 4_000);
}

// ---------------------------------------------------------------- R12 repetition

#[test]
fn r12_repetition_counts_are_configurable() {
    for (ind, end) in [(1u8, 1u8), (2, 3), (3, 3), (5, 2)] {
        let mut cfg = cfg_id();
        cfg.pacing = Pacing::none();
        cfg.indicator_repeat = ind;
        cfg.data_end_repeat = end;
        let mut b = Bench::new(cfg);
        b.eng.start(0);
        b.run_until(0);
        let count = |want: &Ifp| b.raw.iter().filter(|(_, i)| i == want).count();
        assert_eq!(
            count(&Ifp::Indicator(Indicator::NoSignal)),
            3 * usize::from(ind),
            "no-signal (before CED, before the burst, after it) x{ind}"
        );
        assert_eq!(count(&Ifp::Indicator(Indicator::Ced)), usize::from(ind));
        assert_eq!(
            count(&Ifp::Indicator(Indicator::V21Preamble)),
            usize::from(ind)
        );
        let sig_end = b
            .raw
            .iter()
            .filter(|(_, i)| matches!(i, Ifp::Data { fields, .. } if fields[0].field_type == FieldType::HdlcFcsOkSigEnd))
            .count();
        assert_eq!(sig_end, usize::from(end));
        let fcs_ok = b
            .raw
            .iter()
            .filter(|(_, i)| matches!(i, Ifp::Data { fields, .. } if fields[0].field_type == FieldType::HdlcFcsOk))
            .count();
        assert_eq!(
            fcs_ok, 1,
            "the CSI frame ends with a plain hdlc-fcs-OK, once"
        );
    }
}

#[test]
fn r12_zero_repeat_is_treated_as_one() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    cfg.indicator_repeat = 0;
    cfg.data_end_repeat = 0;
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    let ced = b
        .raw
        .iter()
        .filter(|(_, i)| *i == Ifp::Indicator(Indicator::Ced))
        .count();
    assert_eq!(ced, 1);
}

// ---------------------------------------------------------------- R13 HDLC framing

#[test]
fn r13_frames_are_carried_as_hdlc_data_then_fcs_ok_and_the_last_as_sig_end() {
    let mut b = Bench::new(cfg_id());
    b.eng.start(0);
    b.run_until(7_000);
    // Walk the first burst and check the field sequence per frame.
    let mut seq: Vec<(FieldType, usize)> = Vec::new();
    for (_, i) in &b.raw {
        if let Ifp::Data { data_type, fields } = i {
            assert_eq!(*data_type, DataType::V21);
            assert_eq!(fields.len(), 1, "one field per packet");
            seq.push((fields[0].field_type, fields[0].data.len()));
        }
    }
    let mut want = vec![];
    want.extend([(FieldType::HdlcData, 1); 23]); // CSI: FF C0 02 + 20 ident octets
    want.push((FieldType::HdlcFcsOk, 0));
    want.extend([(FieldType::HdlcData, 1); 6]); // DIS
    want.extend([(FieldType::HdlcFcsOkSigEnd, 0); 3]);
    assert_eq!(&seq[..want.len()], &want[..]);
    let frames = b.frames();
    assert_eq!(frames[0].1[..3], [0xFF, 0xC0, 0x02]);
    assert_eq!(frames[1].1, hexs("FF C8 01 00 77 1E"));
    // CSI carries the number reversed
    let rev: Vec<u8> = b"1234567820".iter().map(|b| b.reverse_bits()).collect();
    assert_eq!(&frames[0].1[3..13], &rev[..]);
}

#[test]
fn r13_hdlc_chunk_size_follows_the_target_air_time() {
    // 300 bit/s: 30 ms -> 1 octet, 100 ms -> 3 octets, 1 s -> 37 octets (whole frame).
    for (ms, per_packet) in [(30u64, 1usize), (100, 3), (1000, 6)] {
        let mut cfg = cfg_default();
        cfg.pacing.hdlc_chunk_ms = ms;
        let mut b = Bench::new(cfg);
        b.eng.start(0);
        b.run_until(6_000);
        let sizes: Vec<usize> = b
            .raw
            .iter()
            .filter_map(|(_, i)| match i {
                Ifp::Data { fields, .. } if fields[0].field_type == FieldType::HdlcData => {
                    Some(fields[0].data.len())
                }
                _ => None,
            })
            .collect();
        assert_eq!(sizes.iter().sum::<usize>(), 6);
        assert!(sizes.iter().all(|s| *s <= per_packet), "{ms} ms: {sizes:?}");
        assert_eq!(sizes[0], per_packet.min(6));
    }
}

// ---------------------------------------------------------------- R14 duplicates

#[test]
fn r14_repeated_indicators_and_sig_end_copies_are_one_event() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    // The caller sends DCS with every packet tripled by the transport.
    let dcs = hexs(DCS_STD_MH_14400);
    let t = b.send_burst(100, &[&dcs]);
    b.run_until(t);
    // exactly one training state: the engine is waiting for the TCF, not complaining
    assert_eq!(b.eng.state(), State::WaitTcf, "{:?}", b.eng.drain_log());
    let (dups, _) = b.eng.rx_counters();
    assert!(dups >= 4, "copies were recognised: {dups}");
    // a second, identical sig-end after the frame has no effect either
    b.eng
        .on_ifp(t, &data(DataType::V21, FieldType::HdlcFcsOkSigEnd, &[]));
    assert_eq!(b.eng.state(), State::WaitTcf);
}

// ---------------------------------------------------------------- R15 loss

#[test]
fn r15_a_frame_with_lost_octets_is_ignored_and_the_retransmission_works() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    let dcs = hexs(DCS_STD_MH_14400);
    // first attempt: the transport reports a hole in the middle of the frame
    b.eng.on_ifp(10, &Ifp::Indicator(Indicator::V21Preamble));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &dcs[..3]));
    b.eng.on_loss(10);
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &dcs[3..]));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcFcsOkSigEnd, &[]));
    assert_eq!(
        b.eng.state(),
        State::WaitCommand,
        "damaged DCS not acted on"
    );
    let log = b.eng.drain_log().join("\n");
    assert!(log.contains("bad FCS"), "{log}");
    // the hole flag does not leak into the next frame
    let t = b.send_burst(200, &[&dcs]);
    b.run_until(t);
    assert_eq!(b.eng.state(), State::WaitTcf);
}

#[test]
fn r15_fcs_bad_from_the_gateway_is_not_acted_on() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    let dcs = hexs(DCS_STD_MH_14400);
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &dcs));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcFcsBadSigEnd, &[]));
    assert_eq!(b.eng.state(), State::WaitCommand);
}

#[test]
fn r15_hdlc_without_a_preamble_indicator_is_accepted() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    let dcs = hexs(DCS_STD_MH_14400);
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &dcs));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcFcsOk, &[]));
    assert_eq!(b.eng.state(), State::WaitTcf);
    // ... but a frame that does not start with the address octet lost its head
    let mut b = Bench::new(EngineConfig {
        pacing: Pacing::none(),
        ..cfg_default()
    });
    b.eng.start(0);
    b.run_until(0);
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &dcs[1..]));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcFcsOk, &[]));
    assert_eq!(b.eng.state(), State::WaitCommand);
}

#[test]
fn r15_octets_in_the_fcs_field_and_hdlc_sig_end_are_tolerated() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    let dcs = hexs(DCS_STD_MH_14400);
    // Cisco style: the last octets ride in the fcs-OK field
    b.eng.on_ifp(10, &Ifp::Indicator(Indicator::V21Preamble));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &dcs[..4]));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcFcsOk, &dcs[4..]));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcSigEnd, &[]));
    assert_eq!(b.eng.state(), State::WaitTcf);
    // an unrelated field type on the V.21 data type is ignored
    b.eng.on_ifp(
        11,
        &data(DataType::V21, FieldType::T4NonEcmData, &[1, 2, 3]),
    );
    assert_eq!(b.eng.state(), State::WaitTcf);
}

#[test]
fn r15_oversized_frames_are_marked_bad() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    let mut big = hexs(DCS_STD_MH_14400);
    big.resize(300, 0);
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcData, &big));
    b.eng
        .on_ifp(10, &data(DataType::V21, FieldType::HdlcFcsOk, &[]));
    assert_eq!(b.eng.state(), State::WaitCommand);
}

// ---------------------------------------------------------------- R16 whole calls

/// Plays a complete call and returns the bench.
fn full_call(
    cfg: EngineConfig,
    pages: &[(Vec<u8>, usize)],
    dcs: &str,
    ind: Indicator,
    dt: DataType,
    rate_bytes: usize,
    chunk: usize,
) -> Bench {
    full_call_gap(cfg, pages, dcs, ind, dt, rate_bytes, chunk, 400)
}

/// [`full_call`] where the caller starts its next signal `gap` ms after the end of our
/// previous frame (the engine's own burst tail lasts about 220 ms).
#[allow(clippy::too_many_arguments)]
fn full_call_gap(
    cfg: EngineConfig,
    pages: &[(Vec<u8>, usize)],
    dcs: &str,
    ind: Indicator,
    dt: DataType,
    rate_bytes: usize,
    chunk: usize,
    gap: u64,
) -> Bench {
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    // wait for the first DIS burst
    while !b.frame_fcfs().contains(&0x01) {
        b.step();
        b.now += 1;
    }
    let t0 = b.now + 300; // the burst is out; answer well before T4
    let base = b.frame_fcfs();
    assert_eq!(
        *base.last().unwrap(),
        0x01,
        "DIS (after CSI when an id is set)"
    );
    let dcs = hexs(dcs);
    let mut t = b.send_burst(t0, &[&tsi("12345678"), &dcs]);
    // TCF: 1.5 s of zeros
    t = b.send_image(t, ind, dt, &vec![0u8; rate_bytes * 3 / 2], chunk);
    b.run_until_fcf(0x21, t + 2_500);
    let mut want = base.clone();
    want.push(0x21);
    assert_eq!(
        b.frame_fcfs(),
        want,
        "CFR after the TCF: {:?}",
        b.eng.drain_log()
    );
    // the caller answers `gap` ms after the end of our CFR (our burst tail is still running)
    t = b.frames().last().unwrap().0 + gap;
    for (i, (bm, rows)) in pages.iter().enumerate() {
        t = b.send_image(t, ind, dt, &mh(bm, *rows), chunk);
        let cmd: &str = if i + 1 == pages.len() { EOP } else { MPS };
        t = b.send_burst(t, &[&hexs(cmd)]);
        b.run_until_fcf(0x31, t + 2_500);
        t = b.frames().last().unwrap().0 + gap;
    }
    let _ = b.send_burst(t, &[&hexs(DCN)]);
    b.run_until_done(t + 10_000);
    b
}

fn tsi(id: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xC0, 0xC2];
    let mut f = vec![0x04u8; 20];
    for (i, c) in id.bytes().rev().enumerate() {
        f[i] = c.reverse_bits();
    }
    v.extend(f);
    v
}

#[test]
fn r16_three_page_call_paced_every_page_pixel_exact() {
    let pages: Vec<(Vec<u8>, usize)> = (0..3)
        .map(|i| (test_bitmap(120 + i * 10, i as u32), 120 + i * 10))
        .collect();
    let mut b = full_call(
        cfg_id(),
        &pages,
        DCS_STD_MH_14400,
        Indicator::V17_14400LongTraining,
        DataType::V17_14400,
        1800,
        54,
    );
    let r = b.eng.result().unwrap().clone();
    assert_eq!(r.code, t30::ResultCode::Ok, "{:#?}", b.eng.drain_log());
    assert_eq!(r.pages, 3);
    assert_eq!(r.bit_rate, 14400);
    assert_eq!(r.remote_ident.as_deref(), Some("12345678"));
    assert_eq!(r.bad_lines_per_page, vec![0, 0, 0]);
    let got = b.eng.take_pages();
    assert_eq!(got.len(), 3);
    for (i, p) in got.iter().enumerate() {
        assert_eq!(p.number, i as u32 + 1);
        assert_eq!(
            (p.width, p.height, p.x_dpi, p.y_dpi),
            (1728, pages[i].1 as u32, 204, 98)
        );
        assert_eq!(decode_g4(p), pages[i].0, "page {} pixel exact", i + 1);
        assert_eq!(p.source_coding, t30::Compression::Mh);
        // and the page is writable as TIFF / PDF
        let tiff = t4::write_tiff(
            &[p.as_t4_page()],
            &t4::TiffMeta {
                software: "t",
                datetime: "2026:01:01 00:00:00",
                image_description: None,
            },
        )
        .unwrap();
        assert_eq!(&tiff[..2], b"II");
        let pdf = t4::write_pdf(&[p.as_t4_page()]).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }
    assert!(b.eng.take_pages().is_empty());
}

/// A real gateway (SpanDSP over loopback) answers our CFR / MCF as soon as it has seen the
/// last frame, i.e. before the engine's own burst tail (about 220 ms of flags, FCS and hold)
/// is over. Data that arrives then must not be lost (found by the M2d end-to-end tests: one
/// to three bad lines at the start of the second page).
#[test]
fn r16_peer_that_answers_before_our_burst_tail_is_over_loses_nothing() {
    let pages: Vec<(Vec<u8>, usize)> = (0..3)
        .map(|i| (test_bitmap(120 + i * 10, i as u32), 120 + i * 10))
        .collect();
    let mut b = full_call_gap(
        cfg_id(),
        &pages,
        DCS_STD_MH_14400,
        Indicator::V17_14400LongTraining,
        DataType::V17_14400,
        1800,
        54,
        5,
    );
    let r = b.eng.result().unwrap().clone();
    assert_eq!(r.code, t30::ResultCode::Ok, "{:?}", b.eng.drain_log());
    assert_eq!(r.pages, 3);
    assert_eq!(r.bad_lines_per_page, vec![0, 0, 0]);
    let got = b.eng.take_pages();
    for (i, p) in got.iter().enumerate() {
        assert_eq!(decode_g4(p), pages[i].0, "page {} pixel exact", i + 1);
    }
}

#[test]
fn r16_call_with_version_3_encoding_roundtrips_through_bytes() {
    // Same call but the packets cross the wire as PER octets (v3), both directions.
    let mut cfg = cfg_default();
    cfg.version = T38Version::V3;
    cfg.pacing = Pacing::none();
    let mut e = T38Engine::new(cfg);
    e.start(0);
    let outs: Vec<Vec<u8>> = e
        .poll_timed()
        .iter()
        .map(|t| t.encode(T38Version::V3).unwrap())
        .collect();
    assert!(!outs.is_empty());
    for o in &outs {
        assert!(Ifp::decode(o, T38Version::V3).is_ok());
    }
    let dcs = Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(FieldType::HdlcData, hexs(DCS_STD_MH_14400))],
    };
    e.on_ifp_bytes(5, &dcs.encode(T38Version::V3).unwrap())
        .unwrap();
    let ok = Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(FieldType::HdlcFcsOk, vec![])],
    };
    e.on_ifp_bytes(5, &ok.encode(T38Version::V3).unwrap())
        .unwrap();
    assert_eq!(e.state(), State::WaitTcf);
    // garbage is an error and counts as lost data
    let _ = e.on_ifp_bytes(6, &[0xFF, 0xFF, 0xFF]);
    // trailing zero padding (FEC recovered packets) is fine
    let mut padded = Ifp::Indicator(Indicator::NoSignal)
        .encode(T38Version::V3)
        .unwrap();
    padded.extend([0, 0, 0]);
    e.on_ifp_bytes(7, &padded).unwrap();
}

#[test]
fn r16_rates_4800_and_9600_use_their_own_tcf_length() {
    for (dcs, ind, dt, rate_bytes) in [
        (
            "FF C8 C1 00 50 1E",
            Indicator::V27ter4800Training,
            DataType::V27ter4800,
            600usize,
        ),
        (
            "FF C8 C1 00 60 1E",
            Indicator::V29_9600Training,
            DataType::V29_9600,
            1200,
        ),
    ] {
        let pages = vec![(test_bitmap(60, 5), 60usize)];
        let b = full_call(cfg_default(), &pages, dcs, ind, dt, rate_bytes, 40);
        assert_eq!(b.eng.result().unwrap().code, t30::ResultCode::Ok, "{dcs}");
    }
}

#[test]
fn r16_local_tcf_needs_no_tcf_and_no_cfr() {
    let mut cfg = cfg_default();
    cfg.t30.tcf = t30::TcfMode::Local;
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(4_700);
    let t = b.send_burst(4_800, &[&hexs(DCS_STD_MH_14400)]);
    b.run_until(t + 2_500);
    assert_eq!(b.frame_fcfs(), [0x01], "no CFR with local TCF");
    let bm = test_bitmap(80, 2);
    let t = b.send_image(
        t,
        Indicator::V17_14400ShortTraining,
        DataType::V17_14400,
        &mh(&bm, 80),
        54,
    );
    let t = b.send_burst(t, &[&hexs(EOP)]);
    b.run_until(t + 2_500);
    let t = b.send_burst(t + 2_500, &[&hexs(DCN)]);
    b.run_until_done(t + 5_000);
    assert_eq!(b.eng.result().unwrap().code, t30::ResultCode::Ok);
    let p = b.eng.take_pages();
    assert_eq!(decode_g4(&p[0]), bm);
}

#[test]
fn r16_failure_to_train_then_success_at_a_lower_rate() {
    let mut b = Bench::new(cfg_default());
    b.eng.start(0);
    b.run_until(4_700);
    // 14400: the TCF arrives with errors
    let mut tcf = vec![0u8; 2700];
    for i in (0..2700).step_by(7) {
        tcf[i] = 0xFF;
    }
    let mut t = b.send_burst(4_800, &[&hexs(DCS_STD_MH_14400)]);
    t = b.send_image(
        t,
        Indicator::V17_14400LongTraining,
        DataType::V17_14400,
        &tcf,
        54,
    );
    b.run_until(t + 2_500);
    assert_eq!(b.frame_fcfs(), [0x01, 0x22], "FTT");
    // fall back to 9600 V.17
    t = t.max(b.frames().last().unwrap().0 + 100);
    t = b.send_burst(t, &[&hexs("FF C8 C1 00 64 1E")]);
    t = b.send_image(
        t,
        Indicator::V17_9600LongTraining,
        DataType::V17_9600,
        &vec![0u8; 1800],
        54,
    );
    b.run_until(t + 2_500);
    assert_eq!(b.frame_fcfs(), [0x01, 0x22, 0x21], "CFR");
    t = t.max(b.frames().last().unwrap().0 + 100);
    let bm = test_bitmap(50, 9);
    t = b.send_image(
        t,
        Indicator::V17_9600ShortTraining,
        DataType::V17_9600,
        &mh(&bm, 50),
        54,
    );
    t = b.send_burst(t, &[&hexs(EOP)]);
    b.run_until(t + 2_500);
    let _ = b.send_burst(t + 2_500, &[&hexs(DCN)]);
    b.run_until_done(t + 20_000);
    let r = b.eng.result().unwrap();
    assert_eq!(
        (r.code, r.pages, r.bit_rate),
        (t30::ResultCode::Ok, 1, 9600)
    );
}

#[test]
fn r16_mmr_page_from_a_dcs_with_t6_is_decoded() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    cfg.t30.tcf = t30::TcfMode::Local;
    let mut b = Bench::new(cfg);
    b.eng.start(0);
    b.run_until(0);
    // DCS: receive, V.17 14400, T.6 (bit 31, so four octets and the extend bit 24)
    let t = b.send_burst(10, &[&hexs("FF C8 C1 00 44 1F 02")]);
    b.run_until(t);
    let bm = test_bitmap(70, 3);
    let g4 = t4::encode_mmr(&bm, 1728).unwrap();
    let t = b.send_image(
        t,
        Indicator::V17_14400ShortTraining,
        DataType::V17_14400,
        &g4,
        60,
    );
    let t = b.send_burst(t, &[&hexs(EOP)]);
    b.run_until(t + 100);
    let t = b.send_burst(t + 100, &[&hexs(DCN)]);
    b.run_until_done(t + 100);
    assert_eq!(b.eng.result().unwrap().code, t30::ResultCode::Ok);
    let p = b.eng.take_pages();
    assert_eq!(p[0].source_coding, t30::Compression::Mmr);
    assert_eq!(decode_g4(&p[0]), bm);
}

#[test]
fn r16_dcn_in_the_middle_of_a_page_keeps_the_finished_pages() {
    let mut b = Bench::new(cfg_default());
    b.eng.start(0);
    b.run_until(4_700);
    let mut t = b.send_burst(4_800, &[&hexs(DCS_STD_MH_14400)]);
    t = b.send_image(
        t,
        Indicator::V17_14400LongTraining,
        DataType::V17_14400,
        &vec![0u8; 2700],
        54,
    );
    b.run_until(t + 2_500);
    t = t.max(b.frames().last().unwrap().0 + 100);
    let bm = test_bitmap(40, 1);
    t = b.send_image(
        t,
        Indicator::V17_14400ShortTraining,
        DataType::V17_14400,
        &mh(&bm, 40),
        54,
    );
    t = b.send_burst(t, &[&hexs(MPS)]);
    b.run_until(t + 2_500);
    t = t.max(b.frames().last().unwrap().0 + 100);
    // page 2 is cut off: only the start arrives, then the caller hangs up
    let part = mh(&test_bitmap(40, 2), 40);
    b.schedule(t, Ifp::Indicator(Indicator::V17_14400ShortTraining));
    b.schedule(
        t + 200,
        data(DataType::V17_14400, FieldType::T4NonEcmData, &part[..300]),
    );
    let _ = b.send_burst(t + 500, &[&hexs(DCN)]);
    b.run_until_done(t + 20_000);
    let r = b.eng.result().unwrap();
    assert_eq!((r.code, r.pages), (t30::ResultCode::PartialDcn, 1));
    assert!(r.is_partial());
    let pages = b.eng.take_pages();
    assert_eq!(pages.len(), 1);
    assert_eq!(decode_g4(&pages[0]), bm);
}

#[test]
fn r16_abort_discards_pending_output_and_reports_aborted() {
    let mut b = Bench::new(cfg_default());
    b.eng.start(0);
    b.run_until(500);
    b.eng.abort(600);
    assert!(b.eng.is_done());
    assert_eq!(b.eng.result().unwrap().code, t30::ResultCode::Aborted);
    let n = b.raw.len();
    b.run_until(10_000);
    assert_eq!(b.raw.len(), n, "nothing is sent after the abort");
    assert_eq!(b.eng.next_wakeup_ms(), None);
}

// ---------------------------------------------------------------- R17 wake-ups and misc

#[test]
fn r17_next_wakeup_tracks_packets_then_timers() {
    let mut b = Bench::new(cfg_default());
    assert_eq!(b.eng.next_wakeup_ms(), None);
    b.eng.start(0);
    assert_eq!(
        b.eng.next_wakeup_ms(),
        Some(200),
        "start released the t=0 packets"
    );
    b.run_until(4_658);
    // the burst is out; T1 (35 s) and T4 (3 s after 4658.329) are running
    assert_eq!(b.eng.next_wakeup_ms(), Some(7_658));
    assert_eq!(b.eng.config().indicator_repeat, 3);
    assert_eq!(b.eng.state(), State::WaitCommand);
}

#[test]
fn r17_clock_may_jump_and_events_still_happen_in_order() {
    // Advance in one big step: all packets come out with their own instants.
    let mut e = T38Engine::new(cfg_default());
    e.start(0);
    e.advance(60_000);
    let out = e.poll_timed();
    let times: Vec<u64> = out.iter().map(|t| t.at_ms).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "monotonic");
    assert!(
        e.is_done(),
        "the DIS retries ran out inside the one big step"
    );
    assert_eq!(e.result().unwrap().code, t30::ResultCode::NoResponseToDis);
    let log = e.drain_log();
    assert!(log.iter().any(|l| l.contains("T.30 done")));
}

#[test]
fn r17_packets_after_done_are_ignored() {
    let mut e = T38Engine::new(cfg_default());
    e.start(0);
    e.abort(1);
    let _ = e.poll_output();
    e.on_ifp(2, &Ifp::Indicator(Indicator::Cng));
    e.on_loss(3);
    e.advance(4);
    assert!(e.poll_output().is_empty());
}

#[test]
fn r17_log_is_bounded() {
    let mut cfg = cfg_default();
    cfg.max_log_lines = 5;
    let mut e = T38Engine::new(cfg);
    e.start(0);
    for i in 0..50u64 {
        e.on_ifp(i, &data(DataType::V21, FieldType::HdlcData, &[0x00, 0x01]));
        e.on_ifp(i, &data(DataType::V21, FieldType::HdlcFcsOk, &[]));
    }
    assert!(e.drain_log().len() <= 5);
    assert!(e.drain_log().is_empty());
}

#[test]
fn r17_ignored_inputs_are_counted_not_acted_on() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut e = T38Engine::new(cfg);
    e.start(0);
    // V.34 / V.8 material and ECM HDLC on a fast modem
    e.on_ifp(1, &Ifp::Indicator(Indicator::V8Ansam));
    e.on_ifp(1, &data(DataType::V8, FieldType::CmMessage, &[1]));
    e.on_ifp(1, &data(DataType::V17_14400, FieldType::HdlcData, &[1]));
    let (_, ignored) = e.rx_counters();
    assert_eq!(ignored, 3);
    assert_eq!(e.state(), State::SendingDis.max_state());
}

trait MaxState {
    fn max_state(self) -> State;
}
impl MaxState for State {
    fn max_state(self) -> State {
        // pacing is off, so the DIS burst completed at once
        State::WaitCommand
    }
}

// ---------------------------------------------------------------- R18 UDPTL wrapper

#[test]
fn r18_udptl_wrapper_frames_datagrams_with_redundancy_and_recovers_losses() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut u = UdptlEngine::new(
        cfg,
        ErrorRecovery::Redundancy { depth: 3 },
        ReceiverConfig::default(),
    );
    u.start(0);
    u.advance(0);
    let out = u.poll_datagrams();
    assert!(out.len() > 10);
    // sequence numbers 0.. and a redundancy list that grows to 3
    let first = t38::UdptlPacket::decode(&out[0].bytes).unwrap();
    assert_eq!(first.seq, 0);
    let last = t38::UdptlPacket::decode(&out.last().unwrap().bytes).unwrap();
    assert_eq!(last.seq as usize, out.len() - 1);
    assert!(matches!(&last.recovery, t38::UdptlRecovery::Secondary(l) if l.len() == 3));

    // Feed the caller's DCS burst as datagrams with one datagram dropped in the middle: the
    // redundancy of the next one repairs it, so the frame is intact.
    let mut tx = t38::UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 100);
    let dcs = hexs(DCS_STD_MH_14400);
    let pkts = [
        Ifp::Indicator(Indicator::V21Preamble),
        data(DataType::V21, FieldType::HdlcData, &dcs[..3]),
        data(DataType::V21, FieldType::HdlcData, &dcs[3..]),
        data(DataType::V21, FieldType::HdlcFcsOkSigEnd, &[]),
    ];
    for (i, p) in pkts.iter().enumerate() {
        let d = tx.build(&p.encode(T38Version::V0).unwrap()).unwrap();
        if i == 1 {
            continue; // lost
        }
        u.on_datagram(10 + i as u64, &d).unwrap();
    }
    assert_eq!(u.engine().state(), State::WaitTcf);
    assert_eq!(u.receiver_stats().recovered_redundancy, 1);
    assert_eq!(u.gaps, 0);
    assert!(u.on_datagram(20, &[1, 2, 3]).is_err());
    assert_eq!(u.malformed, 1);
}

#[test]
fn r18_unrecoverable_loss_gives_a_bad_fcs_and_the_reorder_timeout_flushes() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut u = UdptlEngine::new(cfg, ErrorRecovery::None, ReceiverConfig::default());
    u.start(0);
    u.set_reorder_wait_ms(50);
    let mut tx = t38::UdptlSender::new(ErrorRecovery::None, 0);
    let dcs = hexs(DCS_STD_MH_14400);
    let pkts = [
        Ifp::Indicator(Indicator::V21Preamble),
        data(DataType::V21, FieldType::HdlcData, &dcs[..3]),
        data(DataType::V21, FieldType::HdlcData, &dcs[3..]),
        data(DataType::V21, FieldType::HdlcFcsOkSigEnd, &[]),
    ];
    for (i, p) in pkts.iter().enumerate() {
        let d = tx.build(&p.encode(T38Version::V0).unwrap()).unwrap();
        if i == 1 {
            continue;
        }
        u.on_datagram(10, &d).unwrap();
    }
    // the later packets are held back waiting for the hole
    assert_eq!(u.engine().state(), State::WaitCommand);
    assert_eq!(
        u.next_wakeup_ms(),
        Some(10 + 50).map(|w| w.min(u.engine().next_wakeup_ms().unwrap_or(u64::MAX)))
    );
    u.advance(59);
    assert_eq!(u.gaps, 0);
    u.advance(60);
    assert_eq!(u.gaps, 1, "hole given up after the reorder wait");
    // the damaged frame was seen as bad: still waiting for a command
    assert_eq!(u.engine().state(), State::WaitCommand);
    let log = u.engine_mut().drain_log().join("|");
    assert!(log.contains("bad FCS"), "{log}");
}

#[test]
fn r18_undecodable_ifp_inside_a_valid_datagram_counts_as_loss() {
    let mut cfg = cfg_default();
    cfg.pacing = Pacing::none();
    let mut u = UdptlEngine::new(cfg, ErrorRecovery::None, ReceiverConfig::default());
    u.start(0);
    let mut tx = t38::UdptlSender::new(ErrorRecovery::None, 0);
    // an IFP that claims a data field but is truncated
    let d = tx.build(&[0xC0, 0x05]).unwrap();
    u.on_datagram(5, &d).unwrap();
    assert_eq!(u.malformed, 1);
}

/// SpanDSP-style signal end: the last octets ride in the `t4-non-ecm-sig-end` packet and the
/// copies repeat them. The copies are duplicates, not data, whatever state T.30 is in by then.
#[test]
fn r14_sig_end_copies_that_repeat_the_last_octets_are_duplicates() {
    let mut b = Bench::new(cfg_id());
    b.eng.start(0);
    while !b.frame_fcfs().contains(&0x01) {
        b.step();
        b.now += 1;
    }
    let t0 = b.now + 300;
    let dcs = hexs(DCS_STD_MH_14400);
    let mut t = b.send_burst(t0, &[&tsi("12345678"), &dcs]);
    for _ in 0..3 {
        b.schedule(t, Ifp::Indicator(Indicator::V17_14400LongTraining));
    }
    t += 200;
    let dt = DataType::V17_14400;
    for _ in 0..50 {
        b.schedule(t, data(dt, FieldType::T4NonEcmData, &[0u8; 54]));
        t += 30;
    }
    for _ in 0..3 {
        // the copies arrive one after the other, the engine answers in between
        b.schedule(t, data(dt, FieldType::T4NonEcmSigEnd, &[0u8; 54]));
        t += 60;
    }
    b.schedule(t, Ifp::Indicator(Indicator::NoSignal));
    b.run_until_fcf(0x21, t + 5_000);
    b.run_until(b.now + 1_000);
    assert_eq!(
        b.eng.state(),
        State::WaitImage,
        "no phantom page was started"
    );
    let (dups, _) = b.eng.rx_counters();
    assert!(
        dups >= 2,
        "the two repeated sig-ends are duplicates: {dups}"
    );
    assert_eq!(b.frame_fcfs().iter().filter(|f| **f == 0x21).count(), 1);
}
