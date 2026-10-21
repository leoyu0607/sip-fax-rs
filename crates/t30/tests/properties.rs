//! Property tests: parsing never panics, the state machine is total and keeps its invariants
//! under arbitrary event sequences, DIS generation round-trips.

use proptest::prelude::*;
use t30::{
    Action, Config, Dcs, Dis, Event, Frame, MinScanTime, Modems, PaperLength, PaperWidth, Receiver,
    RxSignal, State, TcfMode, Timer, build_dis_fif, decode_ident, encode_ident,
};

fn arb_frame() -> impl Strategy<Value = Vec<u8>> {
    let fcf = prop::sample::select(vec![
        0x01u8, 0x02, 0x41, 0xC1, 0x42, 0xC2, 0x21, 0x22, 0x31, 0x32, 0x33, 0x58, 0xD8, 0x5F, 0xDF,
        0x71, 0xF1, 0x72, 0xF2, 0x74, 0xF4, 0x7D, 0xFD, 0x81, 0x53, 0x00, 0xFF,
    ]);
    (
        any::<bool>(),
        any::<bool>(),
        fcf,
        prop::collection::vec(any::<u8>(), 0..40),
    )
        .prop_map(|(fin, good_header, fcf, fif)| {
            let mut v = vec![0xFF, if fin { 0xC8 } else { 0xC0 }, fcf];
            if !good_header {
                v[0] = 0x00;
            }
            v.extend(fif);
            v
        })
}

fn arb_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        1 => Just(Event::Start),
        6 => Just(Event::TxComplete),
        1 => Just(Event::Abort),
        4 => Just(Event::DataEnd),
        4 => prop::collection::vec(any::<u8>(), 0..300).prop_map(Event::ImageData),
        4 => prop::collection::vec(Just(0u8), 0..3000).prop_map(Event::ImageData),
        12 => (arb_frame(), any::<bool>()).prop_map(|(data, fcs_ok)| Event::Frame { data, fcs_ok }),
        3 => prop::sample::select(vec![RxSignal::Cng, RxSignal::Ced, RxSignal::V21Preamble, RxSignal::Training, RxSignal::NoSignal]).prop_map(Event::Signal),
        6 => prop::sample::select(Timer::ALL.to_vec()).prop_map(Event::Timer),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn parsers_are_total(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = Frame::parse(&bytes);
        let _ = Dcs::parse(&bytes);
        let d = Dis::parse(&bytes);
        let _ = (d.v17(), d.v29(), d.v27ter(), d.v33());
        let id = decode_ident(&bytes);
        prop_assert!(id.len() <= 20);
        prop_assert!(!t30::fif_bit(&bytes, 0));
    }

    #[test]
    fn ident_round_trips(s in "[0-9+][0-9+ ]{0,18}[0-9+]") {
        prop_assert_eq!(decode_ident(&encode_ident(&s)), s);
    }

    #[test]
    fn receiver_is_total_and_consistent(events in prop::collection::vec(arb_event(), 0..60), tcf_local in any::<bool>()) {
        let cfg = Config { tcf: if tcf_local { TcfMode::Local } else { TcfMode::Transferred }, ..Config::default() };
        let mut rx = Receiver::new(cfg);
        let mut now = 0u64;
        let mut dones = 0;
        let mut last_page = 0;
        let mut armed: [bool; 4] = [false; 4];
        let idx = |t: Timer| Timer::ALL.iter().position(|x| *x == t).unwrap();
        for ev in events {
            now += 7;
            let was_done = rx.is_done();
            let out = rx.on_event(now, ev);
            if was_done {
                prop_assert!(out.is_empty(), "actions after Done");
            }
            for a in &out {
                match a {
                    Action::StartTimer { timer, ms } => { armed[idx(*timer)] = true; prop_assert!(*ms > 0); }
                    Action::CancelTimer(t) => { prop_assert!(armed[idx(*t)], "cancel of a timer that is not running"); armed[idx(*t)] = false; }
                    Action::Page(p) => {
                        prop_assert_eq!(p.number, last_page + 1);
                        last_page = p.number;
                        prop_assert_eq!(p.bitmap.len() as u32, p.rows * p.width.div_ceil(8));
                    }
                    Action::Done(r) => {
                        dones += 1;
                        prop_assert_eq!(r.pages, last_page);
                        prop_assert_eq!(r.bad_lines_per_page.len() as u32, r.pages);
                    }
                    Action::SendFrames(f) => {
                        prop_assert!(!f.is_empty());
                        for fr in f { prop_assert!(Frame::parse(fr).is_ok()); }
                    }
                    _ => {}
                }
            }
            prop_assert_eq!(rx.is_done(), rx.state() == State::Done);
        }
        prop_assert!(dones <= 1);
        prop_assert_eq!(dones == 1, rx.is_done());
    }
}

fn arb_config() -> impl Strategy<Value = Config> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        0u8..3,
        0u8..3,
        0u8..8,
        any::<bool>(),
    )
        .prop_map(|(v27, v29, v17, fine, mr, mmr, w, l, scan, t38)| Config {
            modems: Modems {
                v27ter: v27,
                v29,
                v17,
            },
            fine,
            mr,
            mmr,
            max_width: [PaperWidth::A4, PaperWidth::B4, PaperWidth::A3][w as usize],
            max_length: [PaperLength::A4, PaperLength::B4, PaperLength::Unlimited][l as usize],
            min_scan: MinScanTime::from_code(scan),
            t38_iaf: t38,
            ..Config::default()
        })
}

proptest! {
    #[test]
    fn dis_round_trips_for_any_configuration(cfg in arb_config()) {
        let fif = build_dis_fif(&cfg);
        prop_assert_eq!(fif.len(), if cfg.mmr { 4 } else { 3 });
        let d = Dis::parse(&fif);
        prop_assert_eq!((d.fine, d.mr, d.mmr, d.t38_iaf), (cfg.fine, cfg.mr, cfg.mmr, cfg.t38_iaf));
        prop_assert_eq!((d.width, d.length, d.min_scan), (cfg.max_width, cfg.max_length, cfg.min_scan));
        prop_assert!(d.can_receive);
        prop_assert!(!d.ecm && !d.uncompressed);
        prop_assert_eq!(d.v17(), cfg.modems.v17);
        // V.17 always comes with V.29 and V.27ter
        if d.v17() { prop_assert!(d.v29() && d.v27ter()); }
        // the DIS is always a valid frame body
        let mut frame = vec![0xFF, 0xC8, 0x01];
        frame.extend(&fif);
        prop_assert!(Frame::parse(&frame).is_ok());
    }
}
