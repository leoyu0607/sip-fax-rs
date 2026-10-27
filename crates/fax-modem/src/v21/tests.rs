use super::*;
use crate::hdlc::{HdlcEvent, HdlcRx, encode_burst};

fn frames_through(samples: &[i16]) -> Vec<HdlcEvent> {
    let mut rx = V21Rx::new();
    let mut h = HdlcRx::new();
    rx.feed(samples)
        .into_iter()
        .filter_map(|b| h.bit(b))
        .collect()
}

#[test]
fn the_bit_clock_is_exact() {
    let mut tx = V21Tx::new(-12.0);
    // 300 bits are exactly one second
    assert_eq!(tx.modulate(&[true; 300]).len(), 8000);
    assert_eq!(tx.modulate(&[false; 3]).len(), 80);
}

#[test]
fn frames_go_through_the_modem() {
    let frames = vec![
        vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8, 0x80],
        vec![0xFF, 0x13, 0xFB, 0x01, 0x02],
    ];
    let mut tx = V21Tx::new(-12.0);
    let mut s = vec![0i16; 400];
    s.extend(tx.modulate(&encode_burst(&frames, 38, 2)));
    s.extend(vec![0i16; 400]);
    let ev = frames_through(&s);
    let got: Vec<_> = ev
        .into_iter()
        .filter(|e| matches!(e, HdlcEvent::Frame { .. }))
        .collect();
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
fn quiet_and_noisy_signals_still_decode() {
    let frames = vec![vec![0xFF, 0xC8, 0x21, 0x5A, 0xA5, 0x00, 0xFF]];
    for level in [-10.0f32, -43.0] {
        let mut tx = V21Tx::new(level);
        let mut s = tx.modulate(&encode_burst(&frames, 38, 2));
        // white noise 20 dB below the signal
        let noise =
            crate::level::sine_amplitude(level - 20.0) / std::f32::consts::SQRT_2 * 3f32.sqrt();
        let mut x = 0x9E37_79B9u32;
        for v in s.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let u = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
            *v = (*v as f32 + noise * u) as i16;
        }
        let ev = frames_through(&s);
        assert!(
            ev.contains(&HdlcEvent::Frame {
                data: frames[0].clone(),
                fcs_ok: true
            }),
            "{level} dBm0: {ev:?}"
        );
    }
}
