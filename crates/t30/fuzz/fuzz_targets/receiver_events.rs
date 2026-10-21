#![no_main]
//! Drives the receiver with an event stream decoded from the input (one selector octet per
//! event, followed by its payload). The machine must stay total and keep its invariants:
//! nothing after `Done`, at most one `Done`, timers cancelled only when running, frames it sends
//! are well formed.

use libfuzzer_sys::fuzz_target;
use t30::{Action, Config, Event, Frame, Receiver, RxSignal, TcfMode, Timer};

fuzz_target!(|data: &[u8]| {
    let Some((&mode, mut rest)) = data.split_first() else { return };
    let cfg = Config {
        tcf: if mode & 1 == 0 { TcfMode::Transferred } else { TcfMode::Local },
        mmr: mode & 2 != 0,
        ecm: mode & 4 != 0,
        ecm_max_ppr: 3,
        max_rows: 400,
        ..Config::default()
    };
    let mut rx = Receiver::new(cfg);
    let mut now = 0u64;
    let mut armed = [false; 4];
    let mut dones = 0u32;
    let mut guard = 0;
    while let Some((&sel, tail)) = rest.split_first() {
        guard += 1;
        if guard > 400 {
            break;
        }
        let len = usize::from(sel >> 4) * 17 % 120;
        let (payload, next) = tail.split_at(len.min(tail.len()));
        rest = next;
        now += u64::from(sel & 3) * 5;
        let ev = match sel % 14 {
            0 => Event::Start,
            1 => Event::TxComplete,
            2 => Event::Abort,
            3 => Event::DataEnd,
            4 => Event::ImageData(payload.to_vec()),
            5 => Event::Signal(match payload.first().map_or(0, |b| b % 5) {
                0 => RxSignal::Cng,
                1 => RxSignal::Ced,
                2 => RxSignal::V21Preamble,
                3 => RxSignal::Training,
                _ => RxSignal::NoSignal,
            }),
            6 => Event::Timer(Timer::ALL[usize::from(payload.first().copied().unwrap_or(0)) % 4]),
            11 => {
                // ECM image frame: FF C0 60, frame number, data
                let mut f = vec![0xFF, 0xC0, 0x60];
                f.extend_from_slice(payload);
                Event::Frame { data: f, fcs_ok: sel & 4 == 0 }
            }
            12 => {
                // ECM command: the first payload octet picks PPS / EOR / CTC / RR / RCP
                let fcf = [0xFDu8, 0xF3, 0xC8, 0xF6, 0x61][usize::from(payload.first().copied().unwrap_or(0)) % 5];
                let mut f = vec![0xFF, 0xC8, fcf];
                f.extend_from_slice(payload.get(1..).unwrap_or(&[]));
                Event::Frame { data: f, fcs_ok: sel & 4 == 0 }
            }
            13 => {
                // a well formed DCS for ECM (bit 27, optionally 64 octet frames) to reach the ECM states
                let mut fif = vec![0x00, 0x44, 0x1F, 0x20];
                if payload.first().is_some_and(|b| b & 1 != 0) {
                    fif[3] |= 0x10;
                }
                let mut f = vec![0xFF, 0xC8, 0xC1];
                f.extend_from_slice(&fif);
                Event::Frame { data: f, fcs_ok: true }
            }
            7 | 8 => {
                // well formed header so that the payload reaches the command handlers
                let mut f = vec![0xFF, 0xC8];
                f.extend_from_slice(payload);
                Event::Frame { data: f, fcs_ok: sel & 4 == 0 }
            }
            _ => Event::Frame { data: payload.to_vec(), fcs_ok: sel & 8 == 0 },
        };
        let was_done = rx.is_done();
        let out = rx.on_event(now, ev);
        assert!(!was_done || out.is_empty());
        for a in out {
            match a {
                Action::StartTimer { timer, .. } => armed[Timer::ALL.iter().position(|t| *t == timer).unwrap()] = true,
                Action::CancelTimer(t) => {
                    let i = Timer::ALL.iter().position(|x| *x == t).unwrap();
                    assert!(armed[i], "cancel of a timer that is not running");
                    armed[i] = false;
                }
                Action::SendFrames(frames) => {
                    assert!(!frames.is_empty());
                    for f in &frames {
                        assert!(Frame::parse(f).is_ok());
                    }
                }
                Action::Page(p) => assert_eq!(p.bitmap.len() as u32, p.rows * p.width.div_ceil(8)),
                Action::Done(r) => {
                    dones += 1;
                    assert_eq!(r.bad_lines_per_page.len() as u32, r.pages);
                }
                _ => {}
            }
        }
    }
    assert!(dones <= 1);
});
