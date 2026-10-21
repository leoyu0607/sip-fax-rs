//! E05: table-driven test of the ECM states against every event kind (companion of R01, which
//! covers the non-ECM procedure).
//!
//! Each context is reached by replaying a short script from a fresh receiver; the script ends in
//! the `State` named in the table key's context label. For every (context, event) pair
//! `EXPECTED` lists the next state and the actions (logs are not compared); every pair not
//! listed must be ignored: same state, no actions at all, timers untouched.
//!
//! The rows were checked one by one against T.30 Annex A (see the comments); the table is a
//! literal, not output of the code.

mod common;

use common::*;
use std::collections::BTreeMap;
use t30::{Event, RxSignal, State, Timer};

const FRAMES: usize = 3;

fn coded() -> Vec<Vec<u8>> {
    // a page that needs more than FRAMES frames of 64 octets; only the first FRAMES are used,
    // which is enough to drive the machine (the page content is irrelevant here).
    let bm = sparse_bitmap(200, 1);
    let mut f = split_frames(&encode_mmr(&bm), 64);
    assert!(f.len() >= FRAMES);
    f.truncate(FRAMES);
    f
}

fn base() -> Sim {
    to_ecm_wait_block(ecm_config(), &ecm_dcs(true, true, false))
}

/// (label, expected state, script)
fn contexts() -> Vec<(&'static str, State)> {
    vec![
        ("WaitTcf", State::WaitTcf),
        ("SendingCfr", State::SendingCfr),
        ("WaitBlock", State::EcmWaitBlock),
        ("WaitBlockHeld", State::EcmWaitBlock),
        ("WaitBlockAfterMcf", State::EcmWaitBlock),
        ("RxBlock", State::EcmRxBlock),
        ("WaitPps", State::EcmWaitPps),
        ("WaitPpsPartial", State::EcmWaitPps),
        ("SendingMcfNull", State::SendingPostPage),
        ("SendingPpr", State::SendingPostPage),
        ("WaitDcn", State::WaitDcn),
    ]
}

fn setup(label: &str) -> Sim {
    let f = coded();
    let all: Vec<usize> = (0..FRAMES).collect();
    match label {
        "WaitTcf" => {
            let mut s = after_dis(ecm_config());
            s.send(frame_ev(ecm_dcs(true, true, false).frame()));
            s
        }
        "SendingCfr" => {
            let mut s = after_dis(ecm_config());
            s.send(frame_ev(ecm_dcs(true, true, false).frame()));
            s.send(Event::ImageData(vec![0; 2700]));
            s.send(Event::DataEnd);
            s
        }
        "WaitBlock" => base(),
        "WaitBlockHeld" => {
            let mut s = base();
            send_fcds(&mut s, &f, &[0, 2]);
            s.send(frame_ev(pps(Q_NULL, 0, 0, FRAMES)));
            s.send(Event::TxComplete);
            s
        }
        "WaitBlockAfterMcf" => {
            let mut s = base();
            send_fcds(&mut s, &f, &all);
            s.send(frame_ev(pps(Q_NULL, 0, 0, FRAMES)));
            s.send(Event::TxComplete);
            s
        }
        "RxBlock" => {
            let mut s = base();
            s.send(frame_ev(fcd(0, &f[0])));
            s
        }
        "WaitPps" => {
            let mut s = base();
            send_fcds(&mut s, &f, &all);
            s
        }
        "WaitPpsPartial" => {
            let mut s = base();
            send_fcds(&mut s, &f, &[0, 2]);
            s
        }
        "SendingMcfNull" => {
            let mut s = setup("WaitPps");
            s.send(frame_ev(pps(Q_NULL, 0, 0, FRAMES)));
            s
        }
        "SendingPpr" => {
            let mut s = setup("WaitPpsPartial");
            s.send(frame_ev(pps(Q_NULL, 0, 0, FRAMES)));
            s
        }
        "WaitDcn" => {
            let mut s = setup("WaitPps");
            s.send(frame_ev(pps(Q_EOP, 0, 0, FRAMES)));
            s.send(Event::TxComplete);
            s
        }
        other => panic!("no script for {other}"),
    }
}

fn events() -> Vec<(&'static str, Event)> {
    let f = coded();
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
        ("ImageData", Event::ImageData(vec![0; 8])),
        ("DataEnd", Event::DataEnd),
        (
            "Frame:DCS-ECM",
            frame_ev(ecm_dcs(true, true, false).frame()),
        ),
        ("Frame:DCS", frame_ev(DcsSpec::default().frame())),
        ("Frame:TSI", frame_ev(tsi_frame("77"))),
        ("Frame:FCD", frame_ev(fcd(1, &f[1]))),
        ("Frame:FCD-badFCS", bad_frame_ev(fcd(1, &f[1]))),
        ("Frame:RCP", frame_ev(rcp())),
        ("Frame:PPS-NULL", frame_ev(pps(Q_NULL, 0, 0, FRAMES))),
        ("Frame:PPS-EOP", frame_ev(pps(Q_EOP, 0, 0, FRAMES))),
        ("Frame:PPS-EOM", frame_ev(pps(Q_EOM, 0, 0, FRAMES))),
        ("Frame:EOR-NULL", frame_ev(eor(Q_NULL))),
        ("Frame:EOR-EOP", frame_ev(eor(Q_EOP))),
        ("Frame:CTC", frame_ev(ctc(&[11]))),
        ("Frame:RR", frame_ev(rr())),
        ("Frame:DCN", simple_cmd(0xDF)),
        ("Frame:CRP", simple_cmd(0x58)),
        ("Frame:MPS(non-ECM)", simple_cmd(0xF2)),
    ]
}

fn run_matrix() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (label, _) in contexts() {
        for (name, ev) in events() {
            let mut s = setup(label);
            let acts = s.send(ev);
            m.insert(
                format!("{label}/{name}"),
                format!("{:?} | {}", s.state(), summarize(&acts).join("; ")),
            );
        }
    }
    m
}

/// `(context/event, next state, actions)` for every pair that is not ignored.
///
/// Reading guide (T.30 Annex A, called terminal):
/// * FCD frames open / fill a block; RCP, the end of the signal or silence close it; the PPS (or
///   CTC / EOR) that follows is answered MCF, PPR, CTR or ERR. A block is acknowledged only when
///   every frame announced by the PPS is held.
/// * After PPR, CTR and MCF-for-NULL the machine waits for the next frames (T2); after MCF for
///   EOP it waits for DCN.
/// * A command repeated without new frames in between gets the previous answer again.
/// * DCN ends the call without a reply; a DCS (retrain) restarts the training; non-ECM post
///   page commands (MPS, EOP, EOM) are no ECM commands: unexpected frames, tolerated three times.
const EXPECTED: &[(&str, &str, &str)] = &[
    // ---------------------------------------------------------------- WaitBlock
    ("WaitBlock/Abort", "Done", "cancel T2; done aborted"),
    ("WaitBlock/Frame:CRP", "EcmWaitBlock", "cancel T2; tx[21]"),
    (
        "WaitBlock/Frame:CTC",
        "SendingPostPage",
        "cancel T2; tx[23]",
    ),
    (
        "WaitBlock/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    // a new DCS restarts the training: T2 is simply started again
    ("WaitBlock/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitBlock/Frame:DCS-ECM", "WaitTcf", "start T2=6000"),
    // RR is a status request: the last response (CFR) goes out again
    ("WaitBlock/Frame:RR", "EcmWaitBlock", "cancel T2; tx[21]"),
    (
        "WaitBlock/Frame:EOR-EOP",
        "SendingPostPage",
        "cancel T2; tx[38]",
    ),
    (
        "WaitBlock/Frame:EOR-NULL",
        "SendingPostPage",
        "cancel T2; tx[38]",
    ),
    (
        "WaitBlock/Frame:FCD",
        "EcmRxBlock",
        "cancel T2; start Data=15000",
    ),
    (
        "WaitBlock/Frame:PPS-EOM",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitBlock/Frame:PPS-EOP",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitBlock/Frame:PPS-NULL",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    ("WaitBlock/T2", "SendingDcn", "tx[5F]"),
    // ---------------------------------------------------------------- WaitBlockHeld
    ("WaitBlockHeld/Abort", "Done", "cancel T2; done aborted"),
    (
        "WaitBlockHeld/Frame:CRP",
        "EcmWaitBlock",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitBlockHeld/Frame:CTC",
        "SendingPostPage",
        "cancel T2; tx[23]",
    ),
    (
        "WaitBlockHeld/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitBlockHeld/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitBlockHeld/Frame:DCS-ECM", "WaitTcf", "start T2=6000"),
    (
        "WaitBlockHeld/Frame:EOR-EOP",
        "SendingPostPage",
        "cancel T2; page#1 rows=12 bad=0; tx[38]",
    ),
    (
        "WaitBlockHeld/Frame:EOR-NULL",
        "SendingPostPage",
        "cancel T2; tx[38]",
    ),
    (
        "WaitBlockHeld/Frame:FCD",
        "EcmRxBlock",
        "cancel T2; start Data=15000",
    ),
    (
        "WaitBlockHeld/Frame:PPS-EOM",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitBlockHeld/Frame:PPS-EOP",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    // the PPS of the block just answered with PPR, repeated: PPR again (the first was lost)
    (
        "WaitBlockHeld/Frame:PPS-NULL",
        "EcmWaitBlock",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitBlockHeld/Frame:RR",
        "EcmWaitBlock",
        "cancel T2; tx[3D]",
    ),
    ("WaitBlockHeld/T2", "SendingDcn", "tx[5F]"),
    // ---------------------------------------------------------------- WaitBlockAfterMcf
    ("WaitBlockAfterMcf/Abort", "Done", "cancel T2; done aborted"),
    (
        "WaitBlockAfterMcf/Frame:CRP",
        "EcmWaitBlock",
        "cancel T2; tx[31]",
    ),
    (
        "WaitBlockAfterMcf/Frame:CTC",
        "SendingPostPage",
        "cancel T2; tx[23]",
    ),
    (
        "WaitBlockAfterMcf/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitBlockAfterMcf/Frame:DCS", "WaitTcf", "start T2=6000"),
    (
        "WaitBlockAfterMcf/Frame:DCS-ECM",
        "WaitTcf",
        "start T2=6000",
    ),
    (
        "WaitBlockAfterMcf/Frame:EOR-EOP",
        "SendingPostPage",
        "cancel T2; page#1 rows=33 bad=0; tx[38]",
    ),
    (
        "WaitBlockAfterMcf/Frame:EOR-NULL",
        "SendingPostPage",
        "cancel T2; tx[38]",
    ),
    (
        "WaitBlockAfterMcf/Frame:FCD",
        "EcmRxBlock",
        "cancel T2; start Data=15000",
    ),
    (
        "WaitBlockAfterMcf/Frame:PPS-EOM",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitBlockAfterMcf/Frame:PPS-EOP",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    // the PPS of the block just acknowledged, repeated: MCF again, no second delivery
    (
        "WaitBlockAfterMcf/Frame:PPS-NULL",
        "EcmWaitBlock",
        "cancel T2; tx[31]",
    ),
    (
        "WaitBlockAfterMcf/Frame:RR",
        "EcmWaitBlock",
        "cancel T2; tx[31]",
    ),
    ("WaitBlockAfterMcf/T2", "SendingDcn", "tx[5F]"),
    // ---------------------------------------------------------------- RxBlock
    ("RxBlock/Abort", "Done", "cancel Data; done aborted"),
    (
        "RxBlock/Frame:CTC",
        "SendingPostPage",
        "cancel Data; tx[23]",
    ),
    (
        "RxBlock/Frame:DCN",
        "Done",
        "cancel Data; done remote_disconnect",
    ),
    ("RxBlock/Frame:DCS", "WaitTcf", "cancel Data; start T2=6000"),
    (
        "RxBlock/Frame:DCS-ECM",
        "WaitTcf",
        "cancel Data; start T2=6000",
    ),
    (
        "RxBlock/Frame:EOR-EOP",
        "SendingPostPage",
        "cancel Data; page#1 rows=12 bad=0; tx[38]",
    ),
    (
        "RxBlock/Frame:EOR-NULL",
        "SendingPostPage",
        "cancel Data; tx[38]",
    ),
    ("RxBlock/Frame:FCD", "EcmRxBlock", "start Data=15000"),
    (
        "RxBlock/Frame:PPS-EOM",
        "SendingPostPage",
        "cancel Data; tx[3D]",
    ),
    (
        "RxBlock/Frame:PPS-EOP",
        "SendingPostPage",
        "cancel Data; tx[3D]",
    ),
    (
        "RxBlock/Frame:PPS-NULL",
        "SendingPostPage",
        "cancel Data; tx[3D]",
    ),
    (
        "RxBlock/Frame:RCP",
        "EcmWaitPps",
        "cancel Data; start T2=6000",
    ),
    (
        "RxBlock/Sig:NoSignal",
        "EcmWaitPps",
        "cancel Data; start T2=6000",
    ),
    ("RxBlock/TData", "EcmWaitPps", "start T2=6000"),
    // ---------------------------------------------------------------- WaitPps
    ("WaitPps/Abort", "Done", "cancel T2; done aborted"),
    ("WaitPps/Frame:CRP", "EcmWaitPps", "cancel T2; tx[21]"),
    ("WaitPps/Frame:CTC", "SendingPostPage", "cancel T2; tx[23]"),
    (
        "WaitPps/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitPps/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitPps/Frame:DCS-ECM", "WaitTcf", "start T2=6000"),
    (
        "WaitPps/Frame:EOR-EOP",
        "SendingPostPage",
        "cancel T2; page#1 rows=33 bad=0; tx[38]",
    ),
    (
        "WaitPps/Frame:EOR-NULL",
        "SendingPostPage",
        "cancel T2; tx[38]",
    ),
    (
        "WaitPps/Frame:PPS-EOM",
        "SendingPostPage",
        "cancel T2; page#1 rows=33 bad=0; tx[31]",
    ),
    (
        "WaitPps/Frame:PPS-EOP",
        "SendingPostPage",
        "cancel T2; page#1 rows=33 bad=0; tx[31]",
    ),
    (
        "WaitPps/Frame:PPS-NULL",
        "SendingPostPage",
        "cancel T2; tx[31]",
    ),
    ("WaitPps/T2", "SendingDcn", "tx[5F]"),
    // ---------------------------------------------------------------- WaitPpsPartial
    ("WaitPpsPartial/Abort", "Done", "cancel T2; done aborted"),
    (
        "WaitPpsPartial/Frame:CRP",
        "EcmWaitPps",
        "cancel T2; tx[21]",
    ),
    (
        "WaitPpsPartial/Frame:CTC",
        "SendingPostPage",
        "cancel T2; tx[23]",
    ),
    (
        "WaitPpsPartial/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitPpsPartial/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitPpsPartial/Frame:DCS-ECM", "WaitTcf", "start T2=6000"),
    (
        "WaitPpsPartial/Frame:EOR-EOP",
        "SendingPostPage",
        "cancel T2; page#1 rows=12 bad=0; tx[38]",
    ),
    (
        "WaitPpsPartial/Frame:EOR-NULL",
        "SendingPostPage",
        "cancel T2; tx[38]",
    ),
    (
        "WaitPpsPartial/Frame:PPS-EOM",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitPpsPartial/Frame:PPS-EOP",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    (
        "WaitPpsPartial/Frame:PPS-NULL",
        "SendingPostPage",
        "cancel T2; tx[3D]",
    ),
    ("WaitPpsPartial/T2", "SendingDcn", "tx[5F]"),
    // ---------------------------------------------------------------- SendingMcfNull / SendingPpr
    ("SendingMcfNull/Abort", "Done", "done aborted"),
    ("SendingMcfNull/Frame:DCN", "Done", "done remote_disconnect"),
    ("SendingMcfNull/TxComplete", "EcmWaitBlock", "start T2=6000"),
    ("SendingPpr/Abort", "Done", "done aborted"),
    ("SendingPpr/Frame:DCN", "Done", "done remote_disconnect"),
    ("SendingPpr/TxComplete", "EcmWaitBlock", "start T2=6000"),
    // ---------------------------------------------------------------- WaitDcn
    ("WaitDcn/Abort", "Done", "cancel T2; done aborted"),
    ("WaitDcn/Frame:CRP", "WaitDcn", "cancel T2; tx[31]"),
    ("WaitDcn/Frame:DCN", "Done", "cancel T2; done ok"),
    ("WaitDcn/Frame:PPS-EOP", "WaitDcn", "cancel T2; tx[31]"),
    ("WaitDcn/Frame:RR", "WaitDcn", "cancel T2; tx[31]"),
    ("WaitDcn/T2", "Done", "done ok"),
    // ---------------------------------------------------------------- WaitTcf / SendingCfr (ECM DCS)
    ("WaitTcf/Abort", "Done", "cancel T2; done aborted"),
    ("WaitTcf/DataEnd", "SendingFtt", "cancel T2; tx[22]"),
    ("WaitTcf/Frame:CRP", "WaitTcf", "cancel T2; tx[01]"),
    (
        "WaitTcf/Frame:DCN",
        "Done",
        "cancel T2; done remote_disconnect",
    ),
    ("WaitTcf/Frame:DCS", "WaitTcf", "start T2=6000"),
    ("WaitTcf/Frame:DCS-ECM", "WaitTcf", "start T2=6000"),
    ("WaitTcf/ImageData", "RxTcf", "cancel T2; start Data=15000"),
    ("WaitTcf/T2", "SendingFtt", "tx[22]"),
    ("SendingCfr/Abort", "Done", "done aborted"),
    ("SendingCfr/Frame:DCN", "Done", "done remote_disconnect"),
    ("SendingCfr/TxComplete", "EcmWaitBlock", "start T2=6000"),
];

#[test]
fn e05_every_ecm_state_against_every_event() {
    let matrix = run_matrix();
    assert_eq!(matrix.len(), contexts().len() * events().len());
    let expected: BTreeMap<&str, (&str, &str)> =
        EXPECTED.iter().map(|(k, n, a)| (*k, (*n, *a))).collect();
    assert_eq!(expected.len(), EXPECTED.len(), "no duplicate rows");
    if std::env::var_os("E05_DUMP").is_some() {
        for (k, v) in &matrix {
            let ctx = k.split('/').next().unwrap();
            let state = contexts().iter().find(|(l, _)| *l == ctx).unwrap().1;
            if v != &format!("{state:?} | ") {
                eprintln!(
                    "    (\"{k}\", \"{}\", \"{}\"),",
                    v.split(" | ").next().unwrap(),
                    v.split(" | ").nth(1).unwrap()
                );
            }
        }
    }
    let mut bad = Vec::new();
    for (key, got) in &matrix {
        let ctx = key.split('/').next().unwrap();
        let state = contexts().iter().find(|(l, _)| *l == ctx).unwrap().1;
        let want = match expected.get(key.as_str()) {
            Some((n, a)) => format!("{n} | {a}"),
            None => format!("{state:?} | "),
        };
        if got != &want {
            bad.push(format!("{key}: got `{got}`, table says `{want}`"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} cells differ:\n{}",
        bad.len(),
        bad.join("\n")
    );
    for key in expected.keys() {
        assert!(matrix.contains_key(*key), "table row {key} names no cell");
    }
}

#[test]
fn e05_the_script_for_every_context_ends_in_its_state() {
    for (label, state) in contexts() {
        assert_eq!(setup(label).state(), state, "{label}");
    }
}

#[test]
fn e05_ignored_cells_leave_the_machine_untouched_even_when_repeated() {
    for (label, state) in contexts() {
        for (name, ev) in events() {
            let key = format!("{label}/{name}");
            if EXPECTED.iter().any(|(k, _, _)| *k == key) {
                continue;
            }
            let mut s = setup(label);
            let timers = s.timers.clone();
            s.send(ev.clone());
            s.send(ev);
            assert_eq!(s.state(), state, "{key}");
            assert_eq!(s.timers, timers, "{key}");
        }
    }
}
