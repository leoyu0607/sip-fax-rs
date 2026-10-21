//! R01: table-driven test of every state against every event kind.
//!
//! Each state is reached by replaying a short script from a fresh receiver. For every
//! (state, event) pair `EXPECTED` lists the next state and the actions (logs are not
//! compared); every pair not listed must be ignored: same state, no actions at all.

mod common;

use common::*;
use std::collections::BTreeMap;
use t30::{Config, Event, RxSignal, State, Timer};

fn mh20() -> Vec<u8> {
    encode_mh(&test_bitmap(20, 3), 20)
}

fn setup(state: State) -> Sim {
    let cfg = Config::default();
    let dcs = DcsSpec::default();
    match state {
        State::Idle => Sim::new(cfg),
        State::SendingDis => started(cfg),
        State::WaitCommand => after_dis(cfg),
        State::WaitTcf => {
            let mut s = after_dis(cfg);
            s.send(frame_ev(dcs.frame()));
            s
        }
        State::RxTcf => {
            let mut s = setup_with(State::WaitTcf);
            s.send(Event::ImageData(vec![0; 100]));
            s
        }
        State::SendingCfr => {
            let mut s = setup_with(State::WaitTcf);
            s.send(Event::ImageData(vec![0; 2700]));
            s.send(Event::DataEnd);
            s
        }
        State::SendingFtt => {
            let mut s = setup_with(State::WaitTcf);
            s.send(Event::DataEnd);
            s
        }
        State::WaitRetrain => {
            let mut s = setup_with(State::SendingFtt);
            s.send(Event::TxComplete);
            s
        }
        State::WaitImage => {
            let mut s = setup_with(State::SendingCfr);
            s.send(Event::TxComplete);
            s
        }
        State::RxImage => {
            let mut s = setup_with(State::WaitImage);
            let d = mh20();
            s.send(Event::ImageData(d[..40].to_vec()));
            s
        }
        State::WaitPostPage => {
            let mut s = setup_with(State::WaitImage);
            s.send(Event::ImageData(mh20()));
            s.send(Event::DataEnd);
            s
        }
        State::SendingPostPage => {
            let mut s = setup_with(State::WaitPostPage);
            s.send(simple_cmd(0xF2)); // MPS
            s
        }
        State::WaitDcn => {
            let mut s = setup_with(State::WaitPostPage);
            s.send(simple_cmd(0xF4)); // EOP
            s.send(Event::TxComplete);
            s
        }
        State::SendingDcn => {
            let mut s = after_dis(cfg);
            let ecm = DcsSpec {
                extra: vec![27],
                ..DcsSpec::default()
            };
            s.send(frame_ev(ecm.frame()));
            s
        }
        State::Done => {
            let mut s = started(cfg);
            s.send(Event::Abort);
            s
        }
    }
}

fn setup_with(state: State) -> Sim {
    let s = setup(state);
    assert_eq!(s.state(), state, "script for {state:?}");
    s
}

fn events() -> Vec<(&'static str, Event)> {
    vec![
        ("Start", Event::Start),
        ("TxComplete", Event::TxComplete),
        ("T1", Event::Timer(Timer::T1)),
        ("T2", Event::Timer(Timer::T2)),
        ("T4", Event::Timer(Timer::T4)),
        ("TData", Event::Timer(Timer::Data)),
        ("Abort", Event::Abort),
        ("Sig:NoSignal", Event::Signal(RxSignal::NoSignal)),
        ("Sig:Training", Event::Signal(RxSignal::Training)),
        ("Sig:V21", Event::Signal(RxSignal::V21Preamble)),
        ("Sig:Cng", Event::Signal(RxSignal::Cng)),
        ("ImageData", Event::ImageData(vec![0x00; 8])),
        ("DataEnd", Event::DataEnd),
        ("Frame:DCS", frame_ev(DcsSpec::default().frame())),
        (
            "Frame:DCS-ECM",
            frame_ev(
                DcsSpec {
                    extra: vec![27],
                    ..DcsSpec::default()
                }
                .frame(),
            ),
        ),
        ("Frame:TSI", frame_ev(tsi_frame("77"))),
        ("Frame:MPS", simple_cmd(0xF2)),
        ("Frame:EOP", simple_cmd(0xF4)),
        ("Frame:EOM", simple_cmd(0xF1)),
        ("Frame:DCN", simple_cmd(0xDF)),
        ("Frame:CRP", simple_cmd(0x58)),
        ("Frame:CSA(unexpected)", simple_cmd(0xA4)),
        ("Frame:bad-FCS", bad_frame_ev(DcsSpec::default().frame())),
    ]
}

/// Runs the whole matrix and returns `state/event -> "Next | action; action"`.
fn run_matrix() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for state in State::ALL {
        for (name, ev) in events() {
            let mut s = setup_with(state);
            let acts = s.send(ev);
            let sum = summarize(&acts);
            m.insert(
                format!("{state:?}/{name}"),
                format!("{:?} | {}", s.state(), sum.join("; ")),
            );
        }
    }
    m
}

/// `(state/event, next state, actions)` for every pair that is not ignored.
///
/// Reading guide (T.30 section 5 flow charts of the called terminal):
/// * `Idle` only reacts to Start and Abort.
/// * DIS is sent at Start and retransmitted by T4; a valid DCS cancels T1 and T4.
/// * TCF: data cancels T2 and starts the idle timer; the end gives CFR (pass) or FTT (fail).
/// * Image: post-page commands give MCF / RTP / RTN (here RTN: the 20-row page is
///   still incomplete when the command arrives while receiving, and MCF after a whole page).
/// * DCN ends the call from every active state without any reply; failures send DCN first.
/// * Done ignores everything.
const EXPECTED: &[(&str, &str, &str)] = &[
    ("Idle/Abort", "Done", "done aborted"),
    (
        "Idle/Start",
        "SendingDis",
        "start T1=35000; signal Ced; tx[01]",
    ),
    ("RxImage/Abort", "Done", "cancel Data; done aborted"),
    (
        "RxImage/DataEnd",
        "WaitPostPage",
        "cancel Data; start T2=6000",
    ),
    (
        "RxImage/Frame:DCN",
        "Done",
        "cancel Data; done remote_disconnect",
    ),
    ("RxImage/Frame:DCS", "WaitTcf", "cancel Data; start T2=6000"),
    ("RxImage/Frame:DCS-ECM", "SendingDcn", "cancel Data; tx[5F]"),
    (
        "RxImage/Frame:EOM",
        "SendingPostPage",
        "cancel Data; tx[32]",
    ),
    (
        "RxImage/Frame:EOP",
        "SendingPostPage",
        "cancel Data; tx[32]",
    ),
    (
        "RxImage/Frame:MPS",
        "SendingPostPage",
        "cancel Data; tx[32]",
    ),
    ("RxImage/ImageData", "RxImage", "start Data=15000"),
    (
        "RxImage/Sig:NoSignal",
        "WaitPostPage",
        "cancel Data; start T2=6000",
    ),
    ("RxImage/TData", "WaitPostPage", "start T2=6000"),
    ("RxTcf/Abort", "Done", "cancel Data; done aborted"),
    ("RxTcf/DataEnd", "SendingFtt", "cancel Data; tx[22]"),
    (
        "RxTcf/Frame:DCN",
        "Done",
        "cancel Data; done remote_disconnect",
    ),
    ("RxTcf/Frame:DCS", "WaitTcf", "cancel Data; start T2=6000"),
    ("RxTcf/Frame:DCS-ECM", "SendingDcn", "cancel Data; tx[5F]"),
    ("RxTcf/ImageData", "RxTcf", "start Data=15000"),
    ("RxTcf/Sig:NoSignal", "SendingFtt", "cancel Data; tx[22]"),
    ("RxTcf/TData", "SendingFtt", "tx[22]"),
    ("SendingCfr/Abort", "Done", "done aborted"),
    ("SendingCfr/Frame:DCN", "Done", "done remote_disconnect"),
    ("SendingCfr/TxComplete", "WaitImage", "start T2=6000"),
    ("SendingDcn/Abort", "Done", "done aborted"),
    ("SendingDcn/TxComplete", "Done", "done incompatible_dcs"),
    ("SendingDis/Abort", "Done", "cancel T1; done aborted"),
    (
        "SendingDis/Frame:DCN",
        "Done",
        "cancel T1; done remote_disconnect",
    ),
    (
        "SendingDis/Frame:DCS",
        "WaitTcf",
        "cancel T1; start T2=6000",
    ),
    (
        "SendingDis/Frame:DCS-ECM",
        "SendingDcn",
        "cancel T1; tx[5F]",
    ),
    ("SendingDis/T1", "SendingDcn", "tx[5F]"),
    ("SendingDis/TxComplete", "WaitCommand", "start T4=3000"),
    ("SendingFtt/Abort", "Done", "done aborted"),
    ("SendingFtt/Frame:DCN", "Done", "done training_failed"),
    ("SendingFtt/TxComplete", "WaitRetrain", "start T2=6000"),
    ("SendingPostPage/Abort", "Done", "done aborted"),
    ("SendingPostPage/Frame:DCN", "Done", "done partial_dcn"),
    ("SendingPostPage/TxComplete", "WaitImage", "start T2=6000"),
    (
        "WaitCommand/Abort",
        "Done",
        "cancel T1; cancel T4; done aborted",
    ),
    ("WaitCommand/Frame:CRP", "WaitCommand", "tx[01]"),
    (
        "WaitCommand/Frame:DCN",
        "Done",
        "cancel T1; cancel T4; done remote_disconnect",
    ),
    (
        "WaitCommand/Frame:DCS",
        "WaitTcf",
        "cancel T1; cancel T4; start T2=6000",
    ),
    (
        "WaitCommand/Frame:DCS-ECM",
        "SendingDcn",
        "cancel T1; cancel T4; tx[5F]",
    ),
    ("WaitCommand/T1", "SendingDcn", "cancel T4; tx[5F]"),
    ("WaitCommand/T4", "SendingDis", "tx[01]"),
    ("WaitDcn/Abort", "Done", "cancel T2; done aborted"),
    ("WaitDcn/Frame:CRP", "WaitDcn", "tx[31]"),
    ("WaitDcn/Frame:DCN", "Done", "cancel T2; done ok"),
    ("WaitDcn/Frame:EOP", "WaitDcn", "tx[31]"),
    ("WaitDcn/T2", "Done", "done ok"),
    ("WaitImage/Abort", "Done", "cancel T2; done aborted"),
    ("WaitImage/DataEnd", "WaitPostPage", "start T2=6000"),
    ("WaitImage/Frame:CRP", "WaitImage", "tx[21]"),
    (
        "WaitImage/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitImage/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitImage/Frame:DCS-ECM", "SendingDcn", "cancel T2; tx[5F]"),
    (
        "WaitImage/ImageData",
        "RxImage",
        "cancel T2; start Data=15000",
    ),
    ("WaitImage/T2", "SendingDcn", "tx[5F]"),
    ("WaitPostPage/Abort", "Done", "cancel T2; done aborted"),
    ("WaitPostPage/Frame:CRP", "WaitPostPage", "tx[21]"),
    (
        "WaitPostPage/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitPostPage/Frame:DCS", "WaitTcf", "start T2=6000"),
    (
        "WaitPostPage/Frame:DCS-ECM",
        "SendingDcn",
        "cancel T2; tx[5F]",
    ),
    (
        "WaitPostPage/Frame:EOM",
        "SendingPostPage",
        "cancel T2; page#1 rows=20 bad=0; tx[31]",
    ),
    (
        "WaitPostPage/Frame:EOP",
        "SendingPostPage",
        "cancel T2; page#1 rows=20 bad=0; tx[31]",
    ),
    (
        "WaitPostPage/Frame:MPS",
        "SendingPostPage",
        "cancel T2; page#1 rows=20 bad=0; tx[31]",
    ),
    ("WaitPostPage/T2", "SendingDcn", "tx[5F]"),
    ("WaitRetrain/Abort", "Done", "cancel T2; done aborted"),
    ("WaitRetrain/Frame:CRP", "WaitRetrain", "tx[22]"),
    (
        "WaitRetrain/Frame:DCN",
        "Done",
        "cancel T2; done training_failed",
    ),
    ("WaitRetrain/Frame:DCS", "WaitTcf", "start T2=6000"),
    (
        "WaitRetrain/Frame:DCS-ECM",
        "SendingDcn",
        "cancel T2; tx[5F]",
    ),
    ("WaitRetrain/T2", "SendingDcn", "tx[5F]"),
    ("WaitTcf/Abort", "Done", "cancel T2; done aborted"),
    ("WaitTcf/DataEnd", "SendingFtt", "cancel T2; tx[22]"),
    ("WaitTcf/Frame:CRP", "WaitTcf", "tx[01]"),
    (
        "WaitTcf/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitTcf/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitTcf/Frame:DCS-ECM", "SendingDcn", "cancel T2; tx[5F]"),
    ("WaitTcf/ImageData", "RxTcf", "cancel T2; start Data=15000"),
    ("WaitTcf/T2", "SendingFtt", "tx[22]"),
];

#[test]
fn r01_every_state_against_every_event() {
    let matrix = run_matrix();
    assert_eq!(matrix.len(), State::ALL.len() * events().len());
    let expected: BTreeMap<&str, (&str, &str)> =
        EXPECTED.iter().map(|(k, n, a)| (*k, (*n, *a))).collect();
    assert_eq!(expected.len(), EXPECTED.len(), "no duplicate rows");
    for (key, got) in &matrix {
        let state = key.split('/').next().unwrap();
        let want = match expected.get(key.as_str()) {
            Some((n, a)) => format!("{n} | {a}"),
            None => format!("{state} | "), // ignored: same state, no action
        };
        assert_eq!(got, &want, "cell {key}");
    }
    for key in expected.keys() {
        assert!(matrix.contains_key(*key), "table row {key} names no cell");
    }
}

#[test]
fn r01_the_script_for_every_state_ends_in_that_state() {
    for state in State::ALL {
        assert_eq!(setup(state).state(), state);
    }
}

#[test]
fn r01_ignored_cells_leave_the_machine_untouched_even_when_repeated() {
    // Feeding an ignored event twice must not change anything either.
    for state in State::ALL {
        for (name, ev) in events() {
            let key = format!("{state:?}/{name}");
            if EXPECTED.iter().any(|(k, _, _)| *k == key) {
                continue;
            }
            let mut s = setup(state);
            let timers = s.timers.clone();
            s.send(ev.clone());
            s.send(ev);
            assert_eq!(s.state(), state, "{key}");
            assert_eq!(s.timers, timers, "{key}");
        }
    }
}
