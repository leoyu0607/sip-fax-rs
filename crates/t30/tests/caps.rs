//! R02 (DIS content, bit for bit) and R03 (DCS parsing).

mod common;

use common::*;
use t30::{
    Compression, Config, Dcs, DcsError, Dis, MinScanTime, Modem, Modems, PaperLength, PaperWidth,
    Receiver, Resolution, Unsupported, build_dis_fif, decode_ident, encode_ident, fif_bit,
};

/// The Table 2 bit numbers that are set in `fif`, ascending.
fn set_bits(fif: &[u8]) -> Vec<usize> {
    (1..=fif.len() * 8).filter(|&n| fif_bit(fif, n)).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn bin(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:08b}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn base() -> Config {
    Config::default()
}

// ---- R02 -------------------------------------------------------------------------------

/// (name, config, expected FIF, expected set bits). The expectations were computed by hand
/// from T.30 Table 2: bit n is `0x80 >> ((n-1) % 8)` of octet `(n-1)/8`.
#[allow(clippy::type_complexity)]
fn r02_cases() -> Vec<(&'static str, Config, Vec<u8>, Vec<usize>)> {
    let mut v = Vec::new();
    // Defaults: V.27ter + V.29 + V.17, fine, MR, A4, unlimited length, 0 ms, no ECM, no MMR.
    v.push((
        "default",
        base(),
        vec![0x00, 0x77, 0x1E],
        vec![10, 11, 12, 14, 15, 16, 20, 21, 22, 23],
    ));
    v.push((
        "t38 internet-aware",
        Config {
            t38_iaf: true,
            ..base()
        },
        vec![0x20, 0x77, 0x1E],
        vec![3, 10, 11, 12, 14, 15, 16, 20, 21, 22, 23],
    ));
    let mut c = base();
    c.modems = Modems {
        v27ter: true,
        v29: false,
        v17: false,
    };
    v.push((
        "V.27ter only",
        c.clone(),
        vec![0x00, 0x53, 0x1E],
        vec![10, 12, 15, 16, 20, 21, 22, 23],
    ));
    c.modems = Modems {
        v27ter: false,
        v29: true,
        v17: false,
    };
    v.push((
        "V.29 only",
        c.clone(),
        vec![0x00, 0x63, 0x1E],
        vec![10, 11, 15, 16, 20, 21, 22, 23],
    ));
    c.modems = Modems {
        v27ter: false,
        v29: false,
        v17: true,
    };
    v.push((
        "V.17 only (forces 11 and 12)",
        c.clone(),
        vec![0x00, 0x77, 0x1E],
        vec![10, 11, 12, 14, 15, 16, 20, 21, 22, 23],
    ));
    c.modems = Modems {
        v27ter: false,
        v29: false,
        v17: false,
    };
    v.push((
        "no high speed modem (V.27ter fallback 2400)",
        c.clone(),
        vec![0x00, 0x43, 0x1E],
        vec![10, 15, 16, 20, 21, 22, 23],
    ));
    let mut c = base();
    c.fine = false;
    v.push((
        "standard resolution only",
        c.clone(),
        vec![0x00, 0x75, 0x1E],
        vec![10, 11, 12, 14, 16, 20, 21, 22, 23],
    ));
    c.mr = false;
    v.push((
        "MH only",
        c.clone(),
        vec![0x00, 0x74, 0x1E],
        vec![10, 11, 12, 14, 20, 21, 22, 23],
    ));
    let mut c = base();
    c.max_width = PaperWidth::B4;
    v.push((
        "width 255 mm",
        c.clone(),
        vec![0x00, 0x77, 0x9E],
        vec![10, 11, 12, 14, 15, 16, 17, 20, 21, 22, 23],
    ));
    c.max_width = PaperWidth::A3;
    v.push((
        "width 303 mm",
        c.clone(),
        vec![0x00, 0x77, 0x5E],
        vec![10, 11, 12, 14, 15, 16, 18, 20, 21, 22, 23],
    ));
    let mut c = base();
    c.max_length = PaperLength::A4;
    v.push((
        "length A4",
        c.clone(),
        vec![0x00, 0x77, 0x0E],
        vec![10, 11, 12, 14, 15, 16, 21, 22, 23],
    ));
    c.max_length = PaperLength::B4;
    v.push((
        "length B4",
        c.clone(),
        vec![0x00, 0x77, 0x2E],
        vec![10, 11, 12, 14, 15, 16, 19, 21, 22, 23],
    ));
    for (t, byte, bits) in [
        (MinScanTime::Ms20, 0x10u8, vec![20]),
        (MinScanTime::Ms5, 0x18, vec![20, 21]),
        (MinScanTime::Ms10, 0x14, vec![20, 22]),
        (MinScanTime::Ms40, 0x12, vec![20, 23]),
        (MinScanTime::Ms0, 0x1E, vec![20, 21, 22, 23]),
    ] {
        let mut c = base();
        c.min_scan = t;
        let mut b = vec![10, 11, 12, 14, 15, 16];
        b.extend(bits);
        v.push((
            match t {
                MinScanTime::Ms20 => "min scan 20 ms",
                MinScanTime::Ms5 => "min scan 5 ms",
                MinScanTime::Ms10 => "min scan 10 ms",
                MinScanTime::Ms40 => "min scan 40 ms",
                _ => "min scan 0 ms",
            },
            c,
            vec![0x00, 0x77, byte],
            b,
        ));
    }
    // MMR needs the fourth octet; the extend bit 24 links it.
    let mut c = base();
    c.mmr = true;
    v.push((
        "MMR offered",
        c.clone(),
        vec![0x00, 0x77, 0x1F, 0x02],
        vec![10, 11, 12, 14, 15, 16, 20, 21, 22, 23, 24, 31],
    ));
    c.t38_iaf = true;
    v.push((
        "everything",
        c,
        vec![0x20, 0x77, 0x1F, 0x02],
        vec![3, 10, 11, 12, 14, 15, 16, 20, 21, 22, 23, 24, 31],
    ));
    v
}

#[test]
fn r02_dis_fif_matches_table_2_bit_for_bit() {
    for (name, cfg, want, want_bits) in r02_cases() {
        let got = build_dis_fif(&cfg);
        assert_eq!(hex(&got), hex(&want), "{name}: FIF octets");
        // The same thing seen bit by bit (independent of the octet arithmetic above).
        let mut bits = want_bits.clone();
        bits.sort_unstable();
        bits.dedup();
        assert_eq!(set_bits(&got), bits, "{name}: bit numbers\n  {}", bin(&got));
    }
}

#[test]
fn r02_bits_that_must_never_be_set() {
    // Never claimed: ready to transmit (9), V.8 (6), 64 octet ECM (7/28), ECM (27), uncompressed
    // (26), T.6 without the MMR option, 2-D/superfine resolutions (41..43), colour, T.4 options.
    for (name, cfg, _, _) in r02_cases() {
        let fif = build_dis_fif(&cfg);
        for forbidden in [1, 2, 4, 5, 6, 7, 8, 9, 13, 25, 26, 27, 28, 29, 30, 32] {
            assert!(!fif_bit(&fif, forbidden), "{name}: bit {forbidden}");
        }
        if !cfg.mmr {
            assert!(!fif_bit(&fif, 31), "{name}: T.6 only when configured");
            assert_eq!(fif.len(), 3, "{name}: no fourth octet without MMR");
        }
        // The extend bit is set exactly when another octet follows.
        assert_eq!(fif_bit(&fif, 24), fif.len() > 3, "{name}");
        assert!(!fif_bit(&fif, 8), "{name}: bit 8 is reserved");
        assert_eq!(fif_bit(&fif, 16), cfg.mr, "{name}: bit 16 is 2-D coding");
    }
}

#[test]
fn r02_octet_one_and_two_have_no_extend_bit() {
    // Bit 16 is the 2-D coding capability, not an extend bit; bit 8 is reserved.
    let mut c = base();
    c.mr = false;
    let fif = build_dis_fif(&c);
    assert!(!fif_bit(&fif, 16));
    assert!(!fif_bit(&fif, 8));
}

#[test]
fn r02_complete_dis_and_csi_frames() {
    let cfg = Config {
        local_id: "0287654321".into(),
        ..base()
    };
    let r = Receiver::new(cfg);
    let burst = r.dis_burst();
    assert_eq!(burst.len(), 2);
    // CSI: FF C0 02, then the number reversed ("1234567820"), every octet bit reversed
    // (1 = 8C, 2 = 4C, 3 = CC, 4 = 2C, 5 = AC, 6 = 6C, 7 = EC, 8 = 1C, 0 = 0C), padded with
    // spaces (04) to 20 octets. SpanDSP's own CSI carries the same encoding.
    let mut csi = vec![0xFF, 0xC0, 0x02];
    csi.extend_from_slice(&[0x8C, 0x4C, 0xCC, 0x2C, 0xAC, 0x6C, 0xEC, 0x1C, 0x4C, 0x0C]);
    csi.extend_from_slice(&[0x04; 10]);
    assert_eq!(hex(&burst[0]), hex(&csi));
    // DIS: FF C8 01 + FIF
    assert_eq!(hex(&burst[1]), "FF C8 01 00 77 1E");
}

#[test]
fn r02_ident_round_trip_and_edge_cases() {
    let mut want = [0x04u8; 20];
    want[0] = 0x4C; // 2
    want[1] = 0x2C; // 4
    assert_eq!(encode_ident("42"), want);
    assert_eq!(decode_ident(&encode_ident("0287654321")), "0287654321");
    assert_eq!(
        decode_ident(&encode_ident("+886 2 1234 5678")),
        "+886 2 1234 5678"
    );
    assert_eq!(decode_ident(&encode_ident("")), "");
    // longer than 20: the last 20 characters survive
    let long = "012345678901234567890123";
    assert_eq!(decode_ident(&encode_ident(long)), &long[4..]);
    // non printable octets are blanked, never panic
    assert_eq!(
        encode_ident("a\u{7f}b")[0..3],
        [b'b'.reverse_bits(), 0x04, b'a'.reverse_bits()]
    );
    assert_eq!(decode_ident(&[0x00, 0x01, 0x8C, 0xFF]), "1");
    assert_eq!(decode_ident(&[]), "");
    assert_eq!(decode_ident(&[b'x'.reverse_bits(); 40]).len(), 20);
}

// ---- cross-check against what real SpanDSP 3.1.1 puts on the wire ----------------------------

/// DIS frames taken from the golden T.38 sessions (crates/t38/tests/vectors/spandsp, decoded
/// with a throw-away script): the FIF of the answering terminal's DIS.
const SPANDSP_DIS: &[(&str, &str)] = &[
    ("non_ecm V.27ter+V.29+V.17", "20 77 1F 01 01 99 01 01 01 18"),
    ("non_ecm V.29 only", "20 63 1F 01 01 99 01 01 01 18"),
    ("non_ecm V.27ter only", "20 53 1F 01 01 99 01 01 01 18"),
    ("ecm V.27ter+V.29+V.17", "20 77 1F 23 01 99 01 01 01 18"),
];

fn unhex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|x| u8::from_str_radix(x, 16).unwrap())
        .collect()
}

#[test]
fn r02_spandsp_dis_parses_to_the_same_capabilities_as_ours() {
    let ours = Dis::parse(&build_dis_fif(&Config {
        t38_iaf: true,
        ..base()
    }));
    let theirs = Dis::parse(&unhex(SPANDSP_DIS[0].1));
    // SpanDSP adds capabilities we do not claim (ECM bits come with other rows) but every
    // capability we do claim is read identically.
    assert_eq!(ours.t38_iaf, theirs.t38_iaf);
    assert_eq!(ours.can_receive, theirs.can_receive);
    assert_eq!(ours.modem_bits, theirs.modem_bits);
    assert_eq!(ours.fine, theirs.fine);
    assert_eq!(ours.mr, theirs.mr);
    assert_eq!(ours.width, theirs.width);
    assert_eq!(ours.length, theirs.length);
    assert_eq!(ours.min_scan, theirs.min_scan);
    assert_eq!(ours.ecm, theirs.ecm);
    assert_eq!(ours.mmr, theirs.mmr);
    // And the first three octets are SpanDSP's with only the extend bit differing.
    let mine = build_dis_fif(&Config {
        t38_iaf: true,
        ..base()
    });
    let theirs = unhex(SPANDSP_DIS[0].1);
    assert_eq!(hex(&mine), "20 77 1E");
    assert_eq!(&theirs[..2], &mine[..2]);
    assert_eq!(theirs[2] & 0xFE, mine[2]);
    assert_eq!(
        theirs[2] & 0x01,
        1,
        "SpanDSP's extend bit: it sends more octets"
    );
}

#[test]
fn r02_each_spandsp_dis_decodes_to_the_modems_it_was_configured_with() {
    let cases = [
        (0, (true, true, true)),
        (1, (false, true, false)),
        (2, (true, false, false)),
        (3, (true, true, true)),
    ];
    for (i, (v27, v29, v17)) in cases {
        let (name, h) = SPANDSP_DIS[i];
        let d = Dis::parse(&unhex(h));
        assert_eq!((d.v27ter(), d.v29(), d.v17()), (v27, v29, v17), "{name}");
        assert!(d.can_receive && d.fine && d.mr, "{name}");
        assert_eq!(d.ecm, name.starts_with("ecm"), "{name}");
        assert_eq!(
            d.mmr,
            name.starts_with("ecm"),
            "{name}: SpanDSP offers T.6 only with ECM"
        );
        assert!(!d.v33(), "{name}");
    }
}

#[test]
fn r02_dis_round_trips_through_parse() {
    for (name, cfg, _, _) in r02_cases() {
        let d = Dis::parse(&build_dis_fif(&cfg));
        assert_eq!(d.t38_iaf, cfg.t38_iaf, "{name}");
        assert!(d.can_receive, "{name}");
        assert!(
            !d.ready_to_transmit && !d.ecm && !d.uncompressed && !d.v8,
            "{name}"
        );
        assert_eq!(d.fine, cfg.fine, "{name}");
        assert_eq!(d.mr, cfg.mr, "{name}");
        assert_eq!(d.mmr, cfg.mmr, "{name}");
        assert_eq!(d.width, cfg.max_width, "{name}");
        assert_eq!(d.length, cfg.max_length, "{name}");
        assert_eq!(d.min_scan, cfg.min_scan, "{name}");
        let m = cfg.modems;
        assert_eq!(d.v29(), m.v29 || m.v17, "{name}");
        assert_eq!(d.v27ter(), m.v27ter || m.v17, "{name}");
        assert_eq!(d.v17(), m.v17, "{name}");
    }
}

// ---- R03 -------------------------------------------------------------------------------

#[test]
fn r03_dcs_data_signalling_rates() {
    let table: &[(&[usize], Modem, u32)] = &[
        (&[], Modem::V27ter, 2400),
        (&[12], Modem::V27ter, 4800),
        (&[11], Modem::V29, 9600),
        (&[11, 12], Modem::V29, 7200),
        (&[13], Modem::V33, 14400),
        (&[12, 13], Modem::V33, 12000),
        (&[11, 14], Modem::V17, 9600),
        (&[14], Modem::V17, 14400),
        (&[12, 14], Modem::V17, 12000),
        (&[11, 12, 14], Modem::V17, 7200),
    ];
    for &(bits, modem, rate) in table {
        let mut b = vec![10];
        b.extend(bits);
        let dcs = Dcs::parse(&fif_with_bits(3, &b)).unwrap();
        assert_eq!((dcs.modem, dcs.bit_rate), (modem, rate), "bits {bits:?}");
        assert!(dcs.receive);
    }
    // The six reserved combinations.
    for bits in [
        &[13, 14][..],
        &[11, 13],
        &[11, 12, 13],
        &[12, 13, 14],
        &[11, 13, 14],
        &[11, 12, 13, 14],
    ] {
        let mut b = vec![10];
        b.extend(bits);
        let r = Dcs::parse(&fif_with_bits(3, &b));
        assert!(
            matches!(r, Err(DcsError::ReservedModem(_))),
            "bits {bits:?}: {r:?}"
        );
    }
}

#[test]
fn r03_dcs_resolution_compression_width_length_and_scan_time() {
    // vertical resolution
    for (bits, res, y) in [
        (vec![], Resolution::Standard, 98),
        (vec![15], Resolution::Fine, 196),
        (vec![41], Resolution::Superfine, 391),
        (vec![42], Resolution::Dpi300, 300),
        (vec![43], Resolution::Dpi400, 400),
    ] {
        let mut b = vec![10, 14];
        b.extend(&bits);
        let d = Dcs::parse(&fif_with_bits(6, &b)).unwrap();
        assert_eq!(
            (d.resolution, d.y_dpi(), d.x_dpi()),
            (res, y, 204),
            "bits {bits:?}"
        );
    }
    // inch based: bit 44
    let d = Dcs::parse(&fif_with_bits(6, &[10, 14, 15, 44])).unwrap();
    assert_eq!(
        (d.resolution, d.inch_based, d.x_dpi(), d.y_dpi()),
        (Resolution::Fine, true, 200, 200)
    );
    let d = Dcs::parse(&fif_with_bits(6, &[10, 14, 44])).unwrap();
    assert_eq!((d.x_dpi(), d.y_dpi()), (200, 100));
    // compression
    for (bits, c) in [
        (vec![], Compression::Mh),
        (vec![16], Compression::Mr),
        (vec![31], Compression::Mmr),
        (vec![16, 31], Compression::Mmr),
    ] {
        let mut b = vec![10, 14];
        b.extend(&bits);
        let d = Dcs::parse(&fif_with_bits(4, &b)).unwrap();
        assert_eq!(d.compression, c, "bits {bits:?}");
    }
    // width: 00 / 10 / 01 / 11 (invalid 11 treated as 303 mm, T.30 Table 2 note 6)
    for (bits, w, px) in [
        (vec![], PaperWidth::A4, 1728),
        (vec![17], PaperWidth::B4, 2048),
        (vec![18], PaperWidth::A3, 2432),
        (vec![17, 18], PaperWidth::A3, 2432),
    ] {
        let mut b = vec![10, 14];
        b.extend(&bits);
        let d = Dcs::parse(&fif_with_bits(3, &b)).unwrap();
        assert_eq!((d.width, d.width.pixels()), (w, px), "bits {bits:?}");
    }
    // length
    for (bits, l) in [
        (vec![], PaperLength::A4),
        (vec![19], PaperLength::B4),
        (vec![20], PaperLength::Unlimited),
        (vec![19, 20], PaperLength::Unlimited),
    ] {
        let mut b = vec![10, 14];
        b.extend(&bits);
        let d = Dcs::parse(&fif_with_bits(3, &b)).unwrap();
        assert_eq!(d.length, l, "bits {bits:?}");
    }
    // minimum scan line time codes (SpanDSP numbering)
    for (bits, t) in [
        (vec![], MinScanTime::Ms20),
        (vec![21], MinScanTime::Ms5),
        (vec![22], MinScanTime::Ms10),
        (vec![23], MinScanTime::Ms40),
        (vec![21, 22, 23], MinScanTime::Ms0),
        (vec![21, 22], MinScanTime::Other(3)),
    ] {
        let mut b = vec![10, 14];
        b.extend(&bits);
        let d = Dcs::parse(&fif_with_bits(3, &b)).unwrap();
        assert_eq!(d.min_scan, t, "bits {bits:?}");
    }
}

#[test]
fn r03_dcs_extension_octets_and_flags() {
    // The extend bit (24) is not interpreted by the parser; bits beyond it are read when the
    // octets are there and read as 0 when the DCS is short.
    let d = Dcs::parse(&fif_with_bits(4, &[10, 14, 24, 27])).unwrap();
    assert!(d.ecm);
    assert_eq!(d.unsupported(), Some(Unsupported::Ecm));
    let d = Dcs::parse(&fif_with_bits(3, &[10, 14])).unwrap();
    assert!(!d.ecm);
    assert_eq!(d.unsupported(), None);
    let d = Dcs::parse(&fif_with_bits(4, &[10, 14, 24, 26])).unwrap();
    assert_eq!(d.unsupported(), Some(Unsupported::Uncompressed));
    let d = Dcs::parse(&fif_with_bits(6, &[10, 14, 41])).unwrap();
    assert_eq!(
        d.unsupported(),
        Some(Unsupported::Resolution(Resolution::Superfine))
    );
    let d = Dcs::parse(&fif_with_bits(3, &[3, 10, 14])).unwrap();
    assert!(d.t38_iaf);
    // more octets than anything we know about are harmless
    let mut long = fif_with_bits(12, &[10, 14, 24, 32, 40, 48, 56, 64, 72, 88]);
    long[11] = 0xFF;
    assert!(Dcs::parse(&long).is_ok());
    // too short
    assert_eq!(Dcs::parse(&[]), Err(DcsError::TooShort(0)));
    assert_eq!(Dcs::parse(&[0x40, 0x04]), Err(DcsError::TooShort(2)));
}

#[test]
fn r03_spandsp_dcs_frames_parse_as_sent() {
    // (FIF, modem, rate, resolution, compression, ecm) from the golden sessions
    // (crates/t38/tests/vectors/spandsp: the caller's DCS in each session).
    let cases = [
        (
            "00 45 1E",
            Modem::V17,
            14400,
            Resolution::Standard,
            Compression::Mr,
            false,
        ),
        (
            "00 47 1E",
            Modem::V17,
            14400,
            Resolution::Fine,
            Compression::Mr,
            false,
        ),
        (
            "00 61 1E",
            Modem::V29,
            9600,
            Resolution::Standard,
            Compression::Mr,
            false,
        ),
        (
            "00 63 1E",
            Modem::V29,
            9600,
            Resolution::Fine,
            Compression::Mr,
            false,
        ),
        (
            "00 51 1E",
            Modem::V27ter,
            4800,
            Resolution::Standard,
            Compression::Mr,
            false,
        ),
        (
            "00 53 1E",
            Modem::V27ter,
            4800,
            Resolution::Fine,
            Compression::Mr,
            false,
        ),
        (
            "00 44 1F 22",
            Modem::V17,
            14400,
            Resolution::Standard,
            Compression::Mmr,
            true,
        ),
        (
            "00 46 1F 22",
            Modem::V17,
            14400,
            Resolution::Fine,
            Compression::Mmr,
            true,
        ),
    ];
    for (h, modem, rate, res, comp, ecm) in cases {
        let d = Dcs::parse(&unhex(h)).unwrap();
        assert_eq!(
            (d.modem, d.bit_rate, d.resolution, d.compression, d.ecm),
            (modem, rate, res, comp, ecm),
            "{h}"
        );
        assert_eq!(d.width, PaperWidth::A4, "{h}");
        assert_eq!(d.min_scan, MinScanTime::Ms0, "{h}");
        assert_eq!(d.length, PaperLength::Unlimited, "{h}");
        assert!(d.receive, "{h}");
    }
}

#[test]
fn r03_dcs_geometry_helpers() {
    let d = Dcs::parse(&fif_with_bits(3, &[10, 14, 15, 17])).unwrap();
    assert_eq!((d.width.pixels(), d.x_dpi(), d.y_dpi()), (2048, 204, 196));
}
