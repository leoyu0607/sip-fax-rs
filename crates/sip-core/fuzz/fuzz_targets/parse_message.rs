#![no_main]
//! Feeds arbitrary bytes to the SIP message parser (both framings) and the stream framer.
//! Anything that parses must serialise and parse again.

use libfuzzer_sys::fuzz_target;
use sip_core::message::{Framing, parse_datagram, parse_with_framing};
use sip_core::transport::framing::take_frame;

fuzz_target!(|data: &[u8]| {
    for framing in [Framing::Stream, Framing::Datagram] {
        if let Ok((msg, consumed)) = parse_with_framing(data, framing) {
            assert!(consumed <= data.len());
            assert!(parse_datagram(&msg.to_bytes()).is_ok(), "re-parse of serialised message failed");
        }
    }
    let mut buf = data.to_vec();
    let _ = take_frame(&mut buf);
});
