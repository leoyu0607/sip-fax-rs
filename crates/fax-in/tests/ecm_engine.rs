//! ECM through the T.38 engine (M3d): HDLC frames at image rate (`hdlc-data` on V.17 and the
//! other image data types) become T.30 frames, and the engine answers PPS / CTC / EOR.
//!
//! The fax machine on the other end is a scripted caller in this file; it shares no code with
//! the engine (frames are assembled from the T.30 / T.4 Annex A text: FCD `60`, RCP `61`, PPS
//! `7D` + FCF2, frame numbers LSB first). 64 octet frames are tested here because SpanDSP
//! always sends 256 octet frames; the SpanDSP interop is `tools/fax-sim/tests/e_ecm.rs`.

use fax_in::t38engine::{EngineConfig, Pacing, T38Engine};
use std::collections::VecDeque;
use t38::{DataType, Field, FieldType, Ifp, Indicator};

const V17: DataType = DataType::V17_14400;

struct Caller {
    eng: T38Engine,
    now: u64,
    heard: VecDeque<Vec<u8>>,
    hdlc: Vec<u8>,
}

fn data(dt: DataType, ft: FieldType, bytes: &[u8]) -> Ifp {
    Ifp::Data {
        data_type: dt,
        fields: vec![Field::new(ft, bytes.to_vec())],
    }
}

fn cfg() -> EngineConfig {
    let mut c = EngineConfig {
        pacing: Pacing::none(),
        ..EngineConfig::default()
    };
    c.t30.ecm = true;
    c.t30.mmr = true;
    c.t30.max_rows = 2000;
    c
}

impl Caller {
    fn new(cfg: EngineConfig) -> Self {
        let mut c = Self {
            eng: T38Engine::new(cfg),
            now: 0,
            heard: VecDeque::new(),
            hdlc: Vec::new(),
        };
        c.eng.start(0);
        c.collect();
        c
    }

    fn collect(&mut self) {
        for ifp in self.eng.poll_output() {
            if let Ifp::Data { fields, .. } = ifp {
                for f in fields {
                    match f.field_type {
                        FieldType::HdlcData => self.hdlc.extend(&f.data),
                        FieldType::HdlcFcsOk | FieldType::HdlcFcsOkSigEnd
                            if !self.hdlc.is_empty() =>
                        {
                            self.heard.push_back(std::mem::take(&mut self.hdlc));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn put(&mut self, ifp: Ifp) {
        self.now += 20;
        self.eng.on_ifp(self.now, &ifp);
        self.collect();
    }

    /// The next frame the engine sent (must exist).
    fn frame(&mut self) -> Vec<u8> {
        self.collect();
        self.heard
            .pop_front()
            .unwrap_or_else(|| panic!("engine sent nothing; log {:#?}", self.eng.drain_log()))
    }

    fn v21(&mut self, frames: &[Vec<u8>]) {
        self.put(Ifp::Indicator(Indicator::V21Preamble));
        for (i, f) in frames.iter().enumerate() {
            self.put(data(DataType::V21, FieldType::HdlcData, f));
            let ft = if i + 1 == frames.len() {
                FieldType::HdlcFcsOkSigEnd
            } else {
                FieldType::HdlcFcsOk
            };
            self.put(data(DataType::V21, ft, &[]));
        }
        self.put(Ifp::Indicator(Indicator::NoSignal));
        self.now += 100;
        self.eng.advance(self.now);
        self.collect();
    }

    /// Phase B and the TCF up to the CFR; `extra` are DCS bits beyond the usual (27, 28).
    fn negotiate(&mut self, frame64: bool) {
        let dis = self.frame();
        assert_eq!(&dis[..3], [0xFF, 0xC8, 0x01], "DIS");
        // the DIS of an ECM engine: bits 27 (ECM) and 31 (T.6) in octet 4
        assert_eq!(
            dis[3..],
            [0x00, 0x77, 0x1F, 0x22],
            "DIS FIF with ECM and MMR"
        );
        let dcs = vec![
            0xFF,
            0xC8,
            0xC1,
            0x00,
            0x44,
            0x1F,
            0x22 | if frame64 { 0x10 } else { 0 },
        ];
        self.v21(&[dcs]);
        self.put(Ifp::Indicator(Indicator::V17_14400LongTraining));
        for _ in 0..8 {
            self.put(data(V17, FieldType::T4NonEcmData, &[0u8; 270]));
        }
        self.put(data(V17, FieldType::T4NonEcmSigEnd, &[]));
        self.put(Ifp::Indicator(Indicator::NoSignal));
        let cfr = self.frame();
        assert_eq!(cfr, [0xFF, 0xC8, 0x21], "CFR");
    }

    /// One transmission of ECM frames: training, the frames (each `hdlc-data` + `hdlc-fcs-OK`),
    /// three RCP, the last one with the end of the signal.
    fn ecm_burst(&mut self, frames: &[(u8, &[u8])]) {
        self.put(Ifp::Indicator(Indicator::V17_14400LongTraining));
        for (n, d) in frames {
            let mut f = vec![0xFF, 0xC0, 0x60, n.reverse_bits()];
            f.extend_from_slice(d);
            self.put(data(V17, FieldType::HdlcData, &f));
            self.put(data(V17, FieldType::HdlcFcsOk, &[]));
        }
        for i in 0..3 {
            self.put(data(V17, FieldType::HdlcData, &[0xFF, 0xC0, 0x61]));
            let ft = if i == 2 {
                FieldType::HdlcFcsOkSigEnd
            } else {
                FieldType::HdlcFcsOk
            };
            self.put(data(V17, ft, &[]));
        }
        self.put(Ifp::Indicator(Indicator::NoSignal));
    }

    fn pps(&mut self, q: u8, page: u8, block: u8, frames: usize) {
        self.v21(&[vec![
            0xFF,
            0xC8,
            0xFD,
            q,
            page.reverse_bits(),
            block.reverse_bits(),
            ((frames - 1) as u8).reverse_bits(),
        ]]);
    }
}

fn sparse_bitmap(rows: usize, seed: u32) -> Vec<u8> {
    let mut v = vec![0u8; 216 * rows];
    for y in 0..rows {
        for (start, len) in [
            (
                (y as u32 * 37 + seed * 13) % 1600,
                5 + (y as u32 + seed) % 30,
            ),
            ((y as u32 * 11 + seed * 7) % 1500, 8 + (y as u32 * 3) % 17),
        ] {
            for x in start..(start + len).min(1728) {
                v[y * 216 + (x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
    }
    v
}

fn frames_of(bitmap: &[u8], size: usize) -> Vec<Vec<u8>> {
    let coded = t4::encode_mmr(bitmap, 1728).unwrap();
    coded
        .chunks(size)
        .map(|c| {
            let mut v = c.to_vec();
            v.resize(size, 0);
            v
        })
        .collect()
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

fn refs(frames: &[Vec<u8>], which: &[usize]) -> Vec<(u8, Vec<u8>)> {
    which
        .iter()
        .map(|&i| (i as u8, frames[i].clone()))
        .collect()
}

fn burst(c: &mut Caller, items: &[(u8, Vec<u8>)]) {
    let v: Vec<(u8, &[u8])> = items.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    c.ecm_burst(&v);
}

#[test]
fn e01e_two_pages_of_64_octet_frames_with_several_blocks_are_pixel_exact() {
    let mut c = Caller::new(cfg());
    c.negotiate(true);
    let pages = [sparse_bitmap(300, 1), sparse_bitmap(300, 2)];
    for (p, bm) in pages.iter().enumerate() {
        let frames = frames_of(bm, 64);
        assert!(frames.len() > 20, "{} frames", frames.len());
        let chunks: Vec<&[Vec<u8>]> = frames.chunks(40).collect();
        for (ci, chunk) in chunks.iter().enumerate() {
            let block = ci as u8;
            let items: Vec<(u8, Vec<u8>)> = chunk
                .iter()
                .enumerate()
                .map(|(i, f)| (i as u8, f.clone()))
                .collect();
            burst(&mut c, &items);
            let last = ci + 1 == chunks.len();
            let q = if !last {
                0x00
            } else if p == 0 {
                0xF2
            } else {
                0xF4
            };
            c.pps(q, p as u8, block, chunk.len());
            assert_eq!(c.frame(), [0xFF, 0xC8, 0x31], "MCF page {p} block {block}");
        }
    }
    c.v21(&[vec![0xFF, 0xC8, 0xDF]]);
    assert!(c.eng.is_done());
    let r = c.eng.result().unwrap().clone();
    assert!(r.code.is_ok(), "{r:?}");
    assert!(r.ecm);
    assert_eq!((r.pages, r.ecm_ppr_sent, r.ecm_frames_lost), (2, 0, 0));
    let got = c.eng.take_pages();
    assert_eq!(got.len(), 2);
    for (g, bm) in got.iter().zip(&pages) {
        assert_eq!(&decode_g4(g), bm, "pixel exact");
    }
}

#[test]
fn e03e_ppr_recovers_frames_lost_in_the_first_transmission() {
    let mut c = Caller::new(cfg());
    c.negotiate(false);
    let bm = sparse_bitmap(400, 3);
    let frames = frames_of(&bm, 256);
    let n = frames.len();
    assert!(n >= 8, "{n} frames");
    let lost = [1usize, n / 2, n - 1];
    let first: Vec<usize> = (0..n).filter(|i| !lost.contains(i)).collect();
    burst(&mut c, &refs(&frames, &first));
    c.pps(0xF4, 0, 0, n);
    let ppr = c.frame();
    assert_eq!(&ppr[..3], [0xFF, 0xC8, 0x3D]);
    for i in 0..256usize {
        let bit = ppr[3 + i / 8] & (0x80 >> (i % 8)) != 0;
        assert_eq!(bit, lost.contains(&i) || i >= n, "bit {i}");
    }
    // retransmission of exactly the requested frames, then the PPS again
    burst(&mut c, &refs(&frames, &lost));
    c.pps(0xF4, 0, 0, lost.len());
    assert_eq!(c.frame(), [0xFF, 0xC8, 0x31]);
    c.v21(&[vec![0xFF, 0xC8, 0xDF]]);
    let r = c.eng.result().unwrap().clone();
    assert!(r.code.is_ok());
    assert_eq!((r.pages, r.ecm_ppr_sent), (1, 1));
    let pages = c.eng.take_pages();
    assert_eq!(decode_g4(&pages[0]), bm);
}

#[test]
fn e04e_persistent_loss_ctc_then_eor_ends_with_frames_lost() {
    let mut c = Caller::new(cfg());
    c.negotiate(false);
    let bm = sparse_bitmap(400, 4);
    let frames = frames_of(&bm, 256);
    let n = frames.len();
    let first: Vec<usize> = (0..n).filter(|i| *i != 2).collect();
    burst(&mut c, &refs(&frames, &first));
    // four PPRs for the same frame, the retransmissions are lost again
    for round in 1..=4 {
        c.pps(0xF2, 0, 0, n);
        let ppr = c.frame();
        assert_eq!(ppr[2], 0x3D, "PPR {round}");
        assert_ne!(ppr[3] & 0x20, 0, "frame 2 still requested");
        burst(&mut c, &[]); // the resent frame never arrives
    }
    // CTC at 9600 bit/s (V.29): FF C8 C8 00 <bits 9..16: V.29 = bit 11 -> 0x20>
    c.v21(&[vec![0xFF, 0xC8, 0xC8, 0x00, 0x20]]);
    assert_eq!(c.frame(), [0xFF, 0xC8, 0x23], "CTR");
    for round in 1..=4 {
        c.pps(0xF2, 0, 0, n);
        assert_eq!(c.frame()[2], 0x3D, "PPR {round} after CTC");
        burst(&mut c, &[]);
    }
    c.v21(&[vec![0xFF, 0xC8, 0xF3, 0xF2]]); // EOR-MPS
    assert_eq!(c.frame(), [0xFF, 0xC8, 0x38], "ERR");
    // the next page and the end
    let bm2 = sparse_bitmap(120, 5);
    let f2 = frames_of(&bm2, 256);
    let all: Vec<usize> = (0..f2.len()).collect();
    burst(&mut c, &refs(&f2, &all));
    c.pps(0xF4, 1, 0, f2.len());
    assert_eq!(c.frame(), [0xFF, 0xC8, 0x31]);
    c.v21(&[vec![0xFF, 0xC8, 0xDF]]);
    let r = c.eng.result().unwrap().clone();
    assert_eq!(r.code, t30::ResultCode::EcmFramesLost, "{r:?}");
    assert_eq!((r.pages, r.ecm_ppr_sent, r.ecm_frames_lost), (2, 8, 1));
    assert!(r.is_partial());
    assert_eq!((r.bit_rate, r.modem), (9600, Some(t30::Modem::V29)));
    let pages = c.eng.take_pages();
    assert_eq!(pages.len(), 2);
    assert_ne!(decode_g4_lenient(&pages[0]), bm, "frame 2 is a hole");
    assert_eq!(decode_g4(&pages[1]), bm2, "the next page is intact");
}

fn decode_g4_lenient(page: &fax_in::t38engine::ReceivedPage) -> Vec<u8> {
    t4::decode_page(
        &page.g4,
        t4::DecoderConfig::new(page.width, t4::Coding::Mmr),
    )
    .unwrap()
    .bitmap
}

#[test]
fn e05e_a_packet_loss_inside_an_ecm_frame_marks_only_that_frame_missing() {
    let mut c = Caller::new(cfg());
    c.negotiate(false);
    let bm = sparse_bitmap(300, 6);
    let frames = frames_of(&bm, 256);
    let n = frames.len();
    assert!(n >= 4);
    // frame 1 arrives in two `hdlc-data` packets and the second one is lost (on_loss)
    c.put(Ifp::Indicator(Indicator::V17_14400LongTraining));
    for (i, f) in frames.iter().enumerate() {
        let mut fr = vec![0xFF, 0xC0, 0x60, (i as u8).reverse_bits()];
        fr.extend_from_slice(f);
        if i == 1 {
            let (a, _b) = fr.split_at(100);
            c.put(data(V17, FieldType::HdlcData, a));
            c.now += 20;
            c.eng.on_loss(c.now);
            c.put(data(V17, FieldType::HdlcFcsOk, &[]));
        } else {
            c.put(data(V17, FieldType::HdlcData, &fr));
            c.put(data(V17, FieldType::HdlcFcsOk, &[]));
        }
    }
    for i in 0..3 {
        c.put(data(V17, FieldType::HdlcData, &[0xFF, 0xC0, 0x61]));
        c.put(data(
            V17,
            if i == 2 {
                FieldType::HdlcFcsOkSigEnd
            } else {
                FieldType::HdlcFcsOk
            },
            &[],
        ));
    }
    c.put(Ifp::Indicator(Indicator::NoSignal));
    c.pps(0xF4, 0, 0, n);
    let ppr = c.frame();
    assert_eq!(ppr[2], 0x3D);
    for i in 0..n {
        let bit = ppr[3 + i / 8] & (0x80 >> (i % 8)) != 0;
        assert_eq!(bit, i == 1, "frame {i}");
    }
}

#[test]
fn e05e_fcs_bad_frames_and_a_missing_training_indicator_are_handled() {
    let mut c = Caller::new(cfg());
    c.negotiate(false);
    let bm = sparse_bitmap(300, 7);
    let frames = frames_of(&bm, 256);
    let n = frames.len();
    // no training indicator at all: the first fast hdlc-data starts the signal
    for (i, f) in frames.iter().enumerate() {
        let mut fr = vec![0xFF, 0xC0, 0x60, (i as u8).reverse_bits()];
        fr.extend_from_slice(f);
        c.put(data(V17, FieldType::HdlcData, &fr));
        c.put(data(
            V17,
            if i == 0 {
                FieldType::HdlcFcsBad
            } else {
                FieldType::HdlcFcsOk
            },
            &[],
        ));
    }
    // the signal ends with a bare hdlc-sig-end (no RCP seen)
    c.put(data(V17, FieldType::HdlcSigEnd, &[]));
    c.pps(0xF4, 0, 0, n);
    let ppr = c.frame();
    assert_eq!(ppr[2], 0x3D);
    assert_ne!(ppr[3] & 0x80, 0, "frame 0 had a bad FCS");
    for i in 0..n {
        let bit = ppr[3 + i / 8] & (0x80 >> (i % 8)) != 0;
        assert_eq!(bit, i == 0, "frame {i}");
    }
    burst(&mut c, &refs(&frames, &[0]));
    c.pps(0xF4, 0, 0, 1);
    assert_eq!(c.frame(), [0xFF, 0xC8, 0x31]);
    c.v21(&[vec![0xFF, 0xC8, 0xDF]]);
    let pages = c.eng.take_pages();
    assert_eq!(decode_g4(&pages[0]), bm);
}

#[test]
fn e05e_the_engine_survives_a_peer_that_goes_silent_in_the_middle_of_a_block() {
    let mut c = Caller::new(cfg());
    c.negotiate(false);
    let bm = sparse_bitmap(300, 8);
    let frames = frames_of(&bm, 256);
    c.put(Ifp::Indicator(Indicator::V17_14400LongTraining));
    let mut fr = vec![0xFF, 0xC0, 0x60, 0];
    fr.extend_from_slice(&frames[0]);
    c.put(data(V17, FieldType::HdlcData, &fr));
    c.put(data(V17, FieldType::HdlcFcsOk, &[]));
    // nothing else: the idle timer ends the block, T2 ends the call
    c.now += 60_000;
    c.eng.advance(c.now);
    assert!(c.eng.is_done());
    let r = c.eng.result().unwrap();
    assert_eq!(r.code, t30::ResultCode::T2ExpiredCommand);
}
