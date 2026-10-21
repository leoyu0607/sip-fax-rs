//! F1-03: a repeated command restarts the wait timer (field case F1, root cause 2).
//!
//! The sender repeats MPS / EOP / EOM when it did not hear our MCF / RTN / RTP. We repeat the
//! response and T2 must start over once that response has left; before 0.1.1 it kept running
//! from the first response and expired while the sender was still repeating (`t2_expired_image`).
//! Every instant below is on the virtual clock of `Sim`.

mod common;

use common::*;
use t30::{Config, Event, ResultCode, State, Timer};

const MCF: u8 = 0x31;
const RTN: u8 = 0x32;
const MPS: u8 = 0x72 | 0x80;
const EOP: u8 = 0x74 | 0x80;
const EOM: u8 = 0x71 | 0x80;

fn page(s: &mut Sim, seed: u32) {
    for c in encode_mh(&test_bitmap(20, seed), 20).chunks(100) {
        s.send(Event::ImageData(c.to_vec()));
    }
    s.send(Event::DataEnd);
}

/// Page 1 received at the current instant, MPS answered with MCF, MCF fully sent at `t`.
fn mcf_sent_at(t: u64) -> Sim {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    page(&mut s, 1);
    s.send(simple_cmd(MPS));
    s.at(t);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitImage);
    assert_eq!(s.deadline(Timer::T2), Some(t + 6_000));
    s
}

#[test]
fn f1_03_repeated_mps_resends_mcf_and_restarts_t2() {
    let mut s = mcf_sent_at(1_000); // T2 would expire at 7000
    s.at(4_100); // the sender repeats after its T4 (3 s)
    let acts = s.send(simple_cmd(MPS));
    assert_eq!(summarize(&acts), ["cancel T2", "tx[31]"]);
    assert_eq!(s.sent_fcfs().last().unwrap(), &(4_100, vec![MCF]));
    assert_eq!(
        s.deadline(Timer::T2),
        None,
        "no T2 while the response goes out"
    );
    s.at(5_500);
    let acts = s.send(Event::TxComplete);
    assert_eq!(summarize(&acts), ["start T2=6000"]);
    assert_eq!(s.deadline(Timer::T2), Some(11_500));
    // The original T2 instant passes without effect ...
    s.at(7_100);
    assert_eq!(s.state(), State::WaitImage);
    // ... and the next page is received normally.
    page(&mut s, 2);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages.len(), 2);
}

#[test]
fn f1_03_t2_expires_exactly_6_s_after_the_last_resend() {
    let mut s = mcf_sent_at(1_000);
    for (repeat_at, done_at) in [(4_100u64, 5_500u64), (8_600, 10_000)] {
        s.at(repeat_at);
        s.send(simple_cmd(MPS));
        s.at(done_at);
        s.send(Event::TxComplete);
        assert_eq!(s.deadline(Timer::T2), Some(done_at + 6_000));
    }
    s.at(10_000 + 5_999);
    assert_eq!(s.state(), State::WaitImage);
    s.at(10_000 + 6_000);
    assert_eq!(s.state(), State::SendingDcn);
    s.send(Event::TxComplete);
    assert_eq!(s.result.as_ref().unwrap().code, ResultCode::T2ExpiredImage);
    assert_eq!(s.result.as_ref().unwrap().pages, 1);
}

#[test]
fn f1_03_repeated_eop_resends_mcf_and_restarts_t2_while_waiting_for_dcn() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    page(&mut s, 1);
    s.send(simple_cmd(EOP));
    s.at(1_000);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitDcn);
    assert_eq!(s.deadline(Timer::T2), Some(7_000));
    s.at(4_100);
    let acts = s.send(simple_cmd(EOP));
    assert_eq!(summarize(&acts), ["cancel T2", "tx[31]"]);
    s.at(5_500);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T2), Some(11_500));
    s.at(11_499);
    assert!(!s.rx.is_done());
    s.at(11_500);
    assert_eq!(s.result.as_ref().unwrap().code, ResultCode::Ok);
}

#[test]
fn f1_03_repeated_eom_after_rtn_resends_rtn_and_restarts_t2() {
    // An empty page is rejected (RTN), which leaves the machine waiting for a retrain.
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    s.send(Event::DataEnd);
    s.send(simple_cmd(EOM));
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![RTN]);
    s.at(1_000);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitRetrain);
    assert_eq!(s.deadline(Timer::T2), Some(7_000));
    s.at(4_100);
    let acts = s.send(simple_cmd(EOM));
    assert_eq!(summarize(&acts), ["cancel T2", "tx[32]"]);
    s.at(5_500);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T2), Some(11_500));
    // A DCS then retrains as usual.
    s.at(6_000);
    s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(s.state(), State::WaitTcf);
}

#[test]
fn f1_03_a_resend_that_is_overtaken_by_the_next_page_does_not_arm_t2_in_the_page() {
    let mut s = mcf_sent_at(1_000);
    s.at(4_100);
    s.send(simple_cmd(MPS)); // MCF repeated, still going out ...
    page(&mut s, 2); // ... but the sender heard the first one after all
    assert_eq!(s.state(), State::WaitPostPage);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages.len(), 2);
    let acts = s.send(Event::TxComplete); // the resend's completion
    let acts2 = s.send(Event::TxComplete); // and the EOP's MCF
    assert!(summarize(&acts).is_empty(), "{acts:?}");
    assert_eq!(summarize(&acts2), ["start T2=6000"]);
    assert_eq!(s.state(), State::WaitDcn);
}

#[test]
fn f1_03_repeated_dcs_after_cfr_restarts_t2_by_retraining() {
    // Review of the other states: a repeated DCS is a new training, not a repeated response, and
    // already restarts T2 (the TCF wait) from the instant of the DCS.
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    s.at(3_000);
    s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(s.state(), State::WaitTcf);
    assert_eq!(s.deadline(Timer::T2), Some(9_000));
}

#[test]
fn f1_03_repeated_dcs_after_ftt_restarts_t2() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0xFFu8; 100]));
    s.send(Event::DataEnd);
    s.at(1_000);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitRetrain);
    assert_eq!(s.deadline(Timer::T2), Some(7_000));
    s.at(4_000);
    s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(s.state(), State::WaitTcf);
    assert_eq!(s.deadline(Timer::T2), Some(10_000));
}

#[test]
fn f1_03_crp_resend_restarts_t2_like_any_repeated_response() {
    let mut s = mcf_sent_at(1_000);
    s.at(3_000);
    let acts = s.send(simple_cmd(0x58)); // CRP
    assert_eq!(summarize(&acts), ["cancel T2", "tx[31]"]);
    s.at(4_500);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T2), Some(10_500));
}

#[test]
fn f1_03_a_repeated_command_in_phase_b_leaves_t4_alone() {
    // The DIS retransmission timer (T4) is a retransmission rhythm, not a wait for the reply to
    // a response: a CRP does not move it (pinned by R01).
    let mut s = after_dis(Config::default());
    let t4 = s.deadline(Timer::T4);
    s.at(500);
    let acts = s.send(simple_cmd(0x58));
    assert_eq!(summarize(&acts), ["tx[01]"]);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), t4);
}
