//! R04..R10 and further receiver behaviour: pure state machine, virtual clock.

mod common;

use common::*;
use t30::{Action, Config, Event, ResultCode, RxSignal, State, TcfMode, Timer};

const CFR: u8 = 0x21;
const FTT: u8 = 0x22;
const MCF: u8 = 0x31;
const RTN: u8 = 0x32;
const RTP: u8 = 0x33;
const DCN: u8 = 0x5F;
const DIS: u8 = 0x01;
const CSI: u8 = 0x02;
const MPS: u8 = 0x72 | 0x80;
const EOP: u8 = 0x74 | 0x80;
const EOM: u8 = 0x71 | 0x80;

fn cfg_id() -> Config {
    Config {
        local_id: "0287654321".into(),
        ..Config::default()
    }
}

fn mh_page_events(s: &mut Sim, bitmap: &[u8], rows: usize, chunk: usize) {
    let data = encode_mh(bitmap, rows);
    for c in data.chunks(chunk) {
        s.send(Event::ImageData(c.to_vec()));
    }
    s.send(Event::DataEnd);
}

fn last_done(s: &Sim) -> &t30::T30Result {
    s.result.as_ref().expect("call finished")
}

// ------------------------------------------------------------------ R04 transferredTCF

#[test]
fn r04_tcf_all_zero_and_long_enough_gets_cfr() {
    let mut s = after_dis(cfg_id());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::Signal(RxSignal::Training));
    // 14400 bit/s * 1.5 s = 2700 octets, in 30 ms T.38 chunks (54 octets)
    for _ in 0..50 {
        s.send(Event::ImageData(vec![0u8; 54]));
    }
    assert_eq!(s.state(), State::RxTcf);
    let acts = s.send(Event::DataEnd);
    assert_eq!(summarize(&acts), ["cancel Data", "tx[21]"]);
    assert_eq!(s.state(), State::SendingCfr);
}

#[test]
fn r04_tcf_length_boundary_is_exact() {
    // 14400 bit/s: 1.2 s = 2160 octets.
    for (len, expect) in [(2159usize, FTT), (2160, CFR), (2161, CFR)] {
        let mut s = after_dis(Config::default());
        s.send(frame_ev(DcsSpec::default().frame()));
        s.send(Event::ImageData(vec![0u8; len]));
        let acts = s.send(Event::DataEnd);
        assert_eq!(
            s.sent_fcfs().last().unwrap().1,
            vec![expect],
            "len {len}: {acts:?}"
        );
    }
    // 4800 bit/s V.27ter: 1.2 s = 720 octets.
    for (len, expect) in [(719usize, FTT), (720, CFR)] {
        let mut s = after_dis(Config::default());
        let spec = DcsSpec {
            rate: 4800,
            v17: false,
            ..DcsSpec::default()
        };
        s.send(frame_ev(spec.frame()));
        s.send(Event::ImageData(vec![0u8; len]));
        s.send(Event::DataEnd);
        assert_eq!(s.sent_fcfs().last().unwrap().1, vec![expect], "len {len}");
    }
}

#[test]
fn r04_tcf_error_rate_threshold_is_95_percent_zero_bits() {
    // 3000 octets = 24000 bits; 5 % of that is 1200 one-bits.
    for (ones_octets, expect) in [(150usize, CFR), (151, FTT)] {
        let mut s = after_dis(Config::default());
        s.send(frame_ev(DcsSpec::default().frame()));
        let mut data = vec![0u8; 3000];
        // 8 one-bits per damaged octet -> 150 octets = 1200 bits = exactly 5 %.
        for b in data.iter_mut().take(ones_octets) {
            *b = 0xFF;
        }
        s.send(Event::ImageData(data));
        s.send(Event::DataEnd);
        assert_eq!(
            s.sent_fcfs().last().unwrap().1,
            vec![expect],
            "{ones_octets} damaged octets"
        );
    }
}

#[test]
fn r04_ftt_then_retrain_at_a_lower_rate_passes() {
    let mut s = after_dis(cfg_id());
    s.send(frame_ev(tsi_frame("555")));
    s.send(frame_ev(DcsSpec::default().frame()));
    // 14400: the TCF arrives damaged.
    s.send(Event::ImageData(vec![0xFFu8; 2700]));
    s.send(Event::DataEnd);
    assert_eq!(s.state(), State::SendingFtt);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitRetrain);
    // The sender falls back to 9600 (V.17) and sends a clean TCF.
    let spec = DcsSpec {
        rate: 9600,
        v17: true,
        ..DcsSpec::default()
    };
    let acts = s.send(frame_ev(spec.frame()));
    assert_eq!(summarize(&acts), ["start T2=6000"]);
    assert_eq!(s.state(), State::WaitTcf);
    s.send(Event::ImageData(vec![0u8; 1800]));
    s.send(Event::DataEnd);
    assert_eq!(s.state(), State::SendingCfr);
    assert_eq!(
        s.sent_fcfs()
            .iter()
            .map(|(_, f)| f.clone())
            .collect::<Vec<_>>(),
        vec![vec![CSI, DIS], vec![FTT], vec![CFR]]
    );
    s.send(Event::TxComplete);
    // The result reports the rate that finally worked.
    s.send(Event::Abort);
    assert_eq!(last_done(&s).bit_rate, 9600);
}

#[test]
fn r04_no_tcf_at_all_times_out_with_ftt_at_the_exact_instant() {
    let mut s = after_dis(Config::default());
    s.at(10_000);
    s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(s.deadline(Timer::T2), Some(10_000 + 6_000));
    s.at(15_999);
    assert_eq!(s.state(), State::WaitTcf);
    s.at(16_000);
    assert_eq!(s.state(), State::SendingFtt);
    assert_eq!(s.sent_fcfs().last().unwrap(), &(16_000, vec![FTT]));
}

#[test]
fn r04_tcf_data_idle_timeout_closes_the_tcf() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0u8; 2700]));
    assert_eq!(s.deadline(Timer::Data), Some(15_000));
    s.at(15_000);
    assert_eq!(s.state(), State::SendingCfr);
}

#[test]
fn r04_signal_end_indicator_also_ends_the_tcf() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0u8; 2700]));
    s.send(Event::Signal(RxSignal::NoSignal));
    assert_eq!(s.state(), State::SendingCfr);
}

// ------------------------------------------------------------------ R05 localTCF

#[test]
fn r05_local_tcf_sends_nothing_and_waits_for_the_image_directly() {
    let cfg = Config {
        tcf: TcfMode::Local,
        ..cfg_id()
    };
    let mut s = after_dis(cfg);
    s.send(frame_ev(tsi_frame("42")));
    s.at(2_000);
    let acts = s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(
        summarize(&acts),
        ["cancel T1", "cancel T4", "start T2=6000"],
        "no TCF, no CFR: the page follows the DCS (SpanDSP's receiver does the same)"
    );
    assert_eq!(s.state(), State::WaitImage);
    assert_eq!(s.sent_fcfs().len(), 1, "only the DIS was ever sent");
    assert_eq!(s.deadline(Timer::T2), Some(8_000));
    // the page then arrives and is confirmed as usual
    let bm = test_bitmap(30, 1);
    mh_page_events(&mut s, &bm, 30, 100);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages[0].bitmap, bm);
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![MCF]);
}

#[test]
fn r05_local_tcf_can_still_be_configured_to_answer_with_cfr() {
    let cfg = Config {
        tcf: TcfMode::Local,
        local_tcf_sends_cfr: true,
        ..cfg_id()
    };
    let mut s = after_dis(cfg);
    s.at(2_000);
    let acts = s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(summarize(&acts), ["cancel T1", "cancel T4", "tx[21]"]);
    assert_eq!(s.state(), State::SendingCfr);
    assert_eq!(s.sent_fcfs().last().unwrap(), &(2_000, vec![CFR]));
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitImage);
}

#[test]
fn r05_local_tcf_t2_applies_when_the_image_never_comes() {
    let cfg = Config {
        tcf: TcfMode::Local,
        ..Config::default()
    };
    let mut s = after_dis(cfg);
    s.send(frame_ev(DcsSpec::default().frame()));
    let t = s.now;
    s.advance(5_999);
    assert_eq!(s.state(), State::WaitImage);
    s.advance(1);
    assert_eq!(s.now, t + 6_000);
    assert_eq!(s.state(), State::SendingDcn);
}

// ------------------------------------------------------------------ R06 phase D

#[test]
fn r06_mps_gets_mcf_and_the_next_page_follows() {
    let mut s = to_wait_image(cfg_id(), &DcsSpec::default());
    let bm1 = test_bitmap(60, 1);
    let bm2 = test_bitmap(70, 2);
    mh_page_events(&mut s, &bm1, 60, 54);
    assert_eq!(s.state(), State::WaitPostPage);
    s.at(1_000);
    let acts = s.send(simple_cmd(MPS));
    assert_eq!(
        summarize(&acts),
        ["cancel T2", "page#1 rows=60 bad=0", "tx[31]"]
    );
    assert_eq!(s.pages[0].bitmap, bm1, "page 1 is pixel exact");
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitImage);
    assert_eq!(s.deadline(Timer::T2), Some(1_000 + 6_000));
    mh_page_events(&mut s, &bm2, 70, 100);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages[1].bitmap, bm2, "page 2 is pixel exact");
    assert_eq!(s.pages[1].number, 2);
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![MCF]);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitDcn);
    s.send(simple_cmd(DCN));
    let r = last_done(&s);
    assert_eq!(r.code, ResultCode::Ok);
    assert_eq!(r.pages, 2);
    assert_eq!(r.bad_lines_per_page, vec![0, 0]);
    assert_eq!(r.remote_ident.as_deref(), Some("12345"));
    assert_eq!(r.bit_rate, 14400);
    assert!(s.rx.is_done());
}

#[test]
fn r06_eop_gets_mcf_and_waits_for_dcn_then_t2_expiry_is_still_success() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    mh_page_events(&mut s, &test_bitmap(30, 3), 30, 54);
    s.send(simple_cmd(EOP));
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitDcn);
    s.advance(5_999);
    assert!(!s.rx.is_done());
    s.advance(1);
    assert!(s.rx.is_done());
    assert_eq!(last_done(&s).code, ResultCode::Ok);
}

#[test]
fn r06_eom_gets_mcf_then_returns_to_phase_b_with_a_fresh_dis() {
    let mut s = to_wait_image(cfg_id(), &DcsSpec::default());
    mh_page_events(&mut s, &test_bitmap(30, 3), 30, 54);
    s.at(2_000);
    s.send(simple_cmd(EOM));
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![MCF]);
    s.at(2_400);
    let acts = s.send(Event::TxComplete);
    assert_eq!(s.state(), State::SendingDis);
    assert_eq!(summarize(&acts), ["start T1=35000", "tx[02,01]"]);
    assert!(s.deadline(Timer::T1) == Some(2_400 + 35_000));
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitCommand);
    assert_eq!(s.deadline(Timer::T4), Some(2_400 + 3_000));
    // The new DCS (for example a different resolution) starts a new training.
    let spec = DcsSpec {
        fine: true,
        ..DcsSpec::default()
    };
    s.send(frame_ev(spec.frame()));
    assert_eq!(s.state(), State::WaitTcf);
    assert_eq!(s.rx.dcs().unwrap().resolution, t30::Resolution::Fine);
}

#[test]
fn r06_clean_page_mcf_a_few_bad_lines_mcf_or_rtp_many_rtn() {
    // 200-row noise page. Bad rows are created by smashing bits of specific rows.
    let rows = 200;
    let bm = noise_bitmap(rows, 77);
    let cases: &[(usize, u8)] = &[
        (0, MCF),
        (10, MCF), // exactly 5 %
        (11, RTP), // just above 5 %
        (20, RTP), // exactly 10 %
        (21, RTN), // just above 10 %
    ];
    for &(bad, expect) in cases {
        let mut s = to_wait_image(Config::default(), &DcsSpec::default());
        let mut data = encode_mh(&bm, rows);
        // every 9th row, so that the damaged rows are never adjacent
        let victims: Vec<usize> = (0..bad).map(|i| 3 + i * 9).collect();
        smash_rows(&mut data, &victims);
        for c in data.chunks(200) {
            s.send(Event::ImageData(c.to_vec()));
        }
        s.send(Event::DataEnd);
        s.send(simple_cmd(MPS));
        assert_eq!(
            s.sent_fcfs().last().unwrap().1,
            vec![expect],
            "{bad} bad lines of {rows}"
        );
        if expect == RTN {
            assert!(s.pages.is_empty(), "a rejected page is not delivered");
        } else {
            assert_eq!(s.pages.len(), 1);
            assert_eq!(s.pages[0].bad_lines as usize, bad);
            assert_eq!(s.pages[0].rows as usize, rows);
        }
    }
}

#[test]
fn r06_bad_run_longer_than_2_5_mm_is_rtn_at_standard_and_fine() {
    let rows = 400;
    let bm = noise_bitmap(rows, 5);
    // standard: 2.5 mm = 10 lines, so 10 is tolerated and 11 is not; fine: 19 and 20.
    for (fine, run, expect) in [
        (false, 10usize, MCF),
        (false, 11, RTN),
        (true, 19, MCF),
        (true, 20, RTN),
    ] {
        let spec = DcsSpec {
            fine,
            ..DcsSpec::default()
        };
        // isolate the run criterion
        let cfg = Config {
            rtp_bad_percent: 100,
            rtn_bad_percent: 100,
            ..Config::default()
        };
        let mut s = to_wait_image(cfg, &spec);
        let mut data = encode_mh(&bm, rows);
        let victims: Vec<usize> = (50..50 + run).collect();
        smash_rows(&mut data, &victims);
        for c in data.chunks(300) {
            s.send(Event::ImageData(c.to_vec()));
        }
        s.send(Event::DataEnd);
        s.send(simple_cmd(MPS));
        let got = s.sent_fcfs().last().unwrap().1.clone();
        assert_eq!(got, vec![expect], "fine={fine} run={run}");
    }
}

#[test]
fn r06_rtn_then_retrain_and_resend_of_the_same_page() {
    let rows = 100;
    let bm = noise_bitmap(rows, 9);
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let mut bad = encode_mh(&bm, rows);
    smash_rows(&mut bad, &(10..40).collect::<Vec<_>>());
    s.send(Event::ImageData(bad));
    s.send(Event::DataEnd);
    s.send(simple_cmd(MPS));
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![RTN]);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitRetrain);
    // Retrain: DCS + TCF + the page again, now clean.
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0u8; 2700]));
    s.send(Event::DataEnd);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitImage);
    s.send(Event::ImageData(encode_mh(&bm, rows)));
    s.send(Event::DataEnd);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages.len(), 1);
    assert_eq!(s.pages[0].number, 1, "the rejected attempt did not count");
    assert_eq!(s.pages[0].bitmap, bm);
}

#[test]
fn r06_rtn_followed_by_dcn_reports_page_rejected() {
    let rows = 100;
    let bm = noise_bitmap(rows, 9);
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let mut bad = encode_mh(&bm, rows);
    smash_rows(&mut bad, &(10..40).collect::<Vec<_>>());
    s.send(Event::ImageData(bad));
    s.send(Event::DataEnd);
    s.send(simple_cmd(MPS));
    s.send(Event::TxComplete);
    s.send(simple_cmd(DCN));
    let r = last_done(&s);
    assert_eq!(r.code, ResultCode::PageRejected);
    assert_eq!(r.pages, 0);
}

#[test]
fn r06_rtp_accepts_the_page_and_expects_a_retrain() {
    let rows = 200;
    let bm = noise_bitmap(rows, 77);
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let mut data = encode_mh(&bm, rows);
    smash_rows(&mut data, &(0..15).map(|i| 3 + i * 9).collect::<Vec<_>>());
    s.send(Event::ImageData(data));
    s.send(Event::DataEnd);
    s.send(simple_cmd(EOP));
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![RTP]);
    assert_eq!(s.pages.len(), 1, "RTP confirms the page");
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitRetrain);
    // The sender has nothing more to send after EOP: it disconnects. That is a success.
    s.send(simple_cmd(DCN));
    assert_eq!(last_done(&s).code, ResultCode::Ok);
    assert_eq!(last_done(&s).pages, 1);
}

#[test]
fn r06_mr_and_mmr_pages_are_decoded_from_the_dcs() {
    for (spec, enc) in [
        (
            DcsSpec {
                mr: true,
                ..DcsSpec::default()
            },
            0u8,
        ),
        (
            DcsSpec {
                mmr: true,
                ..DcsSpec::default()
            },
            1u8,
        ),
    ] {
        let bm = test_bitmap(50, 4);
        let data = if enc == 0 {
            encode_mr(&bm, 50, 4)
        } else {
            encode_mmr(&bm)
        };
        let mut s = to_wait_image(Config::default(), &spec);
        for c in data.chunks(64) {
            s.send(Event::ImageData(c.to_vec()));
        }
        s.send(Event::DataEnd);
        s.send(simple_cmd(EOP));
        assert_eq!(s.pages.len(), 1, "enc {enc}");
        assert_eq!(s.pages[0].bitmap, bm, "enc {enc}");
        let want = if enc == 0 {
            t30::Compression::Mr
        } else {
            t30::Compression::Mmr
        };
        assert_eq!(s.pages[0].compression, want);
    }
}

#[test]
fn r06_page_geometry_follows_the_dcs() {
    // B4 width (bit 17), fine resolution (bit 15)
    let spec = DcsSpec {
        fine: true,
        width_bits: vec![17],
        ..DcsSpec::default()
    };
    let mut s = to_wait_image(Config::default(), &spec);
    let bm = vec![0u8; 256 * 20];
    let mut e = t4::Encoder::new(2048, t4::Coding::Mh).unwrap();
    for y in 0..20 {
        e.push_row(&bm[y * 256..(y + 1) * 256]).unwrap();
    }
    s.send(Event::ImageData(e.finish()));
    s.send(Event::DataEnd);
    s.send(simple_cmd(EOP));
    let p = &s.pages[0];
    assert_eq!((p.width, p.rows, p.x_dpi, p.y_dpi), (2048, 20, 204, 196));
}

#[test]
fn r06_lost_signal_end_is_inferred_from_the_post_page_command() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let bm = test_bitmap(40, 8);
    s.send(Event::ImageData(encode_mh(&bm, 40)));
    // no DataEnd, no NoSignal: the V.21 command arrives directly
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages.len(), 1);
    assert_eq!(s.pages[0].bitmap, bm);
}

#[test]
fn r06_image_data_idle_timeout_ends_the_page() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    s.send(Event::ImageData(encode_mh(&test_bitmap(40, 8), 40)));
    s.advance(14_999);
    assert_eq!(s.state(), State::RxImage);
    s.advance(1);
    assert_eq!(s.state(), State::WaitPostPage);
}

// ------------------------------------------------------------------ R07 timers

#[test]
fn r07_dis_is_retransmitted_every_t4_three_times_then_the_call_fails() {
    let mut s = started(cfg_id());
    // t=0: CED + CSI/DIS handed out, T1 running.
    assert_eq!(s.deadline(Timer::T1), Some(35_000));
    assert!(s.deadline(Timer::T4).is_none());
    // The front end needs 3.4 s for CED + DIS (T4 only starts when the burst is out).
    s.at(3_400);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), Some(6_400));
    // 1st retransmission
    s.at(6_399);
    assert_eq!(s.sent_fcfs().len(), 1);
    s.at(6_400);
    assert_eq!(s.sent_fcfs().len(), 2);
    assert_eq!(s.state(), State::SendingDis);
    s.at(7_000);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), Some(10_000));
    // 2nd
    s.at(10_000);
    assert_eq!(s.sent_fcfs().len(), 3);
    s.at(10_600);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), Some(13_600));
    // 3rd
    s.at(13_600);
    assert_eq!(s.sent_fcfs().len(), 4);
    s.at(14_200);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), Some(17_200));
    // The T4 after the third retransmission ends the call: DCN, then Done.
    s.at(17_199);
    assert_eq!(s.state(), State::WaitCommand);
    s.at(17_200);
    assert_eq!(s.state(), State::SendingDcn);
    assert_eq!(s.sent_fcfs().last().unwrap(), &(17_200, vec![DCN]));
    assert_eq!(s.sent_fcfs().len(), 5, "DIS x4 and the DCN");
    s.at(17_500);
    s.send(Event::TxComplete);
    let r = last_done(&s);
    assert_eq!(r.code, ResultCode::NoResponseToDis);
    assert_eq!(r.duration_ms, 17_500);
    assert!(s.timers.is_empty(), "no timer is left running");
}

#[test]
fn r07_t1_expires_at_35_s_when_the_dis_cannot_even_be_sent() {
    let mut s = started(Config::default());
    // The front end never reports TxComplete (stuck): T1 still ends the call.
    s.at(34_999);
    assert_eq!(s.state(), State::SendingDis);
    s.at(35_000);
    assert_eq!(s.state(), State::SendingDcn);
    s.send(Event::TxComplete); // the DIS burst
    assert!(!s.rx.is_done());
    s.send(Event::TxComplete); // the DCN
    assert_eq!(last_done(&s).code, ResultCode::T1Expired);
}

#[test]
fn r07_t1_is_cancelled_by_the_first_valid_dcs() {
    let mut s = after_dis(Config::default());
    s.at(20_000);
    s.send(frame_ev(DcsSpec::default().frame()));
    assert!(s.deadline(Timer::T1).is_none());
    assert!(s.deadline(Timer::T4).is_none());
    s.at(60_000);
    // only T2 (6 s) could have fired: FTT after the missing TCF
    assert_ne!(s.state(), State::Done);
}

#[test]
fn r07_t2_after_cfr_waiting_for_the_image_fails_at_6_s() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let start = s.now;
    assert_eq!(s.deadline(Timer::T2), Some(start + 6_000));
    s.advance(5_999);
    assert_eq!(s.state(), State::WaitImage);
    s.advance(1);
    assert_eq!(s.state(), State::SendingDcn);
    s.send(Event::TxComplete);
    assert_eq!(last_done(&s).code, ResultCode::T2ExpiredImage);
}

#[test]
fn r07_t2_waiting_for_the_post_page_command_fails_and_keeps_confirmed_pages() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    // page 1 complete and confirmed
    mh_page_events(&mut s, &test_bitmap(20, 1), 20, 100);
    s.send(simple_cmd(MPS));
    s.send(Event::TxComplete);
    // page 2 arrives but the command never does
    mh_page_events(&mut s, &test_bitmap(20, 2), 20, 100);
    assert_eq!(s.state(), State::WaitPostPage);
    let t = s.now;
    s.advance(6_000);
    assert_eq!(s.now, t + 6_000);
    assert_eq!(s.state(), State::SendingDcn);
    s.send(Event::TxComplete);
    let r = last_done(&s);
    assert_eq!(r.code, ResultCode::T2ExpiredCommand);
    assert_eq!(r.pages, 1);
    assert!(r.is_partial());
}

#[test]
fn r07_t2_waiting_for_a_retrain_dcs_fails() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0xFFu8; 100]));
    s.send(Event::DataEnd);
    s.at(1_000);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitRetrain);
    s.at(1_000 + 5_999);
    assert_eq!(s.state(), State::WaitRetrain);
    s.at(1_000 + 6_000);
    assert_eq!(s.state(), State::SendingDcn);
}

#[test]
fn r07_configured_timer_values_are_honoured() {
    let cfg = Config {
        timers: t30::Timers {
            t1_ms: 10_000,
            t2_ms: 2_000,
            t4_ms: 1_000,
            ..t30::Timers::default()
        },
        dis_retries: 1,
        ..Config::default()
    };
    let mut s = started(cfg);
    assert_eq!(s.deadline(Timer::T1), Some(10_000));
    s.at(500);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), Some(1_500));
    s.at(1_500);
    s.send(Event::TxComplete);
    assert_eq!(s.deadline(Timer::T4), Some(2_500));
    s.at(2_500);
    assert_eq!(s.state(), State::SendingDcn, "one retransmission only");
}

#[test]
fn r07_stale_timer_events_are_ignored() {
    let mut s = after_dis(Config::default());
    let acts = s.send(Event::Timer(Timer::T2)); // never started
    assert!(acts.is_empty());
    assert_eq!(s.state(), State::WaitCommand);
}

// ------------------------------------------------------------------ R08 DCN anywhere

#[test]
fn r08_dcn_is_handled_in_every_active_state() {
    type Setup = Box<dyn Fn() -> Sim>;
    let setups: Vec<(&str, Setup)> = vec![
        ("SendingDis", Box::new(|| started(Config::default()))),
        ("WaitCommand", Box::new(|| after_dis(Config::default()))),
        (
            "WaitTcf",
            Box::new(|| {
                let mut s = after_dis(Config::default());
                s.send(frame_ev(DcsSpec::default().frame()));
                s
            }),
        ),
        (
            "RxTcf",
            Box::new(|| {
                let mut s = after_dis(Config::default());
                s.send(frame_ev(DcsSpec::default().frame()));
                s.send(Event::ImageData(vec![0; 10]));
                s
            }),
        ),
        (
            "WaitImage",
            Box::new(|| to_wait_image(Config::default(), &DcsSpec::default())),
        ),
        (
            "RxImage",
            Box::new(|| {
                let mut s = to_wait_image(Config::default(), &DcsSpec::default());
                s.send(Event::ImageData(vec![0x35, 0x1A]));
                s
            }),
        ),
    ];
    for (name, setup) in setups {
        let mut s = setup();
        let acts = s.send(simple_cmd(DCN));
        assert!(s.rx.is_done(), "{name}");
        let sum = summarize(&acts);
        assert!(sum.last().unwrap().starts_with("done "), "{name}: {sum:?}");
        assert!(s.timers.is_empty(), "{name}: timers cancelled");
        assert!(
            !sum.iter().any(|a| a.starts_with("tx[")),
            "{name}: nothing is sent in reply to a DCN"
        );
        assert!(
            !last_done(&s).code.is_ok(),
            "{name}: early DCN is not a success"
        );
    }
}

#[test]
fn r08_dcn_during_reception_reports_partial_success_for_the_finished_pages() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    mh_page_events(&mut s, &test_bitmap(30, 1), 30, 100);
    s.send(simple_cmd(MPS));
    s.send(Event::TxComplete);
    // page 2 is cut off by the peer hanging up
    s.send(Event::ImageData(vec![0x35; 20]));
    s.send(simple_cmd(DCN));
    let r = last_done(&s);
    assert_eq!(r.code, ResultCode::PartialDcn);
    assert!(r.is_partial());
    assert_eq!(r.pages, 1);
    assert_eq!(r.bad_lines_per_page, vec![0]);
    assert_eq!(s.pages.len(), 1);
}

#[test]
fn r08_dcn_after_ftt_is_a_training_failure() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0xFF; 100]));
    s.send(Event::DataEnd);
    s.send(Event::TxComplete);
    s.send(simple_cmd(DCN));
    assert_eq!(last_done(&s).code, ResultCode::TrainingFailed);
}

#[test]
fn r08_dcn_before_anything_else_is_a_plain_remote_disconnect() {
    let mut s = after_dis(Config::default());
    s.send(simple_cmd(DCN));
    assert_eq!(last_done(&s).code, ResultCode::RemoteDisconnect);
}

#[test]
fn r08_dcn_with_the_x_bit_set_is_recognised() {
    // SpanDSP sends DCN as 0xDF (X = 1).
    let mut s = after_dis(Config::default());
    s.send(frame_ev(vec![0xFF, 0xC8, 0xDF]));
    assert!(s.rx.is_done());
}

#[test]
fn r08_abort_ends_the_call_without_sending_anything() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let acts = s.send(Event::Abort);
    let sum = summarize(&acts);
    assert_eq!(sum, ["cancel T2", "done aborted"]);
    assert!(s.rx.is_done());
    // Idle abort too
    let mut s = Sim::new(Config::default());
    s.send(Event::Abort);
    assert_eq!(last_done(&s).code, ResultCode::Aborted);
}

#[test]
fn r08_nothing_happens_after_done() {
    let mut s = after_dis(Config::default());
    s.send(simple_cmd(DCN));
    for ev in [
        Event::Start,
        Event::TxComplete,
        Event::DataEnd,
        Event::ImageData(vec![1]),
        simple_cmd(MPS),
        Event::Timer(Timer::T1),
        Event::Abort,
    ] {
        assert!(s.send(ev).is_empty());
    }
}

// ------------------------------------------------------------------ R09 CRP

#[test]
fn r09_crp_repeats_the_dis_burst_in_phase_b() {
    let mut s = after_dis(cfg_id());
    let before = s.sent_fcfs().len();
    let acts = s.send(simple_cmd(0x58));
    assert_eq!(summarize(&acts), ["tx[02,01]"]);
    assert_eq!(s.sent_fcfs().len(), before + 1);
    assert_eq!(s.state(), State::WaitCommand, "state unchanged");
}

#[test]
fn r09_crp_repeats_cfr_after_the_dcs_and_mcf_after_a_page() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    let acts = s.send(simple_cmd(0x58));
    assert_eq!(summarize(&acts), ["tx[21]"]);
    s.send(Event::TxComplete);
    mh_page_events(&mut s, &test_bitmap(20, 1), 20, 100);
    s.send(simple_cmd(MPS));
    s.send(Event::TxComplete);
    let acts = s.send(simple_cmd(0x58 | 0x80));
    assert_eq!(summarize(&acts), ["tx[31]"], "CRP with the X bit too");
    assert_eq!(s.state(), State::WaitImage);
}

#[test]
fn r09_crp_before_any_response_is_ignored() {
    let mut s = Sim::new(Config::default());
    s.send(Event::Start);
    // the DIS is the last response; with a fresh machine in Idle nothing is repeated
    let mut idle = Sim::new(Config::default());
    assert!(idle.send(simple_cmd(0x58)).is_empty());
    drop(s);
}

#[test]
fn r09_repeated_post_page_command_resends_the_response_without_a_second_page() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    mh_page_events(&mut s, &test_bitmap(20, 1), 20, 100);
    s.send(simple_cmd(MPS));
    s.send(Event::TxComplete);
    // our MCF got lost: the sender repeats MPS
    let acts = s.send(simple_cmd(MPS));
    assert_eq!(summarize(&acts), ["tx[31]"]);
    assert_eq!(s.pages.len(), 1);
    assert_eq!(s.rx.pages(), 1);
    // after real image data a second MPS is no longer a repeat
    mh_page_events(&mut s, &test_bitmap(20, 2), 20, 100);
    s.send(simple_cmd(MPS));
    assert_eq!(s.pages.len(), 2);
}

// ------------------------------------------------------------------ R10 FCS errors

#[test]
fn r10_bad_fcs_frame_is_not_acted_on() {
    let mut s = after_dis(Config::default());
    let before = s.timers.clone();
    let acts = s.send(bad_frame_ev(DcsSpec::default().frame()));
    assert!(summarize(&acts).is_empty(), "no actions except a log line");
    assert!(acts.iter().all(|a| matches!(a, Action::Log(_))));
    assert_eq!(s.state(), State::WaitCommand);
    assert_eq!(s.timers, before, "T4 keeps running");
    assert!(s.rx.dcs().is_none());
}

#[test]
fn r10_bad_fcs_then_the_retransmitted_good_frame_is_used() {
    let mut s = after_dis(Config::default());
    s.send(bad_frame_ev(DcsSpec::default().frame()));
    s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(s.state(), State::WaitTcf);
}

#[test]
fn r10_bad_fcs_final_command_in_phase_d_is_ignored_and_t2_still_applies() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    mh_page_events(&mut s, &test_bitmap(20, 1), 20, 100);
    let t2 = s.deadline(Timer::T2);
    s.send(bad_frame_ev(final_frame(0x74 | 0x80, &[])));
    assert_eq!(s.state(), State::WaitPostPage);
    assert!(s.pages.is_empty());
    assert_eq!(s.deadline(Timer::T2), t2);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages.len(), 1);
}

#[test]
fn r10_bad_non_final_tsi_is_not_remembered() {
    let mut s = after_dis(Config::default());
    s.send(bad_frame_ev(tsi_frame("999")));
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::Abort);
    assert_eq!(last_done(&s).remote_ident, None);
}

#[test]
fn r10_optional_crp_reply_to_a_bad_frame() {
    let cfg = Config {
        crp_on_bad_fcs: true,
        ..Config::default()
    };
    let mut s = after_dis(cfg);
    let acts = s.send(bad_frame_ev(vec![0xFF, 0xC8, 0xC1, 0x00]));
    assert_eq!(summarize(&acts), ["tx[58]"]);
    assert_eq!(s.state(), State::WaitCommand);
    // not while sending or when nothing is expected
    let cfg = Config {
        crp_on_bad_fcs: true,
        ..Config::default()
    };
    let mut s = started(cfg);
    assert!(summarize(&s.send(bad_frame_ev(vec![1, 2, 3]))).is_empty());
}

#[test]
fn r10_lost_octets_mark_the_frame_bad_and_malformed_frames_are_ignored() {
    let mut s = after_dis(Config::default());
    for junk in [
        vec![],
        vec![0xFF],
        vec![0xFF, 0xC8],
        vec![0x00, 0xC8, 0xC1],
        vec![0xFF, 0x13, 0xC1],
    ] {
        let acts = s.send(frame_ev(junk));
        assert!(summarize(&acts).is_empty());
    }
    assert_eq!(s.state(), State::WaitCommand);
}

// ------------------------------------------------------------------ DCS handling

#[test]
fn dcs_requesting_ecm_or_high_resolution_is_refused_with_dcn() {
    for extra in [vec![27], vec![26], vec![41], vec![42], vec![43]] {
        let mut s = after_dis(Config::default());
        let spec = DcsSpec {
            extra: extra.clone(),
            ..DcsSpec::default()
        };
        s.send(frame_ev(spec.frame()));
        assert_eq!(s.state(), State::SendingDcn, "bit {extra:?}");
        s.send(Event::TxComplete);
        let r = last_done(&s);
        assert_eq!(r.code, ResultCode::IncompatibleDcs, "bit {extra:?}");
    }
}

#[test]
fn dcs_with_a_reserved_modem_code_gets_ftt() {
    let mut s = after_dis(Config::default());
    let f = final_frame(0xC1, &fif_with_bits(3, &[10, 11, 13, 20, 24 - 8 + 8]));
    s.send(frame_ev(f));
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![FTT]);
}

#[test]
fn dcs_too_short_is_ignored() {
    let mut s = after_dis(Config::default());
    let acts = s.send(frame_ev(final_frame(0xC1, &[0x40, 0x04])));
    assert!(summarize(&acts).is_empty());
    assert_eq!(s.state(), State::WaitCommand);
}

#[test]
fn tsi_is_decoded_and_reported() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(tsi_frame("+886 2 1234")));
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::Abort);
    assert_eq!(last_done(&s).remote_ident.as_deref(), Some("+886 2 1234"));
}

#[test]
fn unexpected_final_frames_are_tolerated_a_few_times_then_the_call_is_dropped() {
    let mut s = after_dis(Config::default());
    for i in 1..=3 {
        s.send(simple_cmd(0x80 | 0x24)); // CSA: nothing a receiver expects
        assert_eq!(s.state(), State::WaitCommand, "after {i}");
    }
    s.send(simple_cmd(0x80 | 0x24));
    assert_eq!(s.state(), State::SendingDcn);
    s.send(Event::TxComplete);
    assert_eq!(last_done(&s).code, ResultCode::UnexpectedFrames);
}

#[test]
fn a_dcs_while_a_response_is_in_flight_is_ignored() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0u8; 2700]));
    s.send(Event::DataEnd);
    assert_eq!(s.state(), State::SendingCfr);
    let acts = s.send(frame_ev(DcsSpec::default().frame()));
    assert!(summarize(&acts).is_empty());
    assert_eq!(s.state(), State::SendingCfr);
}

#[test]
fn a_dcs_in_the_middle_of_the_tcf_restarts_the_training() {
    let mut s = after_dis(Config::default());
    s.send(frame_ev(DcsSpec::default().frame()));
    s.send(Event::ImageData(vec![0u8; 1000]));
    s.send(frame_ev(DcsSpec::default().frame()));
    assert_eq!(s.state(), State::WaitTcf);
    // The old partial TCF does not count.
    s.send(Event::ImageData(vec![0u8; 1500]));
    s.send(Event::DataEnd);
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![FTT]);
}

#[test]
fn dis_burst_has_csi_only_with_an_identification() {
    let r = t30::Receiver::new(Config::default());
    assert_eq!(r.dis_burst().len(), 1);
    let r = t30::Receiver::new(cfg_id());
    let b = r.dis_burst();
    assert_eq!(b.len(), 2);
    assert_eq!(&b[0][..3], &[0xFF, 0xC0, 0x02]);
    assert_eq!(&b[1][..3], &[0xFF, 0xC8, 0x01]);
}

#[test]
fn start_sends_ced_then_the_dis_burst() {
    let mut s = Sim::new(cfg_id());
    let acts = s.send(Event::Start);
    assert_eq!(
        summarize(&acts),
        ["start T1=35000", "signal Ced", "tx[02,01]"]
    );
    // A second Start is ignored.
    assert!(s.send(Event::Start).is_empty());
}

#[test]
fn empty_page_gets_rtn() {
    let mut s = to_wait_image(Config::default(), &DcsSpec::default());
    s.send(Event::DataEnd);
    s.send(simple_cmd(MPS));
    assert_eq!(s.sent_fcfs().last().unwrap().1, vec![RTN]);
}

#[test]
fn row_limit_is_enforced_by_the_decoder() {
    let cfg = Config {
        max_rows: 25,
        ..Config::default()
    };
    let mut s = to_wait_image(cfg, &DcsSpec::default());
    s.send(Event::ImageData(encode_mh(&test_bitmap(60, 1), 60)));
    s.send(Event::DataEnd);
    s.send(simple_cmd(EOP));
    assert_eq!(s.pages[0].rows, 25);
    assert_eq!(s.pages[0].end, t4::EndReason::RowLimit);
}

// ------------------------------------------------------------------ helpers

/// Rows of busy random-ish pixels (long MH rows that are easy to damage).
fn noise_bitmap(rows: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = vec![0u8; rows * 216];
    for b in &mut v {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 24) as u8;
    }
    v
}

fn eol_positions(data: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut zeros = 0usize;
    for i in 0..data.len() * 8 {
        let bit = (data[i / 8] >> (7 - i % 8)) & 1;
        if bit == 0 {
            zeros += 1;
        } else {
            if zeros >= 11 {
                out.push(i - 11);
            }
            zeros = 0;
        }
    }
    out
}

/// Overwrites the middle of the given rows with ones so that they no longer decode.
fn smash_rows(data: &mut [u8], rows: &[usize]) {
    let eols = eol_positions(data);
    for &r in rows {
        let (start, end) = (eols[r] + 12, eols[r + 1]);
        assert!(
            end - start > 60,
            "row {r} too short to damage ({})",
            end - start
        );
        for i in start + 8..start + 8 + 40 {
            data[i / 8] |= 0x80 >> (i % 8);
        }
    }
}
