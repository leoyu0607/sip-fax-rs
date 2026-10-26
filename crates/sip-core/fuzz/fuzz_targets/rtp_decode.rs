#![no_main]
//! Feeds arbitrary bytes to the RTP decoder (docs/test-plans/M5.md section 1). Anything that
//! decodes must encode and decode to the same packet, and its payload must go through G.711 and a
//! jitter buffer.

use libfuzzer_sys::fuzz_target;
use sip_core::rtp::{JitterBuffer, JitterConfig, Law, RtpPacket};

fuzz_target!(|data: &[u8]| {
    let Ok(p) = RtpPacket::decode(data) else {
        return;
    };
    assert!(p.payload.len() <= data.len());
    assert_eq!(RtpPacket::decode(&p.encode()).as_ref(), Ok(&p));
    let law = if p.payload_type == 8 {
        Law::Alaw
    } else {
        Law::Ulaw
    };
    let mut samples = Vec::new();
    law.decode(&p.payload, &mut samples);
    assert_eq!(samples.len(), p.payload.len());
    let mut jb = JitterBuffer::new(JitterConfig::G711);
    let mut out = [0i16; 160];
    for k in 0..4u32 {
        let ts = p.timestamp.wrapping_add(k * samples.len() as u32);
        jb.push(p.ssrc, ts, samples.clone());
        let pulled = jb.pull(&mut out);
        assert!(pulled.gap as usize <= out.len());
    }
});
