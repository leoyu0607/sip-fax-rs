//! N03 (M5a): our CED (`fax_modem::tone::ToneGenerator::ced`) is detected by SpanDSP's
//! `modem_connect_tones_rx`, the independent counterpart; and the calling tone our detector
//! hears (N01) is heard by SpanDSP's detector too.
//!
//! SpanDSP's detector (ITU-T V.25 answer tone, T.30 CED: 2100 Hz) is the receiver a real fax
//! machine's modem would use; our generator is only right if that receiver accepts it.

use fax_modem::tone::{CngDetector, ToneGenerator};
use spandsp_sys as sys;

struct Rx {
    ptr: *mut sys::modem_connect_tones_rx_state_t,
}

impl Rx {
    fn new(tone: i32) -> Self {
        // SAFETY: NULL makes SpanDSP allocate the state; no callback.
        let ptr = unsafe {
            sys::modem_connect_tones_rx_init(std::ptr::null_mut(), tone, None, std::ptr::null_mut())
        };
        assert!(!ptr.is_null());
        Self { ptr }
    }

    /// Feeds 20 ms blocks; returns the sample count at the end of the first block after which
    /// a tone was reported (and what it was), if any, and whether it was still reported at the end.
    fn run(&mut self, samples: &[i16]) -> (Option<(usize, i32)>, i32) {
        let mut first = None;
        let mut fed = 0;
        for block in samples.chunks(160) {
            // SAFETY: `block` is valid for `block.len()` samples.
            unsafe { sys::modem_connect_tones_rx(self.ptr, block.as_ptr(), block.len() as i32) };
            fed += block.len();
            // SAFETY: valid state.
            let t = unsafe { sys::modem_connect_tones_rx_get(self.ptr) };
            if t != 0 && first.is_none() {
                first = Some((fed, t));
            }
        }
        // SAFETY: valid state.
        (first, unsafe { sys::modem_connect_tones_rx_get(self.ptr) })
    }
}

impl Drop for Rx {
    fn drop(&mut self) {
        // SAFETY: allocated by the init call.
        unsafe { sys::modem_connect_tones_rx_free(self.ptr) };
    }
}

fn ced(level: f32, lead_ms: usize, tail_ms: usize) -> Vec<i16> {
    let mut v = vec![0i16; lead_ms * 8];
    let mut g = ToneGenerator::ced(level);
    let mut buf = vec![0i16; 30_000];
    let n = g.fill(&mut buf);
    v.extend_from_slice(&buf[..n]);
    v.extend(vec![0i16; tail_ms * 8]);
    v
}

/// xorshift64* and Box-Muller
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn uniform(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
}

fn with_noise(v: &[i16], snr_db: f64, rng: &mut Rng) -> Vec<i16> {
    let sig = f64::from(fax_modem::level::block_mean_square(v));
    // noise relative to the power of the tone part only (the tone is most of the signal)
    let sd = (sig / 10f64.powf(snr_db / 10.0)).sqrt();
    v.iter()
        .map(|&s| {
            (f64::from(s) + sd * rng.gauss())
                .round()
                .clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

#[test]
fn n03_spandsp_detects_our_ced_at_every_level() {
    for level in [-10.0f32, -15.0, -20.0, -25.0, -30.0] {
        let sig = ced(level, 500, 500);
        let (first, _) = Rx::new(sys::MODEM_CONNECT_TONES_ANS).run(&sig);
        let (at, kind) = first.unwrap_or_else(|| panic!("{level} dBm0: SpanDSP heard no CED"));
        assert_eq!(kind, sys::MODEM_CONNECT_TONES_ANS, "{level}");
        // heard while the tone is on (it starts at 500 ms and lasts 3 s), not before it starts
        assert!(
            at > 500 * 8 && at < (500 + 3000) * 8 + 160,
            "{level}: heard at sample {at}"
        );
    }
}

#[test]
fn n03_spandsp_detects_our_ced_with_noise_and_in_three_seeds() {
    for seed in [0xCED0_0001u64, 0xCED0_0002, 0xCED0_0003] {
        let mut rng = Rng(seed ^ 0x9E37_79B9_7F4A_7C15);
        for (level, snr) in [(-10.0f32, 30.0), (-20.0, 25.0), (-30.0, 20.0)] {
            let sig = with_noise(&ced(level, 700, 600), snr, &mut rng);
            let (first, _) = Rx::new(sys::MODEM_CONNECT_TONES_ANS).run(&sig);
            assert!(
                first.is_some(),
                "seed {seed:#x} {level} dBm0 SNR {snr}: SpanDSP heard no CED"
            );
        }
    }
}

#[test]
fn n03_the_same_receiver_also_takes_ced_given_as_ced_or_preamble() {
    // the receiver mode a T.30 answering side would use while it waits for either CED or V.21
    let sig = ced(-15.0, 300, 300);
    let (first, _) = Rx::new(sys::MODEM_CONNECT_TONES_FAX_CED_OR_PREAMBLE).run(&sig);
    assert!(first.is_some());
}

#[test]
fn n03_spandsp_does_not_take_other_tones_for_ced_and_ours_is_not_a_cng() {
    // negative controls: the detector is not simply always on
    let silence = vec![0i16; 8000 * 4];
    assert_eq!(Rx::new(sys::MODEM_CONNECT_TONES_ANS).run(&silence).0, None);
    // 1100 Hz (CNG) is not CED
    let mut g = ToneGenerator::new(1100.0, -15.0, 3000, 75);
    let mut cng = vec![0i16; 30_000];
    let n = g.fill(&mut cng);
    assert_eq!(Rx::new(sys::MODEM_CONNECT_TONES_ANS).run(&cng[..n]).0, None);
    // 1650 Hz (V.21 mark) is not CED
    let mut g = ToneGenerator::new(1650.0, -15.0, 3000, 75);
    let mut v21 = vec![0i16; 30_000];
    let n = g.fill(&mut v21);
    assert_eq!(Rx::new(sys::MODEM_CONNECT_TONES_ANS).run(&v21[..n]).0, None);
    // our CED is not a CNG for SpanDSP either
    assert_eq!(
        Rx::new(sys::MODEM_CONNECT_TONES_FAX_CNG)
            .run(&ced(-15.0, 300, 300))
            .0,
        None
    );
    // ... nor for our own detector
    assert!(!CngDetector::new().feed(&ced(-15.0, 300, 300)));
}

#[test]
fn n03_our_cng_detector_and_spandsp_agree_on_a_cng_train() {
    // N01's independent counterpart: the same 1100 Hz 0.5 s / 3 s train through both detectors
    for level in [-10.0f32, -20.0, -30.0] {
        let mut train = vec![0i16; 4000];
        for _ in 0..4 {
            let mut g = ToneGenerator::new(1100.0, level, 500, 3000);
            let mut buf = vec![0i16; 28_000];
            let n = g.fill(&mut buf);
            train.extend_from_slice(&buf[..n]);
        }
        let mut ours = CngDetector::new();
        let mut our_hits = 0;
        for c in train.chunks(160) {
            if ours.feed(c) {
                our_hits += 1;
            }
        }
        assert_eq!(our_hits, 4, "{level}: ours");
        let (first, _) = Rx::new(sys::MODEM_CONNECT_TONES_FAX_CNG).run(&train);
        let (at, _) = first.unwrap_or_else(|| panic!("{level}: SpanDSP heard no CNG"));
        // SpanDSP hears the first burst, too (within the first burst and its pause)
        assert!(at <= 4000 + 28_000, "{level}: heard at sample {at}");
    }
}
