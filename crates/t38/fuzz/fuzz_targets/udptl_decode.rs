#![no_main]
//! Feeds arbitrary datagrams to the UDPTL packet decoder and the stateful receiver.

use libfuzzer_sys::fuzz_target;
use t38::{UdptlPacket, UdptlReceiver};

fuzz_target!(|data: &[u8]| {
    if let Ok(pkt) = UdptlPacket::decode(data) {
        if !pkt.primary.is_empty() {
            let enc = pkt.encode().expect("non-empty primary encodes");
            assert_eq!(UdptlPacket::decode(&enc).expect("re-decodes"), pkt);
        }
    }
    // Cut the input into a stream of datagrams for the stateful receiver.
    let mut rx = UdptlReceiver::default();
    for chunk in data.chunks(23) {
        let _ = rx.on_datagram(chunk);
    }
    let _ = rx.on_datagram(data);
    let _ = rx.flush();
});
