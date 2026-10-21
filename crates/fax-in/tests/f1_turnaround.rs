//! F1-01 / F1-02: never start our response while the peer's V.21 signal is still on (field case
//! F1, root cause 1). Virtual clock, exact millisecond instants.
//!
//! The peer here is the AudioCodes pattern from the field trace: a frame ends with a plain
//! `hdlc-fcs-OK`, `hdlc-sig-end` follows 120-140 ms later, then `no-signal`.

use fax_in::t38engine::{EngineConfig, Pacing, T38Engine};
use t30::TcfMode;
use t38::{DataType, Field, FieldType, Ifp, Indicator};

const DCS: [u8; 6] = [0xFF, 0xC8, 0xC1, 0x00, 0x44, 0x1E];

fn data(ft: FieldType, bytes: &[u8]) -> Ifp {
    Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(ft, bytes.to_vec())],
    }
}

/// localTCF with a CFR so that the DCS is answered at once (the response under test).
fn cfg() -> EngineConfig {
    let mut c = EngineConfig::default();
    c.t30.tcf = TcfMode::Local;
    c.t30.local_tcf_sends_cfr = true;
    c
}

struct Run {
    eng: T38Engine,
    now: u64,
    out: Vec<(u64, Ifp)>,
}

impl Run {
    /// Engine started, its DIS burst completely sent: waiting for the first command.
    fn after_dis(cfg: EngineConfig) -> Self {
        let mut r = Run {
            eng: T38Engine::new(cfg),
            now: 0,
            out: Vec::new(),
        };
        r.eng.start(0);
        r.until(5_000);
        assert_eq!(r.eng.state(), t30::State::WaitCommand);
        r.out.clear();
        r
    }

    fn until(&mut self, t: u64) {
        while self.now < t {
            self.now += 1;
            self.eng.advance(self.now);
            self.collect();
        }
    }

    fn collect(&mut self) {
        let polled = self.eng.poll_timed();
        self.out
            .extend(polled.into_iter().map(|t| (t.at_ms, t.ifp)));
    }

    /// Delivers `ifp` at `t` ms.
    fn rx(&mut self, t: u64, ifp: &Ifp) {
        self.until(t);
        self.eng.on_ifp(t, ifp);
        self.collect();
    }

    /// The peer sends its DCS; the frame ends (plain `hdlc-fcs-OK`) at `t`.
    fn dcs_frame_ends_at(&mut self, t: u64) {
        self.rx(t - 100, &Ifp::Indicator(Indicator::V21Preamble));
        for (i, b) in DCS.iter().enumerate() {
            self.rx(t - 60 + 10 * i as u64, &data(FieldType::HdlcData, &[*b]));
        }
        self.rx(t, &data(FieldType::HdlcFcsOk, &[]));
    }

    fn preamble_instants(&self) -> Vec<u64> {
        self.out
            .iter()
            .filter(|(_, i)| *i == Ifp::Indicator(Indicator::V21Preamble))
            .map(|(t, _)| *t)
            .collect()
    }
}

const T0: u64 = 6_000;

#[test]
fn f1_01_response_waits_for_sig_end_then_the_turnaround_delay() {
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0); // fcs-OK
    r.rx(T0 + 140, &data(FieldType::HdlcSigEnd, &[]));
    r.rx(T0 + 160, &Ifp::Indicator(Indicator::NoSignal));
    r.until(T0 + 400);
    // Nothing at all (not even the no-signal) went out while the peer was transmitting.
    assert!(
        r.out.iter().all(|(t, _)| *t >= T0 + 140),
        "sent during the peer's signal: {:?}",
        r.out
    );
    // sig-end at +140, turnaround 75 ms: the preamble (3 copies) at exactly +215.
    assert_eq!(r.preamble_instants(), vec![T0 + 215; 3]);
    // The no-signal 20 ms after sig-end is a duplicate and moves nothing.
    r.until(T0 + 1_500);
    let first_data = r
        .out
        .iter()
        .find(|(_, i)| matches!(i, Ifp::Data { .. }))
        .map(|(t, _)| *t);
    assert_eq!(
        first_data,
        Some(T0 + 215 + 1_000),
        "flags 1 s after the preamble"
    );
}

#[test]
fn f1_01_a_no_signal_indicator_alone_also_ends_the_wait() {
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0);
    r.rx(T0 + 160, &Ifp::Indicator(Indicator::NoSignal));
    r.until(T0 + 400);
    assert_eq!(r.preamble_instants(), vec![T0 + 160 + 75; 3]);
}

#[test]
fn f1_01_a_new_non_v21_indicator_also_ends_the_wait() {
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0);
    r.rx(T0 + 130, &Ifp::Indicator(Indicator::V17_14400LongTraining));
    r.until(T0 + 400);
    assert_eq!(r.preamble_instants(), vec![T0 + 130 + 75; 3]);
}

#[test]
fn f1_01_combined_fcs_ok_sig_end_is_unchanged_75_ms_after_the_frame() {
    // SpanDSP's pattern: nothing to wait for.
    let mut r = Run::after_dis(cfg());
    r.rx(T0 - 100, &Ifp::Indicator(Indicator::V21Preamble));
    for (i, b) in DCS.iter().enumerate() {
        r.rx(T0 - 60 + 10 * i as u64, &data(FieldType::HdlcData, &[*b]));
    }
    r.rx(T0, &data(FieldType::HdlcFcsOkSigEnd, &[]));
    r.until(T0 + 300);
    assert_eq!(r.preamble_instants(), vec![T0 + 75; 3]);
}

#[test]
fn f1_01_negative_control_without_the_wait_the_old_behaviour_talks_over_the_peer() {
    let mut c = cfg();
    c.pacing.peer_signal_wait_ms = 0;
    let mut r = Run::after_dis(c);
    r.dcs_frame_ends_at(T0);
    r.rx(T0 + 140, &data(FieldType::HdlcSigEnd, &[]));
    r.until(T0 + 400);
    // 65 ms before the peer's sig-end: exactly the field failure (25.649 -> 25.724).
    assert_eq!(r.preamble_instants(), vec![T0 + 75; 3]);
}

#[test]
fn f1_02_peer_that_never_ends_its_signal_is_answered_after_the_fallback() {
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0);
    r.until(T0 + 2_000);
    // 500 ms fallback, then the 75 ms turnaround.
    assert_eq!(r.preamble_instants(), vec![T0 + 500 + 75; 3]);
    let log = r.eng.drain_log().join("\n");
    assert!(log.contains("response held"), "{log}");
    assert!(log.contains("answering anyway"), "{log}");
}

#[test]
fn f1_02_the_fallback_is_configurable() {
    let mut c = cfg();
    c.pacing.peer_signal_wait_ms = 200;
    let mut r = Run::after_dis(c);
    r.dcs_frame_ends_at(T0);
    r.until(T0 + 1_000);
    assert_eq!(r.preamble_instants(), vec![T0 + 200 + 75; 3]);
    assert_eq!(Pacing::default().peer_signal_wait_ms, 500);
    assert_eq!(Pacing::none().peer_signal_wait_ms, 0);
}

#[test]
fn f1_02_next_wakeup_covers_the_fallback_deadline() {
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0);
    let w = r.eng.next_wakeup_ms().expect("a wakeup");
    assert!(
        w <= T0 + 500,
        "wakeup {w} must not be later than the fallback"
    );
}

#[test]
fn f1_02_a_newer_response_supersedes_a_held_one_and_t30_is_not_left_waiting() {
    // The peer repeats its DCS while our CFR is still held: one CFR leaves, once.
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0);
    r.dcs_frame_ends_at(T0 + 300);
    r.rx(T0 + 320, &data(FieldType::HdlcSigEnd, &[]));
    r.until(T0 + 3_000);
    assert_eq!(r.preamble_instants(), vec![T0 + 320 + 75; 3]);
    assert_eq!(r.eng.state(), t30::State::WaitImage, "burst completed");
}

#[test]
fn f1_02_abort_discards_a_held_response() {
    let mut r = Run::after_dis(cfg());
    r.dcs_frame_ends_at(T0);
    r.eng.abort(T0 + 10);
    r.until(T0 + 2_000);
    assert!(r.out.is_empty(), "{:?}", r.out);
}
