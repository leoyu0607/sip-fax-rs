//! ECM receive (M3d): the called terminal's side of ITU-T T.30 Annex A, one behaviour per test.
//!
//! The expected octets are written from the text of T.30 (09/2005) Annex A and T.4 Annex A, not
//! produced by the crate: FCD `60`, RCP `61`, PPS `7D` with FCF2 `F2` for MPS and the counters
//! least significant bit first, PPR `3D` with a 256 bit map, CTC `48`, CTR `23`, EOR `73`, ERR `38`,
//! RR `76`.

mod common;

use common::*;
use t30::{Action, Config, Event, ResultCode, RxSignal, State, Timer};

const ROWS: usize = 400;

fn page(seed: u32) -> (Vec<u8>, Vec<u8>) {
    let bm = sparse_bitmap(ROWS, seed);
    let coded = encode_mmr(&bm);
    (bm, coded)
}

/// A machine in `EcmWaitBlock` for an MMR page with `size` octet frames.
fn setup(size: usize) -> Sim {
    to_ecm_wait_block(ecm_config(), &ecm_dcs(size == 64, true, false))
}

fn tx_names(s: &Sim) -> Vec<String> {
    summarize(&s.trace.iter().map(|(_, a)| a.clone()).collect::<Vec<_>>())
}

fn last_response(s: &Sim) -> u8 {
    last_tx(s)[0]
}

// ---------------------------------------------------------------------------- happy paths

#[test]
fn e01u_one_block_one_page_pps_eop_for_both_frame_sizes() {
    for size in [64usize, 256] {
        let (bm, coded) = page(3);
        let frames = split_frames(&coded, size);
        assert!(frames.len() > 1, "test page must need several frames");
        let mut s = setup(size);
        let all: Vec<usize> = (0..frames.len()).collect();
        send_fcds(&mut s, &frames, &all);
        assert_eq!(s.state(), State::EcmWaitPps, "size {size}");
        let acts = s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x31))
        );
        assert_eq!(s.pages.len(), 1, "size {size}");
        assert_eq!(s.pages[0].rows as usize, ROWS);
        assert_eq!(s.pages[0].bitmap, bm, "size {size}: pixel exact");
        assert_eq!(s.pages[0].bad_lines, 0);
        assert_eq!(s.pages[0].compression, t30::Compression::Mmr);
        s.send(Event::TxComplete);
        assert_eq!(s.state(), State::WaitDcn);
        s.send(frame_ev(final_frame(0xDF, &[])));
        let r = s.result.unwrap();
        assert_eq!(r.code, ResultCode::Ok);
        assert!(r.ecm);
        assert_eq!((r.pages, r.ecm_ppr_sent, r.ecm_frames_lost), (1, 0, 0));
    }
}

#[test]
fn e01u_mh_and_mr_pages_decode_through_ecm() {
    for (mr, coding) in [(false, 0), (true, 1)] {
        let bm = sparse_bitmap(ROWS, 9);
        let coded = if coding == 0 {
            encode_mh(&bm, ROWS)
        } else {
            encode_mr(&bm, ROWS, 4)
        };
        let frames = split_frames(&coded, 256);
        let mut s = to_ecm_wait_block(ecm_config(), &ecm_dcs(false, false, mr));
        send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
        s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
        assert_eq!(s.pages.len(), 1);
        assert_eq!(s.pages[0].bitmap, bm, "mr={mr}");
    }
}

#[test]
fn e01u_frame_numbers_are_least_significant_bit_first() {
    // Frame 1 travels as 0x80 (T.4 A.3.6.1). Sending the frames in reverse order must still
    // give the right page, and a frame number 0x01 on the wire would be frame 128.
    let (bm, coded) = page(5);
    let frames = split_frames(&coded, 64);
    assert!(frames.len() >= 3);
    let mut s = setup(64);
    s.send(Event::Signal(RxSignal::Training));
    for i in (0..frames.len()).rev() {
        let mut f = vec![0xFF, 0xC0, 0x60, (i as u8).reverse_bits()];
        f.extend(&frames[i]);
        s.send(frame_ev(f));
    }
    s.send(frame_ev(rcp()));
    s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
    assert_eq!(s.pages[0].bitmap, bm);
    // the PPS counters are LSB first too: 40 frames - 1 = 39 = 0b100111 -> 0xE4 on the wire
    assert_eq!(pps(Q_EOP, 1, 2, 40)[3..], [Q_EOP, 0x80, 0x40, 0xE4]);
}

#[test]
fn e01u_several_blocks_pps_null_then_mps_and_a_second_page() {
    let (bm1, c1) = page(1);
    let (bm2, c2) = page(2);
    let f1 = split_frames(&c1, 64);
    let f2 = split_frames(&c2, 64);
    let mut s = setup(64);
    // page 0 in blocks of 3 frames: PPS-NULL after each block but the last
    let mut b = 0u8;
    let mut idx = 0;
    while idx < f1.len() {
        let end = (idx + 3).min(f1.len());
        let chunk = &f1[idx..end];
        // frame numbers restart at 0 in every block
        s.send(Event::Signal(RxSignal::Training));
        for (n, f) in chunk.iter().enumerate() {
            s.send(frame_ev(fcd(n as u8, f)));
        }
        s.send(frame_ev(rcp()));
        let last = end == f1.len();
        s.send(frame_ev(pps(
            if last { Q_MPS } else { Q_NULL },
            0,
            b,
            chunk.len(),
        )));
        assert_eq!(last_response(&s), 0x31, "MCF for block {b}");
        s.send(Event::TxComplete);
        assert_eq!(s.state(), State::EcmWaitBlock);
        assert_eq!(s.pages.len(), usize::from(last), "page only after MPS");
        b += 1;
        idx = end;
    }
    assert_eq!(s.pages[0].bitmap, bm1);
    // page 1, one block, EOP
    send_fcds(&mut s, &f2, &(0..f2.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOP, 1, 0, f2.len())));
    assert_eq!(s.pages.len(), 2);
    assert_eq!(s.pages[1].bitmap, bm2);
    assert_eq!(s.pages[1].number, 2);
    s.send(Event::TxComplete);
    s.send(frame_ev(final_frame(0xDF, &[])));
    assert_eq!(s.result.unwrap().pages, 2);
}

#[test]
fn e01u_pps_eom_returns_to_phase_b_with_a_new_dis() {
    let (bm, coded) = page(4);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOM, 0, 0, frames.len())));
    assert_eq!(last_response(&s), 0x31);
    assert_eq!(s.pages[0].bitmap, bm);
    let acts = s.send(Event::TxComplete);
    assert_eq!(s.state(), State::SendingDis);
    assert!(summarize(&acts).iter().any(|a| a.starts_with("tx[01")));
}

#[test]
fn e01u_pri_variants_and_eos_are_taken_for_their_base_command() {
    // PRI-MPS (FA) behaves like MPS; PRI-EOP (FC) like EOP.
    let (_, coded) = page(4);
    let frames = split_frames(&coded, 256);
    for (q, next) in [(0xFA, State::EcmWaitBlock), (0xFC, State::WaitDcn)] {
        let mut s = setup(256);
        send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
        s.send(frame_ev(pps(q, 0, 0, frames.len())));
        s.send(Event::TxComplete);
        assert_eq!(s.state(), next, "q={q:#04x}");
        assert_eq!(s.pages.len(), 1);
    }
}

// ---------------------------------------------------------------------------- PPR / CTC / EOR

#[test]
fn e02u_ppr_asks_for_exactly_the_missing_frames_and_marks_unused_numbers() {
    let (bm, coded) = page(6);
    let frames = split_frames(&coded, 64);
    let n = frames.len();
    assert!(n >= 5, "need at least 5 frames, have {n}");
    let mut s = setup(64);
    let first: Vec<usize> = (0..n).filter(|i| *i != 1 && *i != 3).collect();
    send_fcds(&mut s, &frames, &first);
    s.send(frame_ev(pps(Q_EOP, 0, 0, n)));
    let ppr = last_tx(&s);
    assert_eq!(ppr[0], 0x3D, "PPR");
    assert_eq!(ppr.len(), 1 + 32, "256 bit map");
    let map = &ppr[1..];
    // Bit i (first transmitted = most significant) is frame i; 1 = send again; numbers past the
    // end of the block are 1 (Annex A.4.4).
    for i in 0..256usize {
        let bit = map[i / 8] & (0x80 >> (i % 8)) != 0;
        assert_eq!(bit, i == 1 || i == 3 || i >= n, "frame {i}");
    }
    assert!(
        s.pages.is_empty(),
        "nothing is delivered before the block is complete"
    );
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::EcmWaitBlock);
    assert_eq!(s.deadline(Timer::T2), Some(s.now + 6000));
    // the sender resends frames 1 and 3 and repeats the PPS
    send_fcds(&mut s, &frames, &[1, 3]);
    s.send(frame_ev(pps(Q_EOP, 0, 0, 2))); // SpanDSP puts the burst size here
    assert_eq!(last_response(&s), 0x31, "MCF once every frame is there");
    assert_eq!(s.pages[0].bitmap, bm, "pixel exact after the repair");
    s.send(Event::TxComplete);
    s.send(frame_ev(final_frame(0xDF, &[])));
    let r = s.result.unwrap();
    assert_eq!(
        (r.code, r.ecm_ppr_sent, r.ecm_frames_lost),
        (ResultCode::Ok, 1, 0)
    );
}

#[test]
fn e02u_a_frame_with_a_bad_fcs_counts_as_missing() {
    let (_, coded) = page(7);
    let frames = split_frames(&coded, 64);
    let mut s = setup(64);
    s.send(Event::Signal(RxSignal::Training));
    for (i, f) in frames.iter().enumerate() {
        if i == 2 {
            s.send(bad_frame_ev(fcd(2, f)));
        } else {
            s.send(frame_ev(fcd(i as u8, f)));
        }
    }
    s.send(frame_ev(rcp()));
    s.send(frame_ev(pps(Q_NULL, 0, 0, frames.len())));
    let ppr = last_tx(&s);
    assert_eq!(ppr[0], 0x3D);
    assert_eq!(ppr[1] & 0b1110_0000, 0b0010_0000, "frame 2 only");
}

#[test]
fn e02u_repeated_pps_repeats_the_response_without_counting_a_new_ppr() {
    let (_, coded) = page(8);
    let frames = split_frames(&coded, 64);
    let n = frames.len();
    let mut s = setup(64);
    send_fcds(&mut s, &frames, &(1..n).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOP, 0, 0, n)));
    assert_eq!(last_response(&s), 0x3D);
    s.send(Event::TxComplete);
    // the PPR was lost: the sender's T4 repeats the PPS, identical, with no frames in between
    let acts = s.send(frame_ev(pps(Q_EOP, 0, 0, n)));
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x3D))
    );
    assert_eq!(s.state(), State::EcmWaitBlock);
    s.send(Event::TxComplete);
    send_fcds(&mut s, &frames, &[0]);
    s.send(frame_ev(pps(Q_EOP, 0, 0, 1)));
    s.send(Event::TxComplete);
    s.send(frame_ev(final_frame(0xDF, &[])));
    assert_eq!(
        s.result.unwrap().ecm_ppr_sent,
        1,
        "the repeat is not a new request"
    );
}

#[test]
fn e02u_a_repeated_pps_after_mcf_gets_mcf_again_and_no_second_page() {
    let (_, coded) = page(8);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_MPS, 0, 0, frames.len())));
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::EcmWaitBlock);
    let acts = s.send(frame_ev(pps(Q_MPS, 0, 0, frames.len())));
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x31))
    );
    assert_eq!(s.pages.len(), 1, "the page is not delivered twice");
    // and after the EOP's MCF the same happens in WaitDcn
    s.send(Event::TxComplete);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOP, 1, 0, frames.len())));
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitDcn);
    let acts = s.send(frame_ev(pps(Q_EOP, 1, 0, frames.len())));
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x31))
    );
    assert_eq!(s.pages.len(), 2);
}

#[test]
fn e02u_ctc_is_answered_with_ctr_and_changes_the_rate() {
    let (bm, coded) = page(10);
    let frames = split_frames(&coded, 64);
    let n = frames.len();
    let mut s = setup(64);
    send_fcds(
        &mut s,
        &frames,
        &(0..n).filter(|i| *i != 2).collect::<Vec<_>>(),
    );
    for round in 0..4 {
        s.send(frame_ev(pps(Q_EOP, 0, 0, n)));
        assert_eq!(last_response(&s), 0x3D, "PPR {round}");
        s.send(Event::TxComplete);
        if round < 3 {
            // resend nothing useful (the line is bad): an empty burst and the PPS again
            send_fcds(&mut s, &frames, &[]);
        }
    }
    assert_eq!(s.state(), State::EcmWaitBlock);
    // CTC at 9600 bit/s V.29 (bits 11 = 1): the receiver answers CTR (X010 0011 = 0x23)
    s.send(frame_ev(ctc(&[11])));
    assert_eq!(last_tx(&s), [0x23]);
    s.send(Event::TxComplete);
    send_fcds(&mut s, &frames, &[2]);
    s.send(frame_ev(pps(Q_EOP, 0, 0, 1)));
    assert_eq!(last_response(&s), 0x31);
    assert_eq!(s.pages[0].bitmap, bm);
    s.send(Event::TxComplete);
    s.send(frame_ev(final_frame(0xDF, &[])));
    let r = s.result.unwrap();
    assert_eq!((r.code, r.ecm_ppr_sent), (ResultCode::Ok, 4));
    assert_eq!(
        (r.bit_rate, r.modem),
        (9600, Some(t30::Modem::V29)),
        "CTC rate"
    );
}

#[test]
fn e02u_a_repeated_ctc_repeats_ctr() {
    let (_, coded) = page(10);
    let frames = split_frames(&coded, 64);
    let mut s = setup(64);
    send_fcds(&mut s, &frames, &[0]);
    s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
    s.send(Event::TxComplete);
    s.send(frame_ev(ctc(&[11])));
    s.send(Event::TxComplete);
    let acts = s.send(frame_ev(ctc(&[11])));
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x23))
    );
}

#[test]
fn e04u_eor_ends_the_retransmission_the_page_keeps_zero_filled_frames() {
    let (bm, coded) = page(11);
    let frames = split_frames(&coded, 64);
    let n = frames.len();
    let mut s = setup(64);
    // frame 1 never arrives
    send_fcds(
        &mut s,
        &frames,
        &(0..n).filter(|i| *i != 1).collect::<Vec<_>>(),
    );
    for round in 0..4 {
        s.send(frame_ev(pps(Q_MPS, 0, 0, n)));
        assert_eq!(last_response(&s), 0x3D, "PPR {round}");
        s.send(Event::TxComplete);
        send_fcds(&mut s, &frames, &[]);
    }
    s.send(frame_ev(eor(Q_MPS)));
    assert_eq!(last_tx(&s), [0x38], "ERR (X011 1000)");
    assert_eq!(s.pages.len(), 1, "the page is delivered with its hole");
    assert_ne!(s.pages[0].bitmap, bm, "frame 1 was lost");
    assert!(
        s.pages[0].rows > 0 && s.pages[0].rows as usize <= ROWS,
        "rows before the hole"
    );
    s.send(Event::TxComplete);
    assert_eq!(
        s.state(),
        State::EcmWaitBlock,
        "EOR-MPS: the next page follows"
    );
    // the next page and the end
    let (bm2, c2) = page(12);
    let f2 = split_frames(&c2, 256);
    send_fcds(&mut s, &f2, &(0..f2.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOP, 1, 0, f2.len())));
    assert_eq!(s.pages[1].bitmap, bm2, "the next page is not affected");
    s.send(Event::TxComplete);
    s.send(frame_ev(final_frame(0xDF, &[])));
    let r = s.result.unwrap();
    assert_eq!(r.code, ResultCode::EcmFramesLost);
    assert_eq!((r.pages, r.ecm_ppr_sent, r.ecm_frames_lost), (2, 4, 1));
    assert!(r.is_partial());
}

#[test]
fn e04u_eor_variants_continue_like_the_matching_pps() {
    let (_, coded) = page(11);
    let frames = split_frames(&coded, 64);
    for (q, next) in [
        (Q_NULL, State::EcmWaitBlock),
        (Q_MPS, State::EcmWaitBlock),
        (Q_EOP, State::WaitDcn),
        (Q_EOM, State::SendingDis),
    ] {
        let mut s = setup(64);
        send_fcds(&mut s, &frames, &[0]);
        s.send(frame_ev(pps(q, 0, 0, frames.len())));
        s.send(Event::TxComplete);
        s.send(frame_ev(eor(q)));
        assert_eq!(last_tx(&s), [0x38], "q={q:#04x}");
        s.send(Event::TxComplete);
        assert_eq!(s.state(), next, "q={q:#04x}");
        assert_eq!(s.pages.len(), usize::from(q != Q_NULL), "q={q:#04x}");
    }
}

#[test]
fn e04u_a_repeated_eor_repeats_err() {
    let (_, coded) = page(11);
    let frames = split_frames(&coded, 64);
    let mut s = setup(64);
    send_fcds(&mut s, &frames, &[0]);
    s.send(frame_ev(pps(Q_NULL, 0, 0, frames.len())));
    s.send(Event::TxComplete);
    s.send(frame_ev(eor(Q_NULL)));
    s.send(Event::TxComplete);
    let acts = s.send(frame_ev(eor(Q_NULL)));
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x38))
    );
    let r = {
        s.send(Event::Abort);
        s.result.clone().unwrap()
    };
    assert_eq!(r.ecm_frames_lost as usize, frames.len() - 1, "counted once");
}

#[test]
fn e04u_a_peer_that_never_sends_ctc_or_eor_is_dropped() {
    let (_, coded) = page(11);
    let frames = split_frames(&coded, 64);
    let cfg = Config {
        ecm_max_ppr: 5,
        ..ecm_config()
    };
    let mut s = to_ecm_wait_block(cfg, &ecm_dcs(true, true, false));
    send_fcds(&mut s, &frames, &[0]);
    for _ in 0..5 {
        s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
        assert_eq!(last_response(&s), 0x3D);
        s.send(Event::TxComplete);
        send_fcds(&mut s, &frames, &[]);
    }
    s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
    assert_eq!(last_response(&s), 0x5F, "DCN");
    s.send(Event::TxComplete);
    assert_eq!(s.result.as_ref().unwrap().code, ResultCode::EcmFailed);
}

// ---------------------------------------------------------------------------- end of block

#[test]
fn e05u_a_block_also_ends_on_the_signal_end_on_a_pps_and_on_silence() {
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    // no RCP, only the end of the signal
    let mut s = setup(256);
    s.send(Event::Signal(RxSignal::Training));
    s.send(frame_ev(fcd(0, &frames[0])));
    assert_eq!(s.state(), State::EcmRxBlock);
    s.send(Event::Signal(RxSignal::NoSignal));
    assert_eq!(s.state(), State::EcmWaitPps);
    assert_eq!(s.deadline(Timer::T2), Some(s.now + 6000));
    // no RCP and no signal end: the PPS itself proves the block is over
    let mut s = setup(256);
    s.send(frame_ev(fcd(0, &frames[0])));
    s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
    assert_eq!(s.state(), State::SendingPostPage);
    // silence: the idle timer ends the block
    let mut s = setup(256);
    s.send(frame_ev(fcd(0, &frames[0])));
    assert!(s.deadline(Timer::Data).is_some());
    s.advance(15_000);
    assert_eq!(s.state(), State::EcmWaitPps);
}

#[test]
fn e05u_timers_in_the_ecm_states() {
    // T2 while waiting for the first frame: image timeout
    let mut s = setup(256);
    assert_eq!(s.deadline(Timer::T2), Some(s.now + 6000));
    s.advance(6000);
    s.send(Event::TxComplete);
    assert_eq!(s.result.as_ref().unwrap().code, ResultCode::T2ExpiredImage);
    // T2 while waiting for the post message command
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &[0]);
    s.advance(5999);
    assert_eq!(s.state(), State::EcmWaitPps);
    s.advance(1);
    s.send(Event::TxComplete);
    assert_eq!(
        s.result.as_ref().unwrap().code,
        ResultCode::T2ExpiredCommand
    );
}

#[test]
fn e05u_rr_repeats_the_last_response_while_waiting() {
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_MPS, 0, 0, frames.len())));
    s.send(Event::TxComplete);
    let acts = s.send(frame_ev(rr()));
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::SendFrames(f) if f[0][2] == 0x31))
    );
    // RR where it makes no sense is ignored without counting as an unexpected frame
    let mut s = setup(256);
    s.send(frame_ev(fcd(0, &frames[0])));
    for _ in 0..10 {
        let acts = s.send(frame_ev(rr()));
        assert!(summarize(&acts).is_empty());
    }
    assert_eq!(s.state(), State::EcmRxBlock);
    s.send(frame_ev(rcp()));
    for _ in 0..10 {
        assert!(summarize(&s.send(frame_ev(rr()))).is_empty());
    }
    assert_eq!(s.state(), State::EcmWaitPps);
}

#[test]
fn e05u_dcn_in_every_ecm_state() {
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    s.send(frame_ev(final_frame(0xDF, &[])));
    assert_eq!(s.result.unwrap().code, ResultCode::RemoteDisconnect);
    let mut s = setup(256);
    s.send(frame_ev(fcd(0, &frames[0])));
    s.send(frame_ev(final_frame(0xDF, &[])));
    assert_eq!(s.result.unwrap().code, ResultCode::RemoteDisconnect);
    // after a complete page DCN in the middle of the next one is a partial success
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_MPS, 0, 0, frames.len())));
    s.send(Event::TxComplete);
    s.send(frame_ev(fcd(0, &frames[0])));
    s.send(frame_ev(final_frame(0xDF, &[])));
    let r = s.result.unwrap();
    assert_eq!((r.code, r.pages), (ResultCode::PartialDcn, 1));
}

#[test]
fn e05u_ecm_frames_outside_an_ecm_block_are_ignored_not_counted() {
    // non-ECM page procedure: FCD and RCP are no commands and must not use up the budget of
    // unexpected frames
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    for i in 0..10 {
        s.send(frame_ev(fcd(i, &[0; 64])));
        s.send(frame_ev(rcp()));
    }
    assert_eq!(s.state(), State::WaitImage);
    assert!(s.result.is_none());
    // and in the ECM wait-for-PPS state a stray FCD is ignored
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &[0]);
    assert_eq!(s.state(), State::EcmWaitPps);
    s.send(frame_ev(fcd(1, &frames[0])));
    assert_eq!(s.state(), State::EcmWaitPps);
}

#[test]
fn e05u_malformed_ecm_frames_are_survivable() {
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    s.send(frame_ev(non_final_frame(0x60, &[]))); // FCD without a number
    s.send(frame_ev(fcd(0, &[0; 300]))); // too long
    assert_eq!(s.state(), State::EcmWaitBlock, "neither started a block");
    send_fcds(&mut s, &frames, &[0]);
    s.send(frame_ev(final_frame(0xFD, &[Q_EOP, 0]))); // PPS too short
    s.send(frame_ev(pps(0x55, 0, 0, 1))); // invalid FCF2
    assert_eq!(s.state(), State::EcmWaitPps);
    s.send(frame_ev(final_frame(0xF3, &[]))); // EOR without FCF2
    s.send(frame_ev(eor(0x55))); // EOR with an invalid FCF2
    assert_eq!(s.state(), State::EcmWaitPps);
    // a CTC with a reserved rate is answered but the rate stays
    s.send(frame_ev(ctc(&[11, 13])));
    assert_eq!(s.state(), State::SendingPostPage);
    assert_eq!(s.rx.dcs().map(|d| d.bit_rate), Some(14400));
}

#[test]
fn e05u_ecm_commands_without_ecm_are_unexpected_frames() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    for _ in 0..3 {
        s.send(frame_ev(pps(Q_EOP, 0, 0, 1)));
    }
    assert!(s.result.is_none());
    s.send(frame_ev(pps(Q_EOP, 0, 0, 1)));
    s.send(Event::TxComplete);
    assert_eq!(
        s.result.as_ref().unwrap().code,
        ResultCode::UnexpectedFrames
    );
}

#[test]
fn e06u_a_dcs_for_ecm_is_refused_when_ecm_is_off() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(ecm_dcs(false, false, false).frame()));
    s.send(Event::TxComplete);
    assert_eq!(s.result.as_ref().unwrap().code, ResultCode::IncompatibleDcs);
}

#[test]
fn e06u_a_second_dcs_in_an_ecm_state_retrains_and_resets_the_block() {
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &[0]);
    s.send(frame_ev(ecm_dcs(false, true, false).frame()));
    assert_eq!(s.state(), State::WaitTcf);
}

#[test]
fn e06u_tx_names_for_the_happy_path_are_in_t30_order() {
    let (_, coded) = page(13);
    let frames = split_frames(&coded, 256);
    let mut s = setup(256);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
    let tx: Vec<String> = tx_names(&s)
        .into_iter()
        .filter(|a| a.starts_with("tx["))
        .collect();
    // DIS, CFR, MCF
    assert_eq!(tx, ["tx[01]", "tx[21]", "tx[31]"]);
}

#[test]
fn e01u_local_tcf_with_ecm_goes_straight_to_the_first_block() {
    // localTCF (T.38 rate management): the DCS is not followed by a TCF and, by default, not by
    // a CFR either; the machine waits for the first FCD frame. (SpanDSP's T.38 terminal stalls
    // in this combination, so this path has no interop test.)
    let cfg = Config {
        tcf: t30::TcfMode::Local,
        ..ecm_config()
    };
    let mut s = after_dis(cfg);
    s.send(frame_ev(ecm_dcs(false, true, false).frame()));
    assert_eq!(s.state(), State::EcmWaitBlock);
    assert_eq!(s.deadline(Timer::T2), Some(s.now + 6000));
    let (bm, coded) = page(14);
    let frames = split_frames(&coded, 256);
    send_fcds(&mut s, &frames, &(0..frames.len()).collect::<Vec<_>>());
    s.send(frame_ev(pps(Q_EOP, 0, 0, frames.len())));
    assert_eq!(s.pages[0].bitmap, bm);
    // with `local_tcf_sends_cfr` the CFR goes out first, then the same
    let cfg = Config {
        tcf: t30::TcfMode::Local,
        local_tcf_sends_cfr: true,
        ..ecm_config()
    };
    let mut s = after_dis(cfg);
    s.send(frame_ev(ecm_dcs(false, true, false).frame()));
    assert_eq!(s.state(), State::SendingCfr);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::EcmWaitBlock);
}
