use super::*;

fn receive(bits: &[bool]) -> Vec<HdlcEvent> {
    let mut rx = HdlcRx::new();
    bits.iter().filter_map(|&b| rx.bit(b)).collect()
}

#[test]
fn fcs_of_the_check_string() {
    // CRC-16/X-25 of "123456789"
    assert_eq!(fcs16(b"123456789"), 0x906E);
    let mut f = b"123456789".to_vec();
    let c = fcs16(&f);
    f.extend([c as u8, (c >> 8) as u8]);
    assert_eq!(crc_update(0xFFFF, &f), GOOD_RESIDUE);
}

#[test]
fn frames_come_back_with_stuffing_undone() {
    // DIS-like frames, one full of ones (forces stuffing everywhere)
    let frames = vec![
        vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8],
        vec![0xFF; 20],
        vec![0xFF, 0xC8, 0x5F],
    ];
    let bits = encode_burst(&frames, 10, 2);
    // no six ones in a row anywhere but in flags: count ones runs
    let ev = receive(&bits);
    assert_eq!(ev[0], HdlcEvent::Preamble);
    let got: Vec<_> = ev[1..].to_vec();
    assert_eq!(
        got,
        frames
            .iter()
            .map(|f| HdlcEvent::Frame {
                data: f.clone(),
                fcs_ok: true
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_flipped_bit_fails_the_fcs_and_seven_ones_abort() {
    let frames = vec![vec![0xFF, 0x13, 0x80]];
    let mut bits = encode_burst(&frames, 4, 1);
    // flip a bit inside the frame (after the 4 preamble flags)
    bits[32 + 9] = !bits[32 + 9];
    let ev = receive(&bits);
    assert!(
        matches!(ev.last(), Some(HdlcEvent::Frame { fcs_ok: false, .. })),
        "{ev:?}"
    );

    let mut bits = encode_burst(&[], 4, 0);
    bits.extend([true, false, true, true, false, false, true, false]); // some data bits
    bits.extend([true; 7]);
    let ev = receive(&bits);
    assert_eq!(ev.last(), Some(&HdlcEvent::Abort));
}

#[test]
fn short_and_odd_frames_are_bad_not_panics() {
    // flag, 2 data octets (shorter than an FCS + 1), flag
    let mut bits = encode_burst(&[], 3, 0);
    for b in [0x12u8, 0x34] {
        (0..8).for_each(|i| bits.push(b >> i & 1 != 0));
    }
    (0..8).for_each(|i| bits.push(FLAG >> i & 1 != 0));
    let ev = receive(&bits);
    assert!(
        matches!(ev.last(), Some(HdlcEvent::Frame { fcs_ok: false, .. })),
        "{ev:?}"
    );
    // garbage never panics
    let mut x = 0x1234_5678u32;
    let noise: Vec<bool> = (0..100_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x & 1 != 0
        })
        .collect();
    let _ = receive(&noise);
}
