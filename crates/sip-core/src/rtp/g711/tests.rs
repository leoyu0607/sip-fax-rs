use super::*;

#[test]
fn known_values() {
    // silence and full scale of both laws (G.711 tables, 16-bit scaling)
    assert_eq!(linear_to_ulaw(0), 0xFF);
    assert_eq!(ulaw_to_linear(0xFF), 0);
    assert_eq!(ulaw_to_linear(0x80), 32124);
    assert_eq!(ulaw_to_linear(0x00), -32124);
    assert_eq!(linear_to_alaw(0), 0xD5);
    assert_eq!(alaw_to_linear(0xD5), 8);
    assert_eq!(alaw_to_linear(0xAA), 32256);
    assert_eq!(alaw_to_linear(0x2A), -32256);
    assert_eq!(linear_to_ulaw(i16::MAX), 0x80);
    assert_eq!(linear_to_ulaw(i16::MIN), 0x00);
    assert_eq!(linear_to_alaw(i16::MAX), 0xAA);
    assert_eq!(linear_to_alaw(i16::MIN), 0x2A);
}

#[test]
fn every_code_decodes_and_encodes_back_to_itself() {
    for law in [Law::Ulaw, Law::Alaw] {
        for c in 0..=255u8 {
            let back = law.encode_one(law.decode_one(c));
            // μ-law has two codes for zero (0x7F, 0xFF): both decode to 0, 0 encodes to 0xFF
            if law == Law::Ulaw && c == 0x7F {
                assert_eq!(back, 0xFF);
            } else {
                assert_eq!(back, c, "{law:?} {c:#04x}");
            }
        }
    }
}

#[test]
fn decoding_is_monotonic_and_symmetric() {
    for law in [Law::Ulaw, Law::Alaw] {
        let mut v: Vec<i16> = (0..=255u8).map(|c| law.decode_one(c)).collect();
        v.sort_unstable();
        v.dedup();
        assert!(v.len() >= 255, "{law:?}");
        for c in 0..=255u8 {
            assert_eq!(
                law.decode_one(c),
                -law.decode_one(c ^ 0x80),
                "{law:?} {c:#04x}"
            );
        }
    }
}

#[test]
fn payload_types() {
    assert_eq!(Law::from_payload_type(0), Some(Law::Ulaw));
    assert_eq!(Law::from_payload_type(8), Some(Law::Alaw));
    assert_eq!(Law::from_payload_type(18), None);
    assert_eq!(Law::Alaw.payload_type(), 8);
    assert_eq!(Law::Ulaw.encoding_name(), "PCMU");
    let mut out = vec![];
    Law::Alaw.decode(&[Law::Alaw.silence()], &mut out);
    assert!(out[0].abs() <= 8);
}
