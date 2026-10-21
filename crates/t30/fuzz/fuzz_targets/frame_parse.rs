#![no_main]
//! Arbitrary octets as an HDLC frame body, a DIS and a DCS information field, an identification.
//! Nothing may panic; a parsed DCS must describe a consistent page geometry.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(f) = t30::Frame::parse(data) {
        let _ = (f.fcf(), f.x_bit(), f.final_frame);
        let _ = t30::Dcs::parse(f.fif);
        let _ = t30::Dis::parse(f.fif);
        let _ = t30::decode_ident(f.fif);
    }
    // every slice is also tried as a bare information field
    let dis = t30::Dis::parse(data);
    let _ = (dis.v17(), dis.v29(), dis.v27ter(), dis.v33());
    if let Ok(d) = t30::Dcs::parse(data) {
        assert!(d.bit_rate >= 2400 && d.bit_rate <= 14400);
        assert!(d.width.pixels() >= 1728 && d.x_dpi() >= 200 && d.y_dpi() >= 98);
        let _ = d.unsupported();
    }
    // CTC carries two octets of DCS bits (the rate); any slice must be total
    if let Ok((_, rate)) = t30::parse_rate(data) {
        assert!((2400..=14400).contains(&rate));
    }
    // ECM: FCD frame number and PPS counters are LSB first
    if let Some((&n, _)) = data.split_first() {
        assert_eq!(t30::lsb_first(t30::lsb_first(n)), n);
    }
    let id = t30::decode_ident(data);
    assert!(id.len() <= 20);
    for n in [0usize, 1, 8, 9, 24, 25, 1000] {
        let _ = t30::fif_bit(data, n);
    }
});
