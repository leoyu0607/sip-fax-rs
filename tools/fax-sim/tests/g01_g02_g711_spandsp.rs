//! G01 / G02 (M5a): `sip_core::rtp::g711` against SpanDSP's G.711 (`g711_encode` / `g711_decode`,
//! which run SpanDSP's own `alaw_to_linear`, `ulaw_to_linear`, `linear_to_alaw`, `linear_to_ulaw`).
//!
//! * G01: all 256 codes of both laws decode to the same 16-bit value.
//! * G02: all 65536 16-bit values encode to the same code; and what both decode again is equal.

use sip_core::rtp::Law;
use spandsp_sys as sys;

struct Spandsp {
    ptr: *mut sys::g711_state_t,
}

impl Spandsp {
    fn new(law: Law) -> Self {
        let mode = match law {
            Law::Alaw => sys::G711_ALAW,
            Law::Ulaw => sys::G711_ULAW,
        };
        // SAFETY: NULL makes SpanDSP allocate the state.
        let ptr = unsafe { sys::g711_init(std::ptr::null_mut(), mode) };
        assert!(!ptr.is_null(), "g711_init returned NULL");
        Self { ptr }
    }

    fn decode(&mut self, codes: &[u8]) -> Vec<i16> {
        let mut out = vec![0i16; codes.len()];
        // SAFETY: both buffers have `codes.len()` elements.
        let n = unsafe {
            sys::g711_decode(
                self.ptr,
                out.as_mut_ptr(),
                codes.as_ptr(),
                codes.len() as i32,
            )
        };
        assert_eq!(n as usize, codes.len());
        out
    }

    fn encode(&mut self, samples: &[i16]) -> Vec<u8> {
        let mut out = vec![0u8; samples.len()];
        // SAFETY: both buffers have `samples.len()` elements.
        let n = unsafe {
            sys::g711_encode(
                self.ptr,
                out.as_mut_ptr(),
                samples.as_ptr(),
                samples.len() as i32,
            )
        };
        assert_eq!(n as usize, samples.len());
        out
    }
}

impl Drop for Spandsp {
    fn drop(&mut self) {
        // SAFETY: allocated by g711_init.
        unsafe { sys::g711_free(self.ptr) };
    }
}

fn all_codes() -> Vec<u8> {
    (0..=255).collect()
}

fn all_samples() -> Vec<i16> {
    (i16::MIN..=i16::MAX).collect()
}

#[test]
fn g01_decoding_all_256_codes_equals_spandsp_alaw() {
    let codes = all_codes();
    let theirs = Spandsp::new(Law::Alaw).decode(&codes);
    for c in 0..=255u8 {
        assert_eq!(
            Law::Alaw.decode_one(c),
            theirs[c as usize],
            "A-law code {c:#04x}"
        );
    }
    // the bulk decoder too
    let mut ours = Vec::new();
    Law::Alaw.decode(&codes, &mut ours);
    assert_eq!(ours, theirs);
}

#[test]
fn g01_decoding_all_256_codes_equals_spandsp_ulaw() {
    let codes = all_codes();
    let theirs = Spandsp::new(Law::Ulaw).decode(&codes);
    for c in 0..=255u8 {
        assert_eq!(
            Law::Ulaw.decode_one(c),
            theirs[c as usize],
            "mu-law code {c:#04x}"
        );
    }
    let mut ours = Vec::new();
    Law::Ulaw.decode(&codes, &mut ours);
    assert_eq!(ours, theirs);
}

#[test]
fn g02_encoding_all_65536_values_equals_spandsp_alaw() {
    let samples = all_samples();
    let theirs = Spandsp::new(Law::Alaw).encode(&samples);
    let mut ours = Vec::new();
    Law::Alaw.encode(&samples, &mut ours);
    let diffs: Vec<_> = samples
        .iter()
        .zip(ours.iter().zip(&theirs))
        .filter(|(_, (a, b))| a != b)
        .map(|(s, (a, b))| (*s, *a, *b))
        .collect();
    assert!(
        diffs.is_empty(),
        "{} A-law encodings differ, first: {:?}",
        diffs.len(),
        &diffs[..diffs.len().min(10)]
    );
}

#[test]
fn g02_encoding_all_65536_values_equals_spandsp_ulaw() {
    let samples = all_samples();
    let theirs = Spandsp::new(Law::Ulaw).encode(&samples);
    let mut ours = Vec::new();
    Law::Ulaw.encode(&samples, &mut ours);
    let diffs: Vec<_> = samples
        .iter()
        .zip(ours.iter().zip(&theirs))
        .filter(|(_, (a, b))| a != b)
        .map(|(s, (a, b))| (*s, *a, *b))
        .collect();
    assert!(
        diffs.is_empty(),
        "{} mu-law encodings differ, first: {:?}",
        diffs.len(),
        &diffs[..diffs.len().min(10)]
    );
}

#[test]
fn g02_the_two_implementations_agree_through_a_full_round_trip() {
    // our encode -> their decode and their encode -> our decode give the same samples, and a
    // second round trip changes nothing (idempotent) in either implementation
    let samples = all_samples();
    for law in [Law::Alaw, Law::Ulaw] {
        let mut sp = Spandsp::new(law);
        let mut ours_codes = Vec::new();
        law.encode(&samples, &mut ours_codes);
        let their_pcm = sp.decode(&ours_codes);
        let their_codes = sp.encode(&samples);
        let mut our_pcm = Vec::new();
        law.decode(&their_codes, &mut our_pcm);
        assert_eq!(their_pcm, our_pcm, "{law:?}");
        // second pass
        let re_encoded = sp.encode(&their_pcm);
        let again_theirs = sp.decode(&re_encoded);
        assert_eq!(again_theirs, their_pcm, "{law:?}: SpanDSP idempotent");
        let mut again = Vec::new();
        law.decode(
            &{
                let mut c = Vec::new();
                law.encode(&our_pcm, &mut c);
                c
            },
            &mut again,
        );
        assert_eq!(again, our_pcm, "{law:?}: ours idempotent");
    }
}

#[test]
fn g01_spandsp_state_is_released_and_reusable() {
    // the FFI wrapper itself: two instances of each law side by side, in either order
    let mut a = Spandsp::new(Law::Alaw);
    let mut u = Spandsp::new(Law::Ulaw);
    assert_eq!(a.decode(&[0xD5]), vec![8]);
    assert_eq!(u.decode(&[0xFF]), vec![0]);
    assert_eq!(a.encode(&[0]), vec![0xD5]);
    assert_eq!(u.encode(&[0]), vec![0xFF]);
}
