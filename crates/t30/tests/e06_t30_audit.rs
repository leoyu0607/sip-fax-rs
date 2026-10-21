//! E06: the bit positions and FCF codes this crate uses, checked against the text of ITU-T
//! Recommendation T.30 (09/2005) and T.4 (07/2003) Annex A.
//!
//! Every literal below was typed from the Recommendation, not taken from the crate: the DIS / DCS
//! bit numbers of Table 2, the binary formats of 5.3.6.1.x ("X111 1101" with X printed first), the
//! FCF2 table of Figure A.1, the FCD / RCP formats of T.4 A.3.5. The clause or note is in each
//! row's name so that a failure points at the text to re-read.
//!
//! Octet convention (crate::frame): the first transmitted bit is the most significant one, which
//! is the order the Recommendation prints. "Bit n" of a DIS / DCS is `fif[(n-1)/8] & (0x80 >> ((n-1)%8))`.

mod common;

use common::fif_with_bits;
use t30::caps::bits;
use t30::{
    Compression, Config, Dcs, DcsError, Dis, MinScanTime, Modem, PaperLength, PaperWidth,
    Resolution, build_dis_fif, fcf, fcf2, lsb_first,
};

/// "X111 1101" / "0000 0001" to the octet, X read as 0 (the caller of a DIS sets it to 1).
fn fmt(s: &str) -> u8 {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == 'X' { '0' } else { c })
        .fold(0u8, |acc, c| (acc << 1) | u8::from(c == '1'))
}

#[test]
fn e06_fcf_values_match_the_binary_formats_of_5_3_6_1() {
    // (clause, format as printed, our constant)
    let table: &[(&str, &str, u8)] = &[
        ("5.3.6.1.1 1) DIS", "0000 0001", fcf::DIS),
        ("5.3.6.1.1 2) CSI", "0000 0010", fcf::CSI),
        ("5.3.6.1.1 3) NSF", "0000 0100", fcf::NSF),
        ("5.3.6.1.2 1) DTC", "1000 0001", fcf::DTC),
        ("5.3.6.1.2 2) CIG", "1000 0010", fcf::CIG),
        ("5.3.6.1.3 1) DCS", "X100 0001", fcf::DCS),
        ("5.3.6.1.3 2) TSI", "X100 0010", fcf::TSI),
        ("5.3.6.1.3 3) NSS", "X100 0100", fcf::NSS),
        ("5.3.6.1.3 4) SUB", "X100 0011", fcf::SUB),
        ("5.3.6.1.3 5) SID", "X100 0101", fcf::SID),
        ("5.3.6.1.3 7) CTC / A.4.1", "X100 1000", fcf::CTC),
        ("5.3.6.1.4 1) CFR", "X010 0001", fcf::CFR),
        ("5.3.6.1.4 2) FTT", "X010 0010", fcf::FTT),
        ("5.3.6.1.4 3) CTR / A.4.2", "X010 0011", fcf::CTR),
        ("5.3.6.1.6 1) EOM", "X111 0001", fcf::EOM),
        ("5.3.6.1.6 2) MPS", "X111 0010", fcf::MPS),
        ("5.3.6.1.6 3) EOP", "X111 0100", fcf::EOP),
        ("5.3.6.1.6 4) PRI-EOM", "X111 1001", fcf::PRI_EOM),
        ("5.3.6.1.6 5) PRI-MPS", "X111 1010", fcf::PRI_MPS),
        ("5.3.6.1.6 6) PRI-EOP", "X111 1100", fcf::PRI_EOP),
        ("5.3.6.1.6 7) EOS", "X111 1000", fcf::EOS),
        ("A.4.3 1) PPS", "X111 1101", fcf::PPS),
        ("A.4.3 2) EOR", "X111 0011", fcf::EOR),
        ("A.4.3 3) RR", "X111 0110", fcf::RR),
        ("5.3.6.1.7 1) MCF", "X011 0001", fcf::MCF),
        ("5.3.6.1.7 2) RTP", "X011 0011", fcf::RTP),
        ("5.3.6.1.7 3) RTN", "X011 0010", fcf::RTN),
        ("5.3.6.1.7 4) PIP", "X011 0101", fcf::PIP),
        ("5.3.6.1.7 5) PIN", "X011 0100", fcf::PIN),
        ("A.4.4 1) PPR", "X011 1101", fcf::PPR),
        ("A.4.4 2) RNR", "X011 0111", fcf::RNR),
        ("A.4.4 3) ERR", "X011 1000", fcf::ERR),
        ("5.3.6.1.8 1) DCN", "X101 1111", fcf::DCN),
        ("5.3.6.1.8 2) CRP", "X101 1000", fcf::CRP),
        ("5.3.6.1.8 3) FNV", "X101 0011", fcf::FNV),
        ("T.4 A.3.5 1) FCD", "0110 0000", fcf::FCD),
        ("T.4 A.3.5 2) RCP", "0110 0001", fcf::RCP),
    ];
    for (clause, printed, ours) in table {
        assert_eq!(fmt(printed), *ours, "{clause}: {printed}");
    }
    // 5.3.4 address, 5.3.5 control "1100 X000" (X = 1 for the final frame)
    assert_eq!(fmt("1111 1111"), t30::frame::ADDRESS);
    assert_eq!(fmt("1100 0000"), t30::frame::CONTROL_NON_FINAL);
    assert_eq!(fmt("1100 1000"), t30::frame::CONTROL_FINAL);
}

#[test]
fn e06_fcf2_values_of_figure_a1() {
    for (printed, ours) in [
        ("0000 0000", fcf2::NULL),
        ("1111 0001", fcf2::EOM),
        ("1111 0010", fcf2::MPS),
        ("1111 0100", fcf2::EOP),
        ("1111 1000", fcf2::EOS),
        ("1111 1001", fcf2::PRI_EOM),
        ("1111 1010", fcf2::PRI_MPS),
        ("1111 1100", fcf2::PRI_EOP),
    ] {
        assert_eq!(fmt(printed), ours, "FCF2 {printed}");
    }
    // Figure A.1 example: PC = 1, BC = 2, FC = 10 transmitted as
    // 1000 0000 | 0100 0000 | 0101 0000 (least significant bit first, printed left to right)
    assert_eq!(
        [lsb_first(1), lsb_first(2), lsb_first(10)],
        [fmt("1000 0000"), fmt("0100 0000"), fmt("0101 0000")]
    );
}

#[test]
fn e06_table_2_bit_numbers() {
    // (bit number in Table 2, our constant, what the row says)
    for (n, ours, what) in [
        (3, bits::T38, "Real-time Internet fax (T.38), note 61"),
        (6, bits::V8, "V.8 capabilities"),
        (
            7,
            bits::FRAME_SIZE_PREF,
            "DIS: 0 = 256 octets preferred, 1 = 64",
        ),
        (
            9,
            bits::READY_TO_TRANSMIT,
            "DIS: ready to transmit (polling)",
        ),
        (10, bits::RECEIVE, "receiver fax operation"),
        (
            11,
            bits::MODEM_FIRST,
            "data signalling rate, first of 11..14",
        ),
        (15, bits::FINE, "R8 x 7.7 lines/mm and/or 200 x 200"),
        (16, bits::TWO_D, "two-dimensional coding"),
        (17, bits::WIDTH_1, "recording width"),
        (18, bits::WIDTH_2, "recording width"),
        (19, bits::LENGTH_1, "recording length"),
        (20, bits::LENGTH_2, "recording length"),
        (21, bits::MIN_SCAN_FIRST, "minimum scan line time 21..23"),
        (24, bits::EXTEND_24, "extend field"),
        (26, bits::UNCOMPRESSED, "uncompressed mode"),
        (27, bits::ECM, "error correction mode"),
        (
            28,
            bits::ECM_64,
            "DCS: frame size 0 = 256, 1 = 64 octets; DIS: set to 0",
        ),
        (31, bits::T6, "T.6 coding capability / enabled"),
        (41, bits::SUPERFINE_200_400, "R8 x 15.4 lines/mm"),
        (42, bits::RES_300_300, "300 x 300 pels/25.4 mm"),
        (43, bits::RES_400_400, "R16 x 15.4 and/or 400 x 400"),
        (
            44,
            bits::INCH,
            "inch based resolution preferred / DCS: 1 = inch",
        ),
        (45, bits::METRIC, "metric based resolution preferred"),
    ] {
        assert_eq!(ours, n, "{what}");
    }
}

#[test]
fn e06_the_dis_bits_we_set_and_the_extend_bits() {
    let mut cfg = Config {
        ecm: true,
        mmr: true,
        ..Config::default()
    };
    let fif = build_dis_fif(&cfg);
    // Only these bits are 1 in the default ECM DIS: 10 (receiver), 11 12 14 (V.27ter, V.29, V.17
    // together, Table 2 note 31 combination 1101), 15 (fine), 16 (2-D), 20 (unlimited length),
    // 21 22 23 (0 ms), 24 (extend: a fourth octet follows), 27 (ECM), 31 (T.6).
    let want = [10, 11, 12, 14, 15, 16, 20, 21, 22, 23, 24, 27, 31];
    for n in 1..=40 {
        assert_eq!(t30::fif_bit(&fif, n), want.contains(&n), "DIS bit {n}");
    }
    assert_eq!(fif, fif_with_bits(4, &want));
    // bit 32 (extend of the fourth octet) is not set: nothing follows
    // T.6 needs ECM (notes 9 and 17): MMR without ECM sets neither
    cfg.ecm = false;
    assert!(!t30::fif_bit(&build_dis_fif(&cfg), 31));
}

#[test]
fn e06_data_signalling_rate_table_of_bits_11_to_14() {
    // (b11 b12 b13 b14 as printed, DCS meaning)
    enum W {
        Rate(Modem, u32),
        Legacy(Modem, u32),
        Reserved,
    }
    let rows = [
        ([0, 0, 0, 0], W::Rate(Modem::V27ter, 2400)),
        ([0, 1, 0, 0], W::Rate(Modem::V27ter, 4800)),
        ([1, 0, 0, 0], W::Rate(Modem::V29, 9600)),
        ([1, 1, 0, 0], W::Rate(Modem::V29, 7200)),
        // "Invalid" in a DCS; note 31: terminals of 1994 and earlier used them for V.33
        ([0, 0, 1, 0], W::Legacy(Modem::V33, 14400)),
        ([0, 1, 1, 0], W::Legacy(Modem::V33, 12000)),
        ([1, 0, 1, 0], W::Reserved),
        ([1, 1, 1, 0], W::Reserved),
        ([0, 0, 0, 1], W::Rate(Modem::V17, 14400)),
        ([0, 1, 0, 1], W::Rate(Modem::V17, 12000)),
        ([1, 0, 0, 1], W::Rate(Modem::V17, 9600)),
        ([1, 1, 0, 1], W::Rate(Modem::V17, 7200)),
        ([0, 0, 1, 1], W::Reserved),
        ([0, 1, 1, 1], W::Reserved),
        ([1, 0, 1, 1], W::Reserved),
        ([1, 1, 1, 1], W::Reserved),
    ];
    for (b, want) in rows {
        let set: Vec<usize> = (0..4).filter(|i| b[*i] == 1).map(|i| 11 + i).collect();
        let mut all = vec![10, 20];
        all.extend(set);
        let fif = fif_with_bits(3, &all);
        let got = Dcs::parse(&fif);
        match (want, got) {
            (W::Rate(m, r) | W::Legacy(m, r), Ok(d)) => {
                assert_eq!((d.modem, d.bit_rate), (m, r), "bits 11-14 = {b:?}")
            }
            (W::Reserved, Err(DcsError::ReservedModem(_))) => {}
            (_, other) => panic!("bits 11-14 = {b:?}: {other:?}"),
        }
    }
    // the DIS reading of the same four bits (V.17 offers 11, 12 and 14 together)
    let d = Dis::parse(&fif_with_bits(3, &[10, 11, 12, 14]));
    assert!(d.v29() && d.v27ter() && d.v17() && !d.v33());
    // bit 13 alone is the V.33 position: DIS "Not used"
    assert!(Dis::parse(&fif_with_bits(3, &[13])).v33());
}

#[test]
fn e06_minimum_scan_line_time_codes_of_bits_21_to_23() {
    // (b21 b22 b23 as printed, DIS meaning)
    let rows: [([usize; 3], MinScanTime); 8] = [
        ([0, 0, 0], MinScanTime::Ms20),
        ([0, 0, 1], MinScanTime::Ms40),
        ([0, 1, 0], MinScanTime::Ms10),
        ([1, 0, 0], MinScanTime::Ms5),
        ([0, 1, 1], MinScanTime::Other(6)), // 10 ms at 3.85, T7.7 = 1/2 T3.85
        ([1, 1, 0], MinScanTime::Other(3)), // 20 ms at 3.85, T7.7 = 1/2 T3.85
        ([1, 0, 1], MinScanTime::Other(5)), // 40 ms at 3.85, T7.7 = 1/2 T3.85
        ([1, 1, 1], MinScanTime::Ms0),
    ];
    for (b, want) in rows {
        let set: Vec<usize> = (0..3).filter(|i| b[*i] == 1).map(|i| 21 + i).collect();
        let fif = fif_with_bits(3, &set);
        assert_eq!(Dis::parse(&fif).min_scan, want, "b21 b22 b23 = {b:?}");
        assert_eq!(
            Dcs::parse(&{
                let mut s = set.clone();
                s.push(10);
                fif_with_bits(3, &s)
            })
            .unwrap()
            .min_scan,
            want
        );
        // and the round trip through the DIS builder
        let cfg = Config {
            min_scan: want,
            ..Config::default()
        };
        let built = build_dis_fif(&cfg);
        for (i, expect) in b.iter().enumerate() {
            assert_eq!(
                t30::fif_bit(&built, 21 + i),
                *expect == 1,
                "{b:?} bit {}",
                21 + i
            );
        }
    }
}

#[test]
fn e06_recording_width_length_and_resolution_rows() {
    // bits 17 18: 00 215 mm; 01 303 mm in a DCS (A4, B4 and A3 in a DIS); 10 255 mm; 11 invalid,
    // to be read as 01 (note 6)
    for (b, want) in [
        ([0, 0], PaperWidth::A4),
        ([0, 1], PaperWidth::A3),
        ([1, 0], PaperWidth::B4),
        ([1, 1], PaperWidth::A3),
    ] {
        let set: Vec<usize> = (0..2).filter(|i| b[*i] == 1).map(|i| 17 + i).collect();
        assert_eq!(Dis::parse(&fif_with_bits(3, &set)).width, want, "{b:?}");
    }
    // bits 19 20: 00 A4; 01 unlimited; 10 B4 (DCS) / A4 and B4 (DIS)
    for (b, want) in [
        ([0, 0], PaperLength::A4),
        ([0, 1], PaperLength::Unlimited),
        ([1, 0], PaperLength::B4),
    ] {
        let set: Vec<usize> = (0..2).filter(|i| b[*i] == 1).map(|i| 19 + i).collect();
        assert_eq!(Dis::parse(&fif_with_bits(3, &set)).length, want, "{b:?}");
    }
    // DCS resolution: bit 15 alone = R8 x 7.7; 41 / 42 / 43 the higher ones; bit 44 = inch
    let dcs = |extra: &[usize]| {
        let mut v = vec![10, 20];
        v.extend(extra);
        Dcs::parse(&fif_with_bits(6, &v)).unwrap()
    };
    assert_eq!(dcs(&[]).resolution, Resolution::Standard);
    assert_eq!(dcs(&[15]).resolution, Resolution::Fine);
    assert_eq!(dcs(&[41]).resolution, Resolution::Superfine);
    assert_eq!(dcs(&[42]).resolution, Resolution::Dpi300);
    assert_eq!(dcs(&[43]).resolution, Resolution::Dpi400);
    assert!(!dcs(&[15]).inch_based && dcs(&[15, 44]).inch_based);
    // coding: bit 16 two-dimensional, bit 31 T.6; ECM bit 27, frame size bit 28
    assert_eq!(dcs(&[]).compression, Compression::Mh);
    assert_eq!(dcs(&[16]).compression, Compression::Mr);
    assert_eq!(dcs(&[27, 31]).compression, Compression::Mmr);
    assert!(dcs(&[27]).ecm && dcs(&[27]).frame_octets == 256);
    assert_eq!(dcs(&[27, 28]).frame_octets, 64);
    assert!(dcs(&[26]).uncompressed);
}

#[test]
fn e06_identification_digits_are_transmitted_least_significant_bit_first() {
    // 5.3.6.2.4: "The least significant bit of the least significant digit shall be the first bit
    // transmitted"; Table 3: the digit 1 is 0011 0001 (ASCII), so on the wire (first bit =
    // most significant bit of our octet) 1000 1100.
    let f = t30::encode_ident("1");
    assert_eq!(f[0], fmt("1000 1100"));
    assert_eq!(f[1], fmt("0000 0100")); // the space 0010 0000 reversed
    assert_eq!(f.len(), 20);
}
