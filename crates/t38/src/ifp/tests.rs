use super::*;

const V: [T38Version; 4] = [
    T38Version::V0,
    T38Version::V1,
    T38Version::V2,
    T38Version::V3,
];

fn hdlc(data: &[u8]) -> Ifp {
    Ifp::Data {
        data_type: DataType::V21,
        fields: vec![
            Field::new(FieldType::HdlcData, data),
            Field::new(FieldType::HdlcFcsOkSigEnd, vec![]),
        ],
    }
}

#[test]
fn indicator_bytes_match_asn1() {
    // dfp=0, choice=0, ext=0, value<<1: matches SpanDSP's `indicator << 1`.
    assert_eq!(
        Ifp::Indicator(Indicator::Cng)
            .encode(T38Version::V0)
            .unwrap(),
        [0x02]
    );
    assert_eq!(
        Ifp::Indicator(Indicator::Ced)
            .encode(T38Version::V3)
            .unwrap(),
        [0x04]
    );
    assert_eq!(
        Ifp::Indicator(Indicator::V21Preamble)
            .encode(T38Version::V3)
            .unwrap(),
        [0x06]
    );
    assert_eq!(
        Ifp::Indicator(Indicator::V17_14400LongTraining)
            .encode(T38Version::V3)
            .unwrap(),
        [0x1E]
    );
    // Addition: ext=1, small-number flag 0, 6 bits index => 0x20 | index>>2, index<<6.
    assert_eq!(
        Ifp::Indicator(Indicator::V8Ansam)
            .encode(T38Version::V3)
            .unwrap(),
        [0x20, 0x00]
    );
    assert_eq!(
        Ifp::Indicator(Indicator::V33_14400Training)
            .encode(T38Version::V3)
            .unwrap(),
        [0x21, 0x80]
    );
}

#[test]
fn data_bytes_match_asn1() {
    // v21 + [hdlc-data(0xFF 0x03), hdlc-fcs-OK-sig-end(no data)], v3:
    // 0x80|0x40 (dfp,data) 0<<1 ; count 2 ; field1: 1,0,000 -> 0x80 ; len-1 = 0x0001 ; FF 03 ;
    // field2: 0,0,100 -> 0x20
    assert_eq!(
        hdlc(&[0xFF, 0x03]).encode(T38Version::V3).unwrap(),
        [0xC0, 0x02, 0x80, 0x00, 0x01, 0xFF, 0x03, 0x20]
    );
    // v0: field type has no extension bit: 1,000 -> 0x80 ; sig-end no-data: 0,100 -> 0x40.
    assert_eq!(
        hdlc(&[0xFF, 0x03]).encode(T38Version::V0).unwrap(),
        [0xC0, 0x02, 0x80, 0x00, 0x01, 0xFF, 0x03, 0x40]
    );
    // v0 packs two data-less fields into one octet (4 bits each).
    let two = Ifp::Data {
        data_type: DataType::V29_9600,
        fields: vec![
            Field::new(FieldType::HdlcSigEnd, vec![]),
            Field::new(FieldType::T4NonEcmSigEnd, vec![]),
        ],
    };
    assert_eq!(two.encode(T38Version::V0).unwrap(), [0xC8, 0x02, 0x17]);
    // v1+: 5 bits each => 0 0 001 | 0 0 111 packed -> 0b0000_1001, 0b1100_0000.
    assert_eq!(
        two.encode(T38Version::V3).unwrap(),
        [0xC8, 0x02, 0x09, 0xC0]
    );
    assert_eq!(
        Ifp::decode(&[0xC8, 0x02, 0x09, 0xC0], T38Version::V3).unwrap(),
        two
    );
    assert_eq!(
        Ifp::decode(&[0xC8, 0x02, 0x17], T38Version::V0).unwrap(),
        two
    );
}

#[test]
fn field_type_additions_use_pure_per() {
    let f = |ft| Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(ft, vec![0xAA])],
    };
    // present=1, ext=1, flag=0, index 0 in 5+1 bits, padded; then len-1 = 0, data.
    assert_eq!(
        f(FieldType::CmMessage).encode(T38Version::V3).unwrap(),
        [0xC0, 0x01, 0xC0, 0x00, 0x00, 0x00, 0xAA]
    );
    // index 1 -> bit0 of the 6-bit number lands in the MSB of the next octet.
    assert_eq!(
        f(FieldType::JmMessage).encode(T38Version::V3).unwrap(),
        [0xC0, 0x01, 0xC0, 0x80, 0x00, 0x00, 0xAA]
    );
    // index 2 -> bit 1 of the 6-bit number is the last bit of the first octet.
    assert_eq!(
        f(FieldType::CiMessage).encode(T38Version::V3).unwrap(),
        [0xC0, 0x01, 0xC1, 0x00, 0x00, 0x00, 0xAA]
    );
}

#[test]
fn roundtrip_every_combination() {
    for v in V {
        for ind in Indicator::ALL {
            let pkt = Ifp::Indicator(*ind);
            match pkt.encode(v) {
                Ok(b) => assert_eq!(Ifp::decode(&b, v).unwrap(), pkt, "{ind:?} {v:?}"),
                Err(e) => {
                    assert!(
                        ind.is_addition() && !v.allows_additions(),
                        "{ind:?} {v:?} {e}"
                    )
                }
            }
        }
        for dt in DataType::ALL {
            for ft in FieldType::ALL {
                for payload in [vec![], vec![1u8, 2, 3]] {
                    let pkt = Ifp::Data {
                        data_type: *dt,
                        fields: vec![
                            Field::new(*ft, payload.clone()),
                            Field::new(FieldType::HdlcSigEnd, vec![]),
                            Field::new(*ft, vec![9]),
                        ],
                    };
                    let allowed = v.allows_additions() || !(dt.is_addition() || ft.is_addition());
                    match pkt.encode(v) {
                        Ok(b) => {
                            assert!(allowed, "{dt:?} {ft:?} {v:?} should be rejected");
                            assert_eq!(Ifp::decode(&b, v).unwrap(), pkt);
                        }
                        Err(e) => {
                            assert!(!allowed, "{dt:?} {ft:?} {v:?}: {e}");
                            assert!(matches!(e, IfpError::NotInVersion(_)));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn v0_decoder_rejects_additions() {
    // v8-ansam on the wire, decoded as v0 / v1.
    for v in [T38Version::V0, T38Version::V1] {
        assert!(matches!(
            Ifp::decode(&[0x20, 0x00], v),
            Err(IfpError::NotInVersion(_))
        ));
        // t30-data v8 addition.
        assert!(matches!(
            Ifp::decode(&[0x60, 0x00], v),
            Err(IfpError::NotInVersion(_))
        ));
    }
    assert_eq!(
        Ifp::decode(&[0x20, 0x00], T38Version::V2).unwrap(),
        Ifp::Indicator(Indicator::V8Ansam)
    );
    // field-type addition under v1 (extension bit exists but additions are v2+).
    let cm = [0xC0, 0x01, 0xC0, 0x00, 0x00, 0x00, 0xAA];
    assert!(matches!(
        Ifp::decode(&cm, T38Version::V1),
        Err(IfpError::NotInVersion(_))
    ));
    // Under v0 the same octets parse as 3-bit field types and fail differently.
    assert!(Ifp::decode(&cm, T38Version::V0).is_err());
}

#[test]
fn malformed_inputs() {
    use IfpError::*;
    let v = T38Version::V3;
    assert!(matches!(Ifp::decode(&[], v), Err(Per(PerError::Truncated))));
    // indicator with data-field bit
    assert_eq!(Ifp::decode(&[0x82], v), Err(DataFieldWithIndicator));
    // t30-data root value 9 is undefined (root has 9 values: 0..=8)
    assert!(matches!(
        Ifp::decode(&[0x40 | (9 << 1)], v),
        Err(UndefinedRoot { .. })
    ));
    // unknown addition index 63 and the >=64 form
    assert!(matches!(
        Ifp::decode(&[0x23, 0xC0], v),
        Err(UnknownExtension { .. })
    ));
    assert!(matches!(
        Ifp::decode(&[0x30, 0x00], v),
        Err(UnknownExtension { .. })
    ));
    // trailing octet
    assert_eq!(Ifp::decode(&[0x02, 0x00], v), Err(TrailingData(1)));
    assert_eq!(
        Ifp::decode_padded(&[0x02, 0x00, 0x00], v).unwrap(),
        Ifp::Indicator(Indicator::Cng)
    );
    assert_eq!(
        Ifp::decode_padded(&[0x02, 0x00, 0x01], v),
        Err(TrailingData(2))
    );
    // count larger than the input
    assert!(matches!(
        Ifp::decode(&[0xC0, 0x7F, 0x80], v),
        Err(Per(PerError::Truncated))
    ));
    // field data truncated
    assert!(matches!(
        Ifp::decode(&[0xC0, 0x01, 0x80, 0x00, 0x05, 0xFF], v),
        Err(Per(PerError::Truncated))
    ));
    // invalid fragment header
    assert!(matches!(
        Ifp::decode(&[0xC0, 0xC9], v),
        Err(Per(PerError::InvalidFragment(0xC9)))
    ));
    // data with no data field decodes to an empty field list
    assert_eq!(
        Ifp::decode(&[0x42], v).unwrap(),
        Ifp::Data {
            data_type: DataType::V27ter2400,
            fields: vec![]
        }
    );
}

#[test]
fn field_too_long_is_rejected() {
    let pkt = Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(FieldType::HdlcData, vec![0u8; 65536])],
    };
    assert_eq!(
        pkt.encode(T38Version::V3),
        Err(IfpError::FieldTooLong(65536))
    );
    let ok = Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(FieldType::HdlcData, vec![7u8; 65535])],
    };
    let b = ok.encode(T38Version::V3).unwrap();
    assert_eq!(&b[..5], [0xC0, 0x01, 0x80, 0xFF, 0xFE]);
    assert_eq!(Ifp::decode(&b, T38Version::V3).unwrap(), ok);
}

/// Builds a packet with `n` data-less fields.
fn many(n: usize) -> Ifp {
    Ifp::Data {
        data_type: DataType::V29_9600,
        fields: (0..n)
            .map(|_| Field::new(FieldType::HdlcSigEnd, vec![]))
            .collect(),
    }
}

#[test]
fn i05_length_determinant_boundaries() {
    for v in [T38Version::V0, T38Version::V3] {
        for (n, header) in [
            (1usize, vec![0x01u8]),
            (127, vec![0x7F]),
            (128, vec![0x80, 0x80]),
            (16383, vec![0xBF, 0xFF]),
            (16384, vec![0xC1]),
            (16385, vec![0xC1]),
            (65536, vec![0xC4]),
        ] {
            let pkt = many(n);
            let b = pkt.encode(v).unwrap();
            assert_eq!(&b[1..1 + header.len()], &header[..], "count {n} {v:?}");
            assert_eq!(Ifp::decode(&b, v).unwrap(), pkt, "count {n} {v:?}");
        }
    }
    // Exact multiple of 16384 ends with an empty chunk.
    let b = many(16384).encode(T38Version::V3).unwrap();
    let bits: usize = 16384 * 5;
    assert_eq!(b.len(), 1 + 1 + bits.div_ceil(8) + 1);
    assert_eq!(b.last(), Some(&0x00));
}

#[test]
fn decode_prefix_reports_used_length() {
    let enc = hdlc(&[1, 2, 3]).encode(T38Version::V3).unwrap();
    let mut padded = enc.clone();
    padded.extend_from_slice(&[0, 0]);
    let (ifp, used) = Ifp::decode_prefix(&padded, T38Version::V3).unwrap();
    assert_eq!(used, enc.len());
    assert_eq!(ifp, hdlc(&[1, 2, 3]));
}

#[test]
fn version_helpers() {
    for n in 0..4 {
        assert_eq!(T38Version::from_number(n).unwrap().number(), n);
    }
    assert_eq!(T38Version::from_number(4), None);
    assert!(!T38Version::V0.has_field_type_extension_bit());
    assert!(T38Version::V1.has_field_type_extension_bit());
    assert!(!T38Version::V1.allows_additions());
    assert!(T38Version::V2.allows_additions());
}
