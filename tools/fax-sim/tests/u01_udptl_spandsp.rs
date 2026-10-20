//! U01 (M2a): the `t38` crate's UDPTL against SpanDSP's reference UDPTL implementation
//! (`tests/udptl.c` of SpanDSP v3.1.1, compiled by `spandsp-sys/build.rs`; libspandsp itself
//! exports no UDPTL API).
//!
//! * byte-for-byte: our `UdptlSender` output equals `udptl_build_packet` output for the same
//!   IFP sequence, for redundancy depth 0-4 and several FEC (span, entries) settings;
//! * our encoder -> SpanDSP decoder: SpanDSP's `udptl_rx_packet` hands the right IFP packets to
//!   its handler, including packets it repaired from redundancy / FEC;
//! * SpanDSP encoder -> our decoder and `UdptlReceiver`.

use spandsp_sys as sys;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::raw::c_int;
use t38::{ErrorRecovery, RxEvent, Source, UdptlPacket, UdptlReceiver, UdptlSender};

type Delivered = RefCell<Vec<(i32, Vec<u8>)>>;

unsafe extern "C" fn on_ifp(user: *mut c_void, msg: *const u8, len: c_int, seq: c_int) -> c_int {
    // SAFETY: `user` is the boxed `Delivered` owned by `RefUdptl`; msg/len describe a valid slice.
    let d = unsafe { &*(user as *const Delivered) };
    let bytes = if len > 0 {
        unsafe { std::slice::from_raw_parts(msg, len as usize) }.to_vec()
    } else {
        Vec::new()
    };
    d.borrow_mut().push((seq, bytes));
    0
}

/// RAII wrapper around SpanDSP's reference `udptl_state_t`.
struct RefUdptl {
    ptr: *mut sys::udptl_state_t,
    delivered: Box<Delivered>,
}

impl RefUdptl {
    fn new(scheme: c_int, span: c_int, entries: c_int) -> Self {
        let delivered: Box<Delivered> = Box::new(RefCell::new(Vec::new()));
        // SAFETY: NULL state => SpanDSP allocates; the box outlives the state (dropped after free).
        let ptr = unsafe {
            sys::udptl_init(
                std::ptr::null_mut(),
                scheme,
                span,
                entries,
                on_ifp,
                &*delivered as *const Delivered as *mut c_void,
            )
        };
        assert!(!ptr.is_null(), "udptl_init failed");
        Self { ptr, delivered }
    }

    fn build(&mut self, ifp: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 2048];
        // SAFETY: buf has room for any datagram (reference limit is 400 octets of IFP).
        let n = unsafe {
            sys::udptl_build_packet(self.ptr, buf.as_mut_ptr(), ifp.as_ptr(), ifp.len() as c_int)
        };
        assert!(n > 0, "udptl_build_packet returned {n}");
        buf.truncate(n as usize);
        buf
    }

    fn rx(&mut self, dg: &[u8]) -> c_int {
        // SAFETY: valid slice for the duration of the call.
        unsafe { sys::udptl_rx_packet(self.ptr, dg.as_ptr(), dg.len() as c_int) }
    }

    /// Latest bytes delivered per sequence number.
    fn take_by_seq(&self) -> BTreeMap<i32, Vec<u8>> {
        self.delivered.borrow_mut().drain(..).collect()
    }
}

impl Drop for RefUdptl {
    fn drop(&mut self) {
        // SAFETY: freed exactly once.
        unsafe { sys::udptl_free(self.ptr) };
    }
}

/// A recognisable IFP-like payload of varying length (1..=29 octets).
fn ifp(i: usize) -> Vec<u8> {
    let len = 1 + (i * 7) % 29;
    (0..len)
        .map(|k| ((i * 31 + k * 13 + 1) & 0xFF) as u8)
        .collect()
}

const FEC_SETTINGS: [(u8, u8); 6] = [(1, 1), (2, 1), (3, 1), (2, 2), (3, 2), (4, 3)];

#[test]
fn u01_redundancy_bytes_identical_to_spandsp() {
    let mut total = 0;
    for depth in 0u8..=4 {
        let mut ours =
            UdptlSender::new(ErrorRecovery::Redundancy { depth }, 0).with_max_datagram(400);
        let mut theirs = RefUdptl::new(
            sys::UDPTL_ERROR_CORRECTION_REDUNDANCY,
            0,
            c_int::from(depth),
        );
        for i in 0..20 {
            let (a, b) = (ours.build(&ifp(i)).unwrap(), theirs.build(&ifp(i)));
            assert_eq!(
                a, b,
                "depth {depth} packet {i}\n ours   {a:02x?}\n spandsp {b:02x?}"
            );
            total += 1;
            // and the decoder sees what we expect
            let p = UdptlPacket::decode(&b).unwrap();
            assert_eq!(p.seq, i as u16);
            assert_eq!(p.primary, ifp(i));
        }
    }
    // error correction NONE (SpanDSP) == empty secondary list (ours)
    let mut ours = UdptlSender::new(ErrorRecovery::None, 0);
    let mut theirs = RefUdptl::new(sys::UDPTL_ERROR_CORRECTION_NONE, 0, 0);
    for i in 0..5 {
        assert_eq!(
            ours.build(&ifp(i)).unwrap(),
            theirs.build(&ifp(i)),
            "none packet {i}"
        );
        total += 1;
    }
    eprintln!("U01 redundancy: {total} datagrams byte-identical (depth 0-4 + none)");
}

#[test]
fn u01_fec_bytes_identical_to_spandsp() {
    let mut total = 0;
    for (span, entries) in FEC_SETTINGS {
        let mut ours = UdptlSender::new(ErrorRecovery::Fec { span, entries }, 0);
        let mut theirs = RefUdptl::new(
            sys::UDPTL_ERROR_CORRECTION_FEC,
            c_int::from(span),
            c_int::from(entries),
        );
        for i in 0..30 {
            let (a, b) = (ours.build(&ifp(i)).unwrap(), theirs.build(&ifp(i)));
            assert_eq!(
                a, b,
                "fec {span}x{entries} packet {i}\n ours   {a:02x?}\n spandsp {b:02x?}"
            );
            total += 1;
        }
    }
    eprintln!(
        "U01 fec: {total} datagrams byte-identical across {} (span, entries) settings",
        FEC_SETTINGS.len()
    );
    // show one excerpt for the report
    let mut ours = UdptlSender::new(
        ErrorRecovery::Fec {
            span: 2,
            entries: 1,
        },
        0,
    );
    let dgs: Vec<_> = (0..4)
        .map(|i| ours.build(&[0x10 + i as u8, 0x20 + i as u8]).unwrap())
        .collect();
    eprintln!("U01 fec 2x1 excerpt, seq 3: {:02x?}", dgs[3]);
}

#[test]
fn u01_our_encoder_to_spandsp_decoder_with_redundancy_loss() {
    let mut ours =
        UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 0).with_max_datagram(400);
    let mut theirs = RefUdptl::new(sys::UDPTL_ERROR_CORRECTION_REDUNDANCY, 0, 3);
    let lost = [4usize, 5, 6, 11, 15, 16];
    for i in 0..20 {
        let dg = ours.build(&ifp(i)).unwrap();
        if lost.contains(&i) {
            continue;
        }
        assert_eq!(theirs.rx(&dg), 0, "SpanDSP rejected our datagram {i}");
    }
    let got = theirs.take_by_seq();
    for i in 0..20 {
        assert_eq!(
            got.get(&(i as i32)),
            Some(&ifp(i)),
            "packet {i} missing/different in SpanDSP"
        );
    }
    eprintln!(
        "U01 ours->SpanDSP redundancy: all 20 IFP packets delivered by SpanDSP despite losses {lost:?}"
    );
}

#[test]
fn u01_our_encoder_to_spandsp_decoder_with_fec_loss() {
    let (span, entries) = (3u8, 1u8);
    let mut ours = UdptlSender::new(ErrorRecovery::Fec { span, entries }, 0);
    let mut theirs = RefUdptl::new(
        sys::UDPTL_ERROR_CORRECTION_FEC,
        i32::from(span),
        i32::from(entries),
    );
    let lost = [7usize, 12];
    for i in 0..20 {
        let dg = ours.build(&ifp(i)).unwrap();
        if lost.contains(&i) {
            continue;
        }
        assert_eq!(theirs.rx(&dg), 0, "SpanDSP rejected our FEC datagram {i}");
    }
    let got = theirs.take_by_seq();
    for i in 0..20 {
        let want = ifp(i);
        let g = got
            .get(&(i as i32))
            .unwrap_or_else(|| panic!("packet {i} not delivered by SpanDSP"));
        // a FEC-rebuilt packet has the length of the longest packet of its set
        assert!(g.len() >= want.len(), "packet {i}");
        assert_eq!(&g[..want.len()], &want[..], "packet {i}");
        assert!(
            g[want.len()..].iter().all(|&b| b == 0),
            "packet {i} padding"
        );
        if !lost.contains(&i) {
            assert_eq!(g, &want, "received packet {i} must be exact");
        }
    }
    eprintln!("U01 ours->SpanDSP fec 3x1: lost {lost:?} repaired by SpanDSP's decoder");
}

fn feed(rx: &mut UdptlReceiver, dgs: &[Vec<u8>], lost: &[usize]) -> Vec<RxEvent> {
    let mut ev = Vec::new();
    for (i, d) in dgs.iter().enumerate() {
        if !lost.contains(&i) {
            ev.extend(rx.on_datagram(d).unwrap());
        }
    }
    ev.extend(rx.flush());
    ev
}

#[test]
fn u01_spandsp_encoder_to_our_receiver_redundancy() {
    let mut theirs = RefUdptl::new(sys::UDPTL_ERROR_CORRECTION_REDUNDANCY, 0, 3);
    let dgs: Vec<Vec<u8>> = (0..20).map(|i| theirs.build(&ifp(i))).collect();
    let lost = [4usize, 5, 6, 11, 15, 16];
    let ev = feed(&mut UdptlReceiver::default(), &dgs, &lost);
    let pk: Vec<(u16, Vec<u8>, Source)> = ev
        .iter()
        .filter_map(|e| match e {
            RxEvent::Packet { seq, ifp, source } => Some((*seq, ifp.clone(), *source)),
            RxEvent::Gap { .. } => None,
        })
        .collect();
    assert_eq!(pk.len(), 20);
    for (i, (seq, data, src)) in pk.iter().enumerate() {
        assert_eq!(*seq as usize, i);
        assert_eq!(*data, ifp(i));
        assert_eq!(*src == Source::Redundancy, lost.contains(&i), "packet {i}");
    }
    eprintln!(
        "U01 SpanDSP->ours redundancy: 20/20 delivered in order, {} via redundancy",
        lost.len()
    );
}

#[test]
fn u01_spandsp_encoder_to_our_receiver_fec() {
    for (span, entries) in [(3u8, 1u8), (2, 2), (4, 3)] {
        let mut theirs = RefUdptl::new(
            sys::UDPTL_ERROR_CORRECTION_FEC,
            i32::from(span),
            i32::from(entries),
        );
        let dgs: Vec<Vec<u8>> = (0..30).map(|i| theirs.build(&ifp(i))).collect();
        // one loss every (span*entries + 1) packets, away from the start
        let period = usize::from(span) * usize::from(entries) + 1;
        let lost: Vec<usize> = (0..29)
            .filter(|i| *i >= 2 * period && (i - 2 * period) % period == 0)
            .collect();
        let ev = feed(&mut UdptlReceiver::default(), &dgs, &lost);
        let mut seqs = Vec::new();
        for e in &ev {
            match e {
                RxEvent::Packet {
                    seq,
                    ifp: data,
                    source,
                } => {
                    let want = ifp(usize::from(*seq));
                    assert_eq!(
                        &data[..want.len()],
                        &want[..],
                        "fec {span}x{entries} seq {seq}"
                    );
                    assert!(data[want.len()..].iter().all(|&b| b == 0));
                    assert_eq!(*source == Source::Fec, lost.contains(&usize::from(*seq)));
                    seqs.push(*seq);
                }
                RxEvent::Gap { count } => panic!("fec {span}x{entries}: unexpected gap {count}"),
            }
        }
        assert_eq!(seqs, (0..30).collect::<Vec<u16>>());
        eprintln!(
            "U01 SpanDSP->ours fec {span}x{entries}: lost {lost:?} all repaired, 30/30 in order"
        );
    }
}
