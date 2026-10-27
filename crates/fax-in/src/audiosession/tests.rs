use super::*;

#[test]
fn rtp_packets_advance_by_one_and_160() {
    let mut tx = RtpTx::new(Law::Alaw);
    let a = RtpPacket::decode(&tx.packet(&[0i16; BLOCK])).unwrap();
    let b = RtpPacket::decode(&tx.packet(&[0i16; BLOCK])).unwrap();
    assert_eq!(a.payload_type, 8);
    assert_eq!(b.sequence, a.sequence.wrapping_add(1));
    assert_eq!(b.timestamp, a.timestamp.wrapping_add(160));
    assert_eq!(a.ssrc, b.ssrc);
    assert_eq!(a.payload, vec![Law::Alaw.silence(); BLOCK]);
}
