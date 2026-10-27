use super::*;

fn ifp(i: u8) -> Vec<u8> {
    vec![0x06, i, i.wrapping_mul(3), 0xF0 | i]
}

#[test]
fn worked_example_redundancy_bytes() {
    // seq 1, primary [02], one secondary [06]: derived by hand from the ASN.1.
    let pkt = UdptlPacket {
        seq: 1,
        primary: vec![0x02],
        recovery: UdptlRecovery::Secondary(vec![vec![0x06]]),
    };
    let enc = pkt.encode().unwrap();
    assert_eq!(enc, [0x00, 0x01, 0x01, 0x02, 0x00, 0x01, 0x01, 0x06]);
    assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt);
}

#[test]
fn worked_example_fec_bytes() {
    // seq 0x0102, primary [AA BB], span 2, entries [[01 02 03], []].
    let pkt = UdptlPacket {
        seq: 0x0102,
        primary: vec![0xAA, 0xBB],
        recovery: UdptlRecovery::Fec {
            span: 2,
            data: vec![vec![1, 2, 3], vec![]],
        },
    };
    let enc = pkt.encode().unwrap();
    assert_eq!(
        enc,
        [
            0x01, 0x02, 0x02, 0xAA, 0xBB, 0x80, 0x01, 0x02, 0x02, 0x03, 1, 2, 3, 0x00
        ]
    );
    assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt);
}

#[test]
fn fec_integer_encoding_edges() {
    for (span, int) in [
        (0u32, vec![0x00u8]),
        (127, vec![0x7F]),
        (128, vec![0x00, 0x80]),
        (255, vec![0x00, 0xFF]),
        (256, vec![0x01, 0x00]),
        (0x8000_0000, vec![0x00, 0x80, 0, 0, 0]),
        (u32::MAX, vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF]),
    ] {
        let pkt = UdptlPacket {
            seq: 0,
            primary: vec![1],
            recovery: UdptlRecovery::Fec { span, data: vec![] },
        };
        let enc = pkt.encode().unwrap();
        assert_eq!(
            &enc[5..5 + 1 + int.len()],
            [&[int.len() as u8][..], &int[..]].concat()
        );
        assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt, "span {span}");
    }
}

#[test]
fn decode_rejects_bad_input() {
    let good = UdptlPacket {
        seq: 5,
        primary: vec![1, 2],
        recovery: UdptlRecovery::Secondary(vec![vec![3]]),
    }
    .encode()
    .unwrap();
    for cut in 0..good.len() {
        assert!(UdptlPacket::decode(&good[..cut]).is_err(), "cut {cut}");
    }
    let mut extra = good.clone();
    extra.push(0);
    assert_eq!(
        UdptlPacket::decode(&extra),
        Err(UdptlError::TrailingData(1))
    );
    // negative / empty / oversized fec integer
    for bad in [
        &[0x80u8, 0x01, 0x80][..],
        &[0x80, 0x00][..],
        &[0x80, 0x06, 1, 2, 3, 4, 5, 6][..],
    ] {
        let mut b = vec![0, 1, 1, 9];
        b.extend_from_slice(bad);
        assert_eq!(UdptlPacket::decode(&b), Err(UdptlError::BadInteger));
    }
    // 5-octet integer that overflows u32
    assert_eq!(
        UdptlPacket::decode(&[0, 1, 1, 9, 0x80, 0x05, 0x01, 0, 0, 0, 0, 0]),
        Err(UdptlError::BadInteger)
    );
    // huge count
    assert!(matches!(
        UdptlPacket::decode(&[0, 1, 1, 9, 0x00, 0x7F]),
        Err(UdptlError::CountTooLarge(127))
    ));
    assert_eq!(
        UdptlPacket {
            seq: 0,
            primary: vec![],
            recovery: UdptlRecovery::Secondary(vec![])
        }
        .encode(),
        Err(UdptlError::EmptyIfp)
    );
}

#[test]
fn large_primary_is_fragmented_and_roundtrips() {
    for len in [16383usize, 16384, 16385, 40000] {
        let pkt = UdptlPacket {
            seq: 9,
            primary: (0..len).map(|i| (i % 255) as u8).collect(),
            recovery: UdptlRecovery::Secondary(vec![]),
        };
        let enc = pkt.encode().unwrap();
        assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt, "len {len}");
    }
}

#[test]
fn redundancy_depth_zero_to_four() {
    for depth in 0u8..=4 {
        let mut tx = UdptlSender::new(ErrorRecovery::Redundancy { depth }, 0);
        for i in 0u8..8 {
            let enc = tx.build(&ifp(i)).unwrap();
            let pkt = UdptlPacket::decode(&enc).unwrap();
            assert_eq!(pkt.seq, u16::from(i));
            assert_eq!(pkt.primary, ifp(i));
            let UdptlRecovery::Secondary(list) = pkt.recovery else {
                panic!("expected redundancy")
            };
            let want = usize::from(i).min(usize::from(depth));
            assert_eq!(list.len(), want, "depth {depth} seq {i}");
            for (m, s) in list.iter().enumerate() {
                assert_eq!(*s, ifp(i - 1 - m as u8), "depth {depth} seq {i} entry {m}");
            }
        }
    }
}

#[test]
fn redundancy_respects_max_datagram() {
    let mut tx = UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 0).with_max_datagram(20);
    for i in 0u8..6 {
        let enc = tx.build(&ifp(i)).unwrap();
        assert!(enc.len() <= 20, "len {}", enc.len());
        let pkt = UdptlPacket::decode(&enc).unwrap();
        assert_eq!(pkt.primary, ifp(i));
    }
    let mut tx = UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 0).with_max_datagram(1);
    tx.build(&ifp(0)).unwrap();
    let enc = tx.build(&ifp(1)).unwrap();
    // primary is always sent, secondaries are dropped
    assert_eq!(
        UdptlPacket::decode(&enc).unwrap().recovery,
        UdptlRecovery::Secondary(vec![])
    );
}

#[test]
fn fec_ramp_and_xor_sets() {
    let (span, entries) = (3u8, 2u8);
    let mut tx = UdptlSender::new(ErrorRecovery::Fec { span, entries }, 65530);
    let mut sent: Vec<Vec<u8>> = Vec::new();
    for n in 0u8..14 {
        let p = ifp(n);
        let enc = tx.build(&p).unwrap();
        let pkt = UdptlPacket::decode(&enc).unwrap();
        assert_eq!(pkt.seq, 65530u16.wrapping_add(u16::from(n)));
        let UdptlRecovery::Fec { span: s, data } = pkt.recovery else {
            panic!("expected FEC")
        };
        let n = usize::from(n);
        // ramp-up exactly as SpanDSP / Asterisk
        let (exp_s, exp_e) = if n < 6 {
            (if n < 3 { 0 } else { 3 }, n / 3)
        } else {
            (3, 2)
        };
        assert_eq!((s as usize, data.len()), (exp_s, exp_e), "n={n}");
        for (m, entry) in data.iter().enumerate() {
            let mut acc = vec![0u8; 4];
            for k in 0..exp_s {
                let idx = n + m - exp_s * exp_e + k * exp_e;
                for (a, b) in acc.iter_mut().zip(&sent[idx]) {
                    *a ^= b;
                }
            }
            assert_eq!(*entry, acc, "n={n} m={m}");
        }
        sent.push(p);
    }
}

#[test]
fn fec_with_unequal_lengths_xors_zero_extended() {
    let mut tx = UdptlSender::new(
        ErrorRecovery::Fec {
            span: 2,
            entries: 1,
        },
        0,
    );
    let pk = [vec![0xFF], vec![0x0F, 0xF0, 0x11], vec![0x01]];
    let mut last = Vec::new();
    for p in &pk {
        last = tx.build(p).unwrap();
    }
    // seq 2: entry 0 protects packets 0 and 1 => [FF^0F, F0, 11]
    let UdptlRecovery::Fec { span, data } = UdptlPacket::decode(&last).unwrap().recovery else {
        panic!()
    };
    assert_eq!(span, 2);
    assert_eq!(data, vec![vec![0xF0, 0xF0, 0x11]]);
}

#[test]
fn fec_degenerate_params_and_mode_changes() {
    let mut tx = UdptlSender::new(
        ErrorRecovery::Fec {
            span: 0,
            entries: 3,
        },
        0,
    );
    let p = UdptlPacket::decode(&tx.build(&[1]).unwrap()).unwrap();
    assert_eq!(
        p.recovery,
        UdptlRecovery::Fec {
            span: 0,
            data: vec![]
        }
    );
    tx.set_mode(ErrorRecovery::None);
    let p = UdptlPacket::decode(&tx.build(&[2]).unwrap()).unwrap();
    assert_eq!(p.recovery, UdptlRecovery::Secondary(vec![]));
    assert_eq!(p.seq, 1);
    assert_eq!(tx.next_seq(), 2);
    assert_eq!(tx.build(&[]), Err(UdptlError::EmptyIfp));
    assert_eq!(tx.next_seq(), 2);
    assert_eq!(
        ErrorRecovery::default(),
        ErrorRecovery::Redundancy { depth: 3 }
    );
}

#[test]
fn sequence_wraps() {
    let mut tx = UdptlSender::new(ErrorRecovery::Redundancy { depth: 2 }, 65535);
    let a = UdptlPacket::decode(&tx.build(&[1]).unwrap()).unwrap();
    let b = UdptlPacket::decode(&tx.build(&[2]).unwrap()).unwrap();
    assert_eq!((a.seq, b.seq), (65535, 0));
    assert_eq!(b.recovery, UdptlRecovery::Secondary(vec![vec![1]]));
}
