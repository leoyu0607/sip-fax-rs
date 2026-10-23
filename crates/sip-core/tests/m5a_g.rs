//! M5a (test plan `docs/test-plans/M5.md` §2): G01–G04, G.711 and `RtpPacket`.
//!
//! The independent counterpart of G01/G02 (SpanDSP) lives in `tools/fax-sim/tests/g01_g02_g711_spandsp.rs`;
//! here the reference is the G.711 table itself, written out a second way (segment base and step
//! of Tables 1a/2a of the recommendation), not the shift-and-mask code of `rtp::g711`.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use sip_core::rtp::g711::{alaw_to_linear, linear_to_alaw, linear_to_ulaw, ulaw_to_linear};
use sip_core::rtp::{Extension, Law, RTP_HEADER_SIZE, RtpError, RtpPacket};

// ------------------------------------------------------------------------------------- G.711

/// μ-law decoder from the table form of G.711 Table 2a: after inverting all bits, a code is
/// `S SSS MMMM`; the 14-bit magnitude is `((2·M + 33) << SSS) − 33`; the 16-bit scale is 4×.
fn ref_ulaw_decode(code: u8) -> i16 {
    let c = !code;
    let neg = c & 0x80 != 0;
    let seg = u32::from((c >> 4) & 7);
    let m = i32::from(c & 15);
    let mag14 = ((2 * m + 33) << seg) - 33;
    let v = 4 * mag14;
    (if neg { -v } else { v }) as i16
}

/// A-law decoder from the table form of G.711 Table 1a: after XOR 0x55 a code is `S SSS MMMM`;
/// segment 0 has the 13-bit magnitude `2·M + 1`, segment k ≥ 1 has `(2·M + 33) << (k − 1)`;
/// the 16-bit scale is 8×. (Sign bit set = positive.)
fn ref_alaw_decode(code: u8) -> i16 {
    let c = code ^ 0x55;
    let pos = c & 0x80 != 0;
    let seg = u32::from((c >> 4) & 7);
    let m = i32::from(c & 15);
    let mag13 = if seg == 0 {
        2 * m + 1
    } else {
        (2 * m + 33) << (seg - 1)
    };
    let v = 8 * mag13;
    (if pos { v } else { -v }) as i16
}

#[test]
fn g01_ulaw_decoding_of_all_256_codes_equals_the_g711_table() {
    for c in 0..=255u8 {
        let want = ref_ulaw_decode(c);
        assert_eq!(ulaw_to_linear(c), want, "const fn, code {c:#04x}");
        assert_eq!(Law::Ulaw.decode_one(c), want, "table, code {c:#04x}");
    }
}

#[test]
fn g01_alaw_decoding_of_all_256_codes_equals_the_g711_table() {
    for c in 0..=255u8 {
        let want = ref_alaw_decode(c);
        assert_eq!(alaw_to_linear(c), want, "const fn, code {c:#04x}");
        assert_eq!(Law::Alaw.decode_one(c), want, "table, code {c:#04x}");
    }
}

#[test]
fn g01_literal_values_from_the_recommendation() {
    // μ-law (Table 2a, 16-bit scale): codes are transmitted inverted, 0xFF / 0x7F are the two zeros.
    let ulaw = [
        (0xFFu8, 0i16),
        (0x7F, 0),
        (0xFE, 8),
        (0xFD, 16),
        (0xF0, 120),
        (0xEF, 132),
        (0x80, 32124),
        (0x00, -32124),
        (0x7E, -8),
        (0x70, -120),
    ];
    for (code, lin) in ulaw {
        assert_eq!(ulaw_to_linear(code), lin, "PCMU {code:#04x}");
    }
    // A-law (Table 1a, 16-bit scale; even bits inverted): 0xD5 and 0x55 are +8 and -8.
    let alaw = [
        (0xD5u8, 8i16),
        (0x55, -8),
        (0xD4, 24),
        (0xD7, 40),
        (0xC5, 264),
        (0xC4, 280),
        (0xAB, 31232),
        (0xAA, 32256),
        (0x2A, -32256),
    ];
    for (code, lin) in alaw {
        assert_eq!(alaw_to_linear(code), lin, "PCMA {code:#04x}");
    }
}

#[test]
fn g01_decode_of_a_buffer_equals_decode_of_each_code() {
    let all: Vec<u8> = (0..=255).collect();
    for law in [Law::Ulaw, Law::Alaw] {
        let mut out = vec![7i16]; // decode appends
        law.decode(&all, &mut out);
        assert_eq!(out.len(), 257);
        assert_eq!(out[0], 7);
        for c in 0..=255usize {
            assert_eq!(out[c + 1], law.decode_one(c as u8), "{law:?} {c}");
        }
    }
}

/// Width of the quantization interval of a code on the 16-bit scale (G.711 Tables 1a / 2a):
/// mu-law 8 * 2^seg; A-law 16 for the two lowest segments, then doubling.
fn step(law: Law, code: u8) -> i32 {
    match law {
        Law::Ulaw => 8 << ((!code >> 4) & 7),
        Law::Alaw => {
            let seg = i32::from(((code ^ 0x55) >> 4) & 7);
            16 << (seg - 1).max(0)
        }
    }
}

#[test]
fn g02_every_16_bit_value_lands_in_the_interval_of_its_code() {
    for law in [Law::Ulaw, Law::Alaw] {
        let max = (0..=255u8)
            .map(|c| i32::from(law.decode_one(c)).abs())
            .max()
            .unwrap();
        for x in i16::MIN..=i16::MAX {
            let code = law.encode_one(x);
            let got = i32::from(law.decode_one(code));
            let err = (i32::from(x) - got).abs();
            // The decoder returns the midpoint of the interval, so the error is at most half the
            // step. (+1: the negative half of mu-law is shifted by one sample, the bias is applied
            // to -x-1.) Beyond the last level the value is clipped to it.
            let allowed = step(law, code) / 2 + 1;
            if i32::from(x).abs() <= max {
                assert!(
                    err <= allowed,
                    "{law:?} {x}: code {code:#04x} = {got}, error {err} > {allowed}"
                );
            } else {
                assert_eq!(got.abs(), max, "{law:?} {x} is clipped");
                assert_eq!(got.signum(), i32::from(x).signum());
            }
        }
    }
}

#[test]
fn g02_encoding_is_monotonic_in_the_input() {
    for law in [Law::Ulaw, Law::Alaw] {
        let mut prev = i32::MIN;
        for x in i16::MIN..=i16::MAX {
            let d = i32::from(law.decode_one(law.encode_one(x)));
            assert!(d >= prev, "{law:?} {x}: {d} < {prev}");
            prev = d;
        }
    }
}

#[test]
fn g02_decode_then_encode_returns_the_same_code() {
    for law in [Law::Ulaw, Law::Alaw] {
        for c in 0..=255u8 {
            let lin = law.decode_one(c);
            let back = law.encode_one(lin);
            // mu-law has two zeros (0x7F negative, 0xFF positive): both decode to 0, 0 encodes to 0xFF
            if law == Law::Ulaw && c == 0x7F {
                assert_eq!(back, 0xFF);
            } else {
                assert_eq!(back, c, "{law:?} {c:#04x} -> {lin}");
            }
            assert_eq!(law.decode_one(back), lin);
        }
    }
}

#[test]
fn g02_every_sample_is_idempotent_through_the_codec() {
    for law in [Law::Ulaw, Law::Alaw] {
        for x in i16::MIN..=i16::MAX {
            let d = law.decode_one(law.encode_one(x));
            // what the codec returns, the codec returns again (the code may differ only for the
            // two zeros of mu-law, which decode alike)
            assert_eq!(law.decode_one(law.encode_one(d)), d, "{law:?} {x}");
        }
    }
}

#[test]
fn g02_full_scale_and_silence() {
    assert_eq!(linear_to_ulaw(0), Law::Ulaw.silence());
    assert_eq!(linear_to_alaw(0), Law::Alaw.silence());
    assert_eq!(Law::Ulaw.decode_one(Law::Ulaw.silence()), 0);
    assert_eq!(Law::Alaw.decode_one(Law::Alaw.silence()), 8);
    // clipping, not wrapping
    assert_eq!(linear_to_ulaw(i16::MAX), 0x80);
    assert_eq!(linear_to_ulaw(i16::MIN), 0x00);
    assert_eq!(linear_to_alaw(i16::MAX), 0xAA);
    assert_eq!(linear_to_alaw(i16::MIN), 0x2A);
    // A sign flip of the input flips the sign bit of the code. mu-law is a sign-magnitude law:
    // exactly (-x and x give codes that differ only in the sign bit, G.711 Table 2a is symmetric);
    // A-law takes the one's complement of a negative value, the way its table is defined.
    for x in 1..=i16::MAX {
        assert_eq!(linear_to_ulaw(x) ^ 0x80, linear_to_ulaw(-x), "mu-law {x}");
        assert_eq!(
            linear_to_alaw(x) ^ 0x80,
            linear_to_alaw(-x - 1),
            "A-law {x}"
        );
    }
    assert_eq!(linear_to_ulaw(i16::MIN), linear_to_ulaw(i16::MAX) ^ 0x80);
}

#[test]
fn g02_buffer_encode_equals_encode_of_each_sample_and_appends() {
    let samples: Vec<i16> = (i16::MIN..=i16::MAX).step_by(7).collect();
    for law in [Law::Ulaw, Law::Alaw] {
        let mut out = vec![0xEEu8];
        law.encode(&samples, &mut out);
        assert_eq!(out.len(), samples.len() + 1);
        assert_eq!(out[0], 0xEE);
        for (i, &s) in samples.iter().enumerate() {
            assert_eq!(out[i + 1], law.encode_one(s));
        }
    }
}

#[test]
fn g02_quantization_error_stays_within_the_segment_step() {
    // within the range of the law the error is at most half a step of the segment:
    // mu-law 16 * 2^seg (seg 0..7) i.e. at most 512 at the top; A-law the same order
    for law in [Law::Ulaw, Law::Alaw] {
        for x in -32000i16..=32000 {
            let e = (i32::from(x) - i32::from(law.decode_one(law.encode_one(x)))).abs();
            let bound = 1 + i32::from(x).abs() / 16; // signal-to-noise of at least ~ 1/16 + 1
            assert!(e <= bound + 8, "{law:?} {x} error {e}");
        }
    }
}

// ----------------------------------------------------------------------------- RtpPacket (G03)

/// A valid datagram built by hand (not with `encode`): V=2, P, X, CC, M, PT, seq, ts, ssrc,
/// CSRCs, extension (profile, words), payload, padding of `pad` bytes (0 = no P bit).
#[allow(clippy::too_many_arguments)]
fn build(
    marker: bool,
    pt: u8,
    seq: u16,
    ts: u32,
    ssrc: u32,
    csrc: &[u32],
    ext: Option<(u16, &[u8])>,
    payload: &[u8],
    pad: usize,
) -> Vec<u8> {
    let mut d = Vec::new();
    let mut b0 = 0x80u8 | csrc.len() as u8;
    if ext.is_some() {
        b0 |= 0x10;
    }
    if pad > 0 {
        b0 |= 0x20;
    }
    d.push(b0);
    d.push((u8::from(marker) << 7) | pt);
    d.extend_from_slice(&seq.to_be_bytes());
    d.extend_from_slice(&ts.to_be_bytes());
    d.extend_from_slice(&ssrc.to_be_bytes());
    for c in csrc {
        d.extend_from_slice(&c.to_be_bytes());
    }
    if let Some((profile, data)) = ext {
        assert_eq!(data.len() % 4, 0);
        d.extend_from_slice(&profile.to_be_bytes());
        d.extend_from_slice(&((data.len() / 4) as u16).to_be_bytes());
        d.extend_from_slice(data);
    }
    d.extend_from_slice(payload);
    if pad > 0 {
        d.extend(std::iter::repeat_n(0u8, pad - 1));
        d.push(pad as u8);
    }
    d
}

#[test]
fn g03_every_csrc_count_from_0_to_15_is_read_and_stays_out_of_the_payload() {
    for cc in 0..=15usize {
        let csrc: Vec<u32> = (0..cc as u32).map(|i| 0x1000_0000 + i * 0x0101).collect();
        let payload: Vec<u8> = (0..160).map(|i| (i * 3) as u8).collect();
        let d = build(
            false,
            8,
            65535,
            0xFFFF_FFF0,
            0xCAFE_F00D,
            &csrc,
            None,
            &payload,
            0,
        );
        assert_eq!(d.len(), 12 + 4 * cc + 160);
        let p = RtpPacket::decode(&d).unwrap();
        assert_eq!(p.csrc, csrc, "cc={cc}");
        assert_eq!(p.payload, payload, "cc={cc}: the CSRC list is not payload");
        assert_eq!(p.payload_type, 8);
        assert_eq!(p.sequence, 65535);
        assert_eq!(p.timestamp, 0xFFFF_FFF0);
        assert_eq!(p.ssrc, 0xCAFE_F00D);
        assert!(!p.marker);
        assert_eq!(p.extension, None);
    }
}

#[test]
fn g03_marker_and_payload_type_bits() {
    for pt in [0u8, 8, 18, 96, 101, 127] {
        for marker in [false, true] {
            let d = build(marker, pt, 1, 2, 3, &[], None, &[0xAB], 0);
            let p = RtpPacket::decode(&d).unwrap();
            assert_eq!(p.marker, marker);
            assert_eq!(p.payload_type, pt);
            assert_eq!(p.payload, vec![0xAB]);
        }
    }
}

#[test]
fn g03_extension_of_0_1_and_many_words() {
    for words in [0usize, 1, 2, 17, 255] {
        let data: Vec<u8> = (0..words * 4).map(|i| (i % 251) as u8).collect();
        let d = build(true, 0, 9, 9, 9, &[5], Some((0xBEDE, &data)), &[1, 2, 3], 0);
        let p = RtpPacket::decode(&d).unwrap();
        assert_eq!(
            p.extension,
            Some(Extension {
                profile: 0xBEDE,
                data: data.clone()
            }),
            "words={words}"
        );
        assert_eq!(p.payload, vec![1, 2, 3]);
        assert_eq!(p.csrc, vec![5]);
    }
}

#[test]
fn g03_padding_is_cut_from_the_payload() {
    for pad in [1usize, 2, 4, 255] {
        let payload = vec![0x11u8; 300];
        let d = build(false, 0, 1, 1, 1, &[], None, &payload, pad);
        let p = RtpPacket::decode(&d).unwrap();
        assert_eq!(p.payload, payload, "pad={pad}");
    }
    // the padding may be the whole rest of the packet (empty payload)
    let d = build(false, 0, 1, 1, 1, &[], None, &[], 4);
    assert_eq!(RtpPacket::decode(&d).unwrap().payload, Vec::<u8>::new());
    // all together: CSRC + extension + padding
    let d = build(
        true,
        8,
        7,
        7,
        7,
        &[1, 2, 3],
        Some((1, &[9; 8])),
        &[0xAA; 160],
        3,
    );
    let p = RtpPacket::decode(&d).unwrap();
    assert_eq!(p.payload, vec![0xAA; 160]);
    assert_eq!(p.csrc.len(), 3);
    assert_eq!(p.extension.as_ref().unwrap().data, vec![9; 8]);
}

#[test]
fn g03_an_empty_payload_is_a_valid_packet() {
    let d = build(false, 0, 1, 1, 1, &[], None, &[], 0);
    assert_eq!(d.len(), RTP_HEADER_SIZE);
    assert!(RtpPacket::decode(&d).unwrap().payload.is_empty());
}

#[test]
fn g03_version_other_than_2_is_refused() {
    for v in [0u8, 1, 3] {
        let mut d = build(false, 0, 1, 1, 1, &[], None, &[1; 160], 0);
        d[0] = (d[0] & 0x3F) | (v << 6);
        assert_eq!(RtpPacket::decode(&d), Err(RtpError::Version(v)), "v={v}");
    }
}

#[test]
fn g03_truncated_header_csrc_list_and_extension_are_refused_at_every_length() {
    let d = build(
        false,
        0,
        1,
        1,
        1,
        &[1, 2],
        Some((0x1234, &[1, 2, 3, 4, 5, 6, 7, 8])),
        &[],
        0,
    );
    // every prefix shorter than header + CSRCs + extension is not a packet
    for n in 0..d.len() {
        let r = RtpPacket::decode(&d[..n]);
        assert!(
            matches!(r, Err(RtpError::Truncated(len)) if len == n),
            "prefix {n} of {}: {r:?}",
            d.len()
        );
    }
    assert!(RtpPacket::decode(&d).is_ok());
    // an extension that announces more words than there are
    let mut e = build(false, 0, 1, 1, 1, &[], Some((1, &[0; 4])), &[], 0);
    e[14] = 0;
    e[15] = 200; // 200 words
    assert!(matches!(RtpPacket::decode(&e), Err(RtpError::Truncated(_))));
    // the extension flag with nothing after the header
    let mut x = build(false, 0, 1, 1, 1, &[], None, &[], 0);
    x[0] |= 0x10;
    assert!(matches!(
        RtpPacket::decode(&x),
        Err(RtpError::Truncated(12))
    ));
}

#[test]
fn g03_bad_padding_is_refused() {
    // P bit with nothing after the header
    let mut d = build(false, 0, 1, 1, 1, &[], None, &[], 0);
    d[0] |= 0x20;
    assert!(matches!(
        RtpPacket::decode(&d),
        Err(RtpError::Truncated(_) | RtpError::Padding(_))
    ));
    // zero, longer than the rest, longer than the rest by one
    for (payload, pad_byte, want) in [
        (vec![1u8, 2, 3], 0u8, RtpError::Padding(0)),
        (vec![1, 2, 3], 4, RtpError::Padding(4)),
        (vec![1, 2, 3], 255, RtpError::Padding(255)),
    ] {
        let mut d = build(false, 0, 1, 1, 1, &[], None, &payload, 0);
        d[0] |= 0x20;
        *d.last_mut().unwrap() = pad_byte;
        assert_eq!(RtpPacket::decode(&d), Err(want), "pad byte {pad_byte}");
    }
    // exactly as long as the rest is fine
    let mut d = build(false, 0, 1, 1, 1, &[], None, &[1, 2, 3], 0);
    d[0] |= 0x20;
    *d.last_mut().unwrap() = 3;
    assert!(RtpPacket::decode(&d).unwrap().payload.is_empty());
    // padding must not eat into the extension: with nothing after the extension any pad count is
    // too long, even 1 (the last byte is the extension's)
    let mut d = build(false, 0, 1, 1, 1, &[], Some((1, &[0; 4])), &[], 0);
    d[0] |= 0x20;
    *d.last_mut().unwrap() = 1;
    assert_eq!(RtpPacket::decode(&d), Err(RtpError::Padding(1)));
    // ... and with one payload byte after it, a pad count of 1 is that byte
    let mut d = build(false, 0, 1, 1, 1, &[], Some((1, &[0; 4])), &[1], 0);
    d[0] |= 0x20;
    *d.last_mut().unwrap() = 1;
    assert!(RtpPacket::decode(&d).unwrap().payload.is_empty());
    *d.last_mut().unwrap() = 2;
    assert_eq!(RtpPacket::decode(&d), Err(RtpError::Padding(2)));
}

#[test]
fn g03_the_sipress_decoder_took_everything_after_byte_12_as_payload() {
    // the original decoder (docs/sipress-extraction.md): payload = data[12..]. With a CSRC, an
    // extension and padding that is wrong in three ways; here none of the three leaks.
    let payload = [0x55u8; 160];
    let d = build(
        false,
        0,
        1,
        160,
        1,
        &[0xAAAA_AAAA],
        Some((0xBEDE, &[0xEE; 4])),
        &payload,
        2,
    );
    let sipress_payload = &d[12..];
    assert_ne!(sipress_payload.len(), payload.len());
    assert_eq!(RtpPacket::decode(&d).unwrap().payload, payload);
}

#[test]
fn g03_encode_limits_csrcs_to_15_and_pads_the_extension() {
    let mut p = RtpPacket::new(0, 1, 2, 3, vec![9]);
    p.csrc = (0..20).collect();
    let q = RtpPacket::decode(&p.encode()).unwrap();
    assert_eq!(q.csrc, (0..15).collect::<Vec<u32>>());
    assert_eq!(q.payload, vec![9]);
    // extension data that is not a multiple of 4 bytes is padded with zeros
    let mut p = RtpPacket::new(0, 1, 2, 3, vec![9]);
    p.extension = Some(Extension {
        profile: 7,
        data: vec![1, 2, 3, 4, 5],
    });
    let q = RtpPacket::decode(&p.encode()).unwrap();
    assert_eq!(q.extension.unwrap().data, vec![1, 2, 3, 4, 5, 0, 0, 0]);
    assert_eq!(q.payload, vec![9]);
}

// ------------------------------------------------------------------------------------- G04

fn packet_strategy() -> impl Strategy<Value = RtpPacket> {
    (
        any::<bool>(),
        0u8..=127,
        any::<u16>(),
        any::<u32>(),
        any::<u32>(),
        proptest::collection::vec(any::<u32>(), 0..=15),
        proptest::option::of((
            any::<u16>(),
            proptest::collection::vec(any::<u8>(), 0..=16).prop_map(|mut v| {
                v.truncate(v.len() / 4 * 4);
                v
            }),
        )),
        proptest::collection::vec(any::<u8>(), 0..=400),
    )
        .prop_map(
            |(marker, payload_type, sequence, timestamp, ssrc, csrc, ext, payload)| RtpPacket {
                marker,
                payload_type,
                sequence,
                timestamp,
                ssrc,
                csrc,
                extension: ext.map(|(profile, data)| Extension { profile, data }),
                payload,
            },
        )
}

fn runner(seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases: 2000,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

#[test]
fn g04_encode_then_decode_is_the_identity() {
    for seed in [1u8, 2, 3, 4] {
        runner(seed)
            .run(&packet_strategy(), |p| {
                let bytes = p.encode();
                let q =
                    RtpPacket::decode(&bytes).map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(&q, &p);
                // the length is exactly header + CSRCs + extension + payload
                let ext = p.extension.as_ref().map_or(0, |e| 4 + e.data.len());
                prop_assert_eq!(
                    bytes.len(),
                    RTP_HEADER_SIZE + 4 * p.csrc.len() + ext + p.payload.len()
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn g04_decode_of_arbitrary_bytes_never_panics_and_a_decoded_packet_survives_a_round_trip() {
    for seed in [1u8, 2, 3] {
        runner(seed)
            .run(
                &proptest::collection::vec(any::<u8>(), 0..=300),
                |mut bytes| {
                    // half of the inputs get a plausible first byte so that the deeper paths run
                    if bytes.len() > 12 && bytes[2] & 1 == 0 {
                        bytes[0] = (bytes[0] & 0x3F) | 0x80;
                    }
                    if let Ok(p) = RtpPacket::decode(&bytes) {
                        prop_assert!(p.payload.len() <= bytes.len());
                        let again = RtpPacket::decode(&p.encode())
                            .map_err(|e| TestCaseError::fail(e.to_string()))?;
                        prop_assert_eq!(again, p);
                    }
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn g04_the_g711_payload_survives_the_wire() {
    // 20 ms of every sample value through encode -> RTP -> decode -> decode again
    for law in [Law::Ulaw, Law::Alaw] {
        let pcm: Vec<i16> = (0..160).map(|i| (i * 400 - 32000) as i16).collect();
        let mut codes = Vec::new();
        law.encode(&pcm, &mut codes);
        let pkt = RtpPacket::new(law.payload_type(), 100, 16_000, 0x1234_5678, codes.clone());
        let back = RtpPacket::decode(&pkt.encode()).unwrap();
        assert_eq!(Law::from_payload_type(back.payload_type), Some(law));
        let mut out = Vec::new();
        law.decode(&back.payload, &mut out);
        let mut direct = Vec::new();
        law.decode(&codes, &mut direct);
        assert_eq!(out, direct);
    }
}
