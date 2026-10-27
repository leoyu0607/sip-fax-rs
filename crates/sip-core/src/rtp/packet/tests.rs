use super::*;

#[test]
fn an_extension_longer_than_its_length_field_is_cut_not_corrupted() {
    let mut pkt = RtpPacket::new(0, 1, 2, 3, vec![0x55; 4]);
    pkt.extension = Some(Extension {
        profile: 1,
        data: vec![7; 4 * 65_536 + 3],
    });
    let p = RtpPacket::decode(&pkt.encode()).unwrap();
    assert_eq!(p.extension.unwrap().data, vec![7; 4 * 65_535]);
    assert_eq!(p.payload, vec![0x55; 4]);
}

// Sipress' original test, kept (with the new constructor).
#[test]
fn test_encode_decode_roundtrip() {
    let pkt = RtpPacket::new(0, 42, 1000, 0xDEADBEEF, vec![0u8; 160]);
    let bytes = pkt.encode();
    assert_eq!(bytes.len(), RTP_HEADER_SIZE + 160);
    let decoded = RtpPacket::decode(&bytes).unwrap();
    assert_eq!(decoded.sequence, 42);
    assert_eq!(decoded.timestamp, 1000);
    assert_eq!(decoded.ssrc, 0xDEADBEEF);
    assert_eq!(decoded.payload.len(), 160);
}

#[test]
fn marker_csrc_extension_and_padding_are_read() {
    // V=2 P=1 X=1 CC=2, M=1 PT=8
    let mut d = vec![0xB2, 0x88, 0x00, 0x07, 0, 0, 0x01, 0x40, 1, 2, 3, 4];
    d.extend_from_slice(&[0, 0, 0, 0x11, 0, 0, 0, 0x22]); // CSRCs
    d.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01, 9, 9, 9, 9]); // extension, 1 word
    d.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // payload
    d.extend_from_slice(&[0, 0, 3]); // 3 bytes of padding
    let p = RtpPacket::decode(&d).unwrap();
    assert!(p.marker);
    assert_eq!(p.payload_type, 8);
    assert_eq!(p.sequence, 7);
    assert_eq!(p.timestamp, 320);
    assert_eq!(p.csrc, vec![0x11, 0x22]);
    assert_eq!(
        p.extension,
        Some(Extension {
            profile: 0xBEDE,
            data: vec![9; 4]
        })
    );
    assert_eq!(p.payload, vec![0xAA, 0xBB, 0xCC]);
    // written again without the padding, everything else equal
    assert_eq!(RtpPacket::decode(&p.encode()).unwrap(), p);
}

#[test]
fn bad_packets_are_refused() {
    assert_eq!(RtpPacket::decode(&[0x80; 11]), Err(RtpError::Truncated(11)));
    let mut v1 = RtpPacket::new(0, 1, 1, 1, vec![]).encode();
    v1[0] = 0x40;
    assert_eq!(RtpPacket::decode(&v1), Err(RtpError::Version(1)));
    // CC=3 without the CSRCs
    let mut cc = RtpPacket::new(0, 1, 1, 1, vec![1, 2]).encode();
    cc[0] |= 3;
    assert!(matches!(
        RtpPacket::decode(&cc),
        Err(RtpError::Truncated(_))
    ));
    // padding longer than the payload, and zero
    let mut pad = RtpPacket::new(0, 1, 1, 1, vec![1, 2]).encode();
    pad[0] |= 0x20;
    *pad.last_mut().unwrap() = 9;
    assert_eq!(RtpPacket::decode(&pad), Err(RtpError::Padding(9)));
    *pad.last_mut().unwrap() = 0;
    assert_eq!(RtpPacket::decode(&pad), Err(RtpError::Padding(0)));
}
