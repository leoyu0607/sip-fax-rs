//! Property tests: I02/I04 (IFP) and U05 (UDPTL stream under loss, reordering, duplication).

use proptest::prelude::*;
use t38::{
    DataType, ErrorRecovery, Field, FieldType, Ifp, Indicator, ReceiverConfig, RxEvent, Source,
    T38Version, UdptlPacket, UdptlReceiver, UdptlRecovery, UdptlSender,
};

const VERSIONS: [T38Version; 4] = [
    T38Version::V0,
    T38Version::V1,
    T38Version::V2,
    T38Version::V3,
];

fn version() -> impl Strategy<Value = T38Version> {
    prop::sample::select(VERSIONS.to_vec())
}

fn arb_ifp() -> impl Strategy<Value = Ifp> {
    let ind = prop::sample::select(Indicator::ALL.to_vec()).prop_map(Ifp::Indicator);
    let field = (
        prop::sample::select(FieldType::ALL.to_vec()),
        prop::collection::vec(any::<u8>(), 0..40),
    )
        .prop_map(|(field_type, data)| Field { field_type, data });
    let data = (
        prop::sample::select(DataType::ALL.to_vec()),
        prop::collection::vec(field, 0..6),
    )
        .prop_map(|(data_type, fields)| Ifp::Data { data_type, fields });
    prop_oneof![ind, data]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// I02/I04: whatever the encoder accepts decodes back to the same value; whatever it
    /// refuses is an addition used with a version that lacks it.
    #[test]
    fn ifp_encode_decode_roundtrip(pkt in arb_ifp(), v in version()) {
        match pkt.encode(v) {
            Ok(bytes) => {
                prop_assert_eq!(Ifp::decode(&bytes, v).unwrap(), pkt.clone());
                // trailing zero padding is accepted by decode_padded only
                let mut padded = bytes.clone();
                padded.extend_from_slice(&[0, 0, 0]);
                prop_assert!(Ifp::decode(&padded, v).is_err());
                prop_assert_eq!(Ifp::decode_padded(&padded, v).unwrap(), pkt);
            }
            Err(e) => {
                prop_assert!(!v.allows_additions(), "{e}");
            }
        }
    }

    /// I04: arbitrary bytes never panic; if they decode, re-encoding and decoding again is a
    /// fixed point (the encoding is canonical up to ignorable padding bits).
    #[test]
    fn ifp_decode_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..200), v in version()) {
        if let Ok((pkt, used)) = Ifp::decode_prefix(&bytes, v) {
            prop_assert!(used <= bytes.len());
            let enc = pkt.encode(v).unwrap();
            let (again, used2) = Ifp::decode_prefix(&enc, v).unwrap();
            prop_assert_eq!(again, pkt);
            prop_assert_eq!(used2, enc.len());
        }
    }

    /// I04: every strict prefix of a valid packet is an error.
    #[test]
    fn ifp_truncations_are_errors(pkt in arb_ifp(), v in version()) {
        if let Ok(bytes) = pkt.encode(v) {
            for cut in 0..bytes.len() {
                prop_assert!(Ifp::decode(&bytes[..cut], v).is_err(), "cut {cut} of {}", bytes.len());
            }
        }
    }

    /// Mutating one byte of a valid packet never panics.
    #[test]
    fn ifp_mutations_do_not_panic(pkt in arb_ifp(), v in version(), pos in any::<usize>(), val in any::<u8>()) {
        if let Ok(mut bytes) = pkt.encode(v) {
            let n = bytes.len();
            bytes[pos % n] = val;
            let _ = Ifp::decode(&bytes, v);
        }
    }

    #[test]
    fn udptl_decode_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        if let Ok(pkt) = UdptlPacket::decode(&bytes)
            && !pkt.primary.is_empty() {
                let enc = pkt.encode().unwrap();
                prop_assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt);
            }
        let mut rx = UdptlReceiver::default();
        let _ = rx.on_datagram(&bytes);
        let _ = rx.flush();
    }

    #[test]
    fn udptl_packet_roundtrip(
        seq in any::<u16>(),
        primary in prop::collection::vec(any::<u8>(), 1..60),
        sec in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..40), 0..6),
        span in any::<u32>(),
        fec in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..40), 0..6),
        use_fec in any::<bool>(),
    ) {
        let recovery = if use_fec {
            UdptlRecovery::Fec { span, data: fec }
        } else {
            UdptlRecovery::Secondary(sec)
        };
        let pkt = UdptlPacket { seq, primary, recovery };
        let enc = pkt.encode().unwrap();
        prop_assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt);
    }
}

fn payload(i: usize) -> Vec<u8> {
    // Varying lengths exercise FEC zero extension.
    let len = 1 + (i * 7) % 13;
    (0..len).map(|k| (i * 31 + k * 17 + 5) as u8).collect()
}

#[derive(Debug, Clone)]
struct Fate {
    lost: bool,
    dup: bool,
    jitter: u8,
}

fn arb_mode() -> impl Strategy<Value = ErrorRecovery> {
    prop_oneof![
        Just(ErrorRecovery::None),
        (0u8..=4).prop_map(|depth| ErrorRecovery::Redundancy { depth }),
        (1u8..=4, 1u8..=3).prop_map(|(span, entries)| ErrorRecovery::Fec { span, entries }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// U05: any pattern of loss, reordering (jitter <= 5 positions) and duplication yields an
    /// in-order stream without duplicates, where delivered + gap-counted packets exactly tile
    /// the range from the first to the last delivered sequence number.
    #[test]
    fn u05_stream_properties(
        mode in arb_mode(),
        first_seq in prop_oneof![0u16..100, 65500u16..=65535],
        fates in prop::collection::vec(
            (any::<bool>(), 0u8..10, any::<bool>(), 0u8..6).prop_map(|(l, p, dup, jitter)| {
                // ~20% loss
                Fate { lost: l && p < 4, dup, jitter }
            }),
            3..70,
        ),
    ) {
        let n = fates.len();
        let mut tx = UdptlSender::new(mode, first_seq);
        let datagrams: Vec<Vec<u8>> = (0..n).map(|i| tx.build(&payload(i)).unwrap()).collect();

        // arrival schedule
        let mut arrivals: Vec<(usize, usize)> = Vec::new(); // (key, packet index)
        for (i, f) in fates.iter().enumerate() {
            if f.lost { continue; }
            arrivals.push((i * 8 + usize::from(f.jitter) * 8 / 2, i));
            if f.dup { arrivals.push((i * 8 + 40, i)); }
        }
        arrivals.sort();

        let mut rx = UdptlReceiver::new(ReceiverConfig { reorder_window: 32, history: 256 });
        let mut events = Vec::new();
        for (_, i) in &arrivals {
            events.extend(rx.on_datagram(&datagrams[*i]).unwrap());
        }
        events.extend(rx.flush());

        // 1. contiguous tiling with gaps; no duplicates; order
        let mut cursor: Option<u16> = None;
        let mut delivered = 0u64;
        let mut gaps = 0u64;
        let mut delivered_idx = Vec::new();
        for e in &events {
            match e {
                RxEvent::Packet { seq, ifp, source } => {
                    if let Some(c) = cursor { prop_assert_eq!(*seq, c, "out of order / duplicate"); }
                    cursor = Some(seq.wrapping_add(1));
                    delivered += 1;
                    let idx = usize::from(seq.wrapping_sub(first_seq));
                    prop_assert!(idx < n);
                    delivered_idx.push(idx);
                    let orig = payload(idx);
                    match source {
                        Source::Fec => {
                            prop_assert!(ifp.len() >= orig.len());
                            prop_assert_eq!(&ifp[..orig.len()], &orig[..]);
                            prop_assert!(ifp[orig.len()..].iter().all(|&b| b == 0));
                        }
                        _ => prop_assert_eq!(ifp, &orig),
                    }
                }
                RxEvent::Gap { count } => {
                    prop_assert!(*count > 0);
                    gaps += u64::from(*count);
                    if let Some(c) = cursor { cursor = Some(c.wrapping_add(*count as u16)); }
                }
            }
        }

        // 2. delivered + gap == span from first to last delivered
        if let (Some(&lo), Some(&hi)) = (delivered_idx.first(), delivered_idx.last()) {
            prop_assert_eq!(delivered + gaps, (hi - lo + 1) as u64);
            prop_assert!(delivered_idx.windows(2).all(|w| w[0] < w[1]));
        }

        // 3. everything that arrived at or after the stream start is delivered (jitter and
        //    dups stay inside the window). Packets reordered *ahead of* the first datagram the
        //    receiver ever saw are older than its starting point and are dropped as late.
        if let Some(&lo) = delivered_idx.first() {
            for (_, i) in &arrivals {
                prop_assert!(*i < lo || delivered_idx.contains(i), "packet {i} arrived but was not delivered");
            }
        }

        // 4. redundancy: runs of losses up to the depth are fully repaired
        if let ErrorRecovery::Redundancy { depth } = mode {
            let mut run = 0usize;
            let mut max_run = 0usize;
            for f in &fates {
                if f.lost { run += 1; max_run = max_run.max(run); } else { run = 0; }
            }
            if max_run <= usize::from(depth) {
                prop_assert_eq!(gaps, 0, "loss runs <= depth must not produce gaps");
                if let Some(last) = arrivals.iter().map(|(_, i)| *i).max() {
                    prop_assert_eq!(delivered_idx.last().copied(), Some(last));
                }
            }
        }
        let s = rx.stats();
        prop_assert_eq!(s.delivered, delivered);
        prop_assert_eq!(s.lost, gaps);
    }

    /// With one loss per FEC set and no reordering, FEC delivers everything that was sent
    /// before the last datagram.
    #[test]
    fn u05_fec_single_loss_per_set(
        span in 2u8..=4,
        entries in 1u8..=3,
        n in 12usize..40,
        seed in any::<u64>(),
    ) {
        let mut tx = UdptlSender::new(ErrorRecovery::Fec { span, entries }, 0);
        let dg: Vec<_> = (0..n).map(|i| tx.build(&payload(i)).unwrap()).collect();
        // lose every packet whose index is a multiple of (span*entries + 1) offset by seed:
        // two losses are always at least span*entries+1 apart, so no set sees two of them.
        let period = usize::from(span) * usize::from(entries) + 1;
        let offset = (seed as usize % period) + period; // never lose the first few
        let mut rx = UdptlReceiver::default();
        let mut events = Vec::new();
        for (i, d) in dg.iter().enumerate() {
            if i >= offset && (i - offset).is_multiple_of(period) && i + 1 < n {
                continue;
            }
            events.extend(rx.on_datagram(d).unwrap());
        }
        events.extend(rx.flush());
        let seqs: Vec<u16> = events.iter().filter_map(|e| match e {
            RxEvent::Packet { seq, .. } => Some(*seq),
            RxEvent::Gap { .. } => None,
        }).collect();
        prop_assert_eq!(rx.stats().lost, 0, "events: {:?}", events.len());
        prop_assert_eq!(seqs, (0..n as u16).collect::<Vec<_>>());
    }
}
