#![no_main]
//! Feeds arbitrary bytes, as a bit stream (each byte least significant bit first), to the HDLC
//! receiver (`fax_modem::hdlc::HdlcRx`). It must not panic; frames come out at most `MAX_FRAME`
//! octets long; a frame it calls good re-encodes (`encode_burst`) and decodes to the same frame.

use fax_modem::hdlc::{HdlcEvent, HdlcRx, MAX_FRAME, encode_burst};
use libfuzzer_sys::fuzz_target;

fn decode(bits: impl Iterator<Item = bool>) -> Vec<HdlcEvent> {
    let mut rx = HdlcRx::new();
    bits.filter_map(|b| rx.bit(b)).collect()
}

fuzz_target!(|data: &[u8]| {
    let bits = data
        .iter()
        .flat_map(|&b| (0..8).map(move |i| b >> i & 1 != 0));
    let mut frames = Vec::new();
    for ev in decode(bits) {
        if let HdlcEvent::Frame { data, fcs_ok } = ev {
            assert!(data.len() <= MAX_FRAME);
            if fcs_ok {
                assert!(!data.is_empty());
                frames.push(data);
            }
        }
    }
    // Round trip: the good frames, sent again, come back as good frames.
    if !frames.is_empty() {
        let again: Vec<Vec<u8>> = decode(encode_burst(&frames, 4, 2).into_iter())
            .into_iter()
            .filter_map(|e| match e {
                HdlcEvent::Frame { data, fcs_ok: true } => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(again, frames, "re-encoded frames differ");
    }
});
