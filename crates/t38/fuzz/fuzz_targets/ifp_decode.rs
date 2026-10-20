#![no_main]
//! Feeds arbitrary bytes to the IFP decoder under every T.38 version. Whatever decodes must
//! re-encode and decode to the same value (the encoding of a decoded packet is canonical).

use libfuzzer_sys::fuzz_target;
use t38::{Ifp, T38Version};

fuzz_target!(|data: &[u8]| {
    for v in [T38Version::V0, T38Version::V1, T38Version::V2, T38Version::V3] {
        let _ = Ifp::decode(data, v);
        let _ = Ifp::decode_padded(data, v);
        if let Ok((pkt, used)) = Ifp::decode_prefix(data, v) {
            assert!(used <= data.len());
            let enc = pkt.encode(v).expect("a decoded packet must be encodable");
            let (again, used2) = Ifp::decode_prefix(&enc, v).expect("re-encoded packet decodes");
            assert_eq!(again, pkt, "decode(encode(x)) != x");
            assert_eq!(used2, enc.len(), "re-encoding leaves no trailing octets");
        }
    }
});
