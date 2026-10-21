//! Small API surface tests: names, displays and frame helpers.

use t30::frame::{self, FrameError, build, fcf_name};
use t30::{DcsError, Frame, MinScanTime, ResultCode, T30Result, fcf, fif_bit};

#[test]
fn result_codes_have_stable_numbers_and_names() {
    let all = [
        (ResultCode::Ok, 0, "ok"),
        (ResultCode::NoResponseToDis, 1, "no_response_to_dis"),
        (ResultCode::T1Expired, 2, "t1_expired"),
        (ResultCode::T2ExpiredCommand, 3, "t2_expired_command"),
        (ResultCode::T2ExpiredImage, 4, "t2_expired_image"),
        (ResultCode::RemoteDisconnect, 5, "remote_disconnect"),
        (ResultCode::TrainingFailed, 6, "training_failed"),
        (ResultCode::PartialDcn, 7, "partial_dcn"),
        (ResultCode::IncompatibleDcs, 8, "incompatible_dcs"),
        (ResultCode::PageRejected, 9, "page_rejected"),
        (ResultCode::UnexpectedFrames, 10, "unexpected_frames"),
        (ResultCode::Aborted, 11, "aborted"),
        (ResultCode::DecoderError, 12, "decoder_error"),
    ];
    for (c, n, name) in all {
        assert_eq!((c.as_u8(), c.name(), c.is_ok()), (n, name, n == 0));
    }
}

#[test]
fn partial_means_failed_with_pages() {
    let mut r = T30Result {
        code: ResultCode::PartialDcn,
        pages: 2,
        bit_rate: 9600,
        modem: None,
        resolution: None,
        compression: None,
        remote_ident: None,
        bad_lines_per_page: vec![0, 1],
        duration_ms: 1,
    };
    assert!(r.is_partial());
    r.pages = 0;
    assert!(!r.is_partial());
    r.code = ResultCode::Ok;
    r.pages = 3;
    assert!(!r.is_partial());
}

#[test]
fn frame_parse_and_build() {
    let f = build(true, fcf::CFR, &[1, 2]);
    assert_eq!(f, [0xFF, 0xC8, 0x21, 1, 2]);
    let p = Frame::parse(&f).unwrap();
    assert!(p.final_frame && !p.x_bit());
    assert_eq!((p.fcf(), p.fif), (fcf::CFR, &[1u8, 2][..]));
    let p = Frame::parse(&[0xFF, 0xC0, 0xC1]).unwrap();
    assert!(!p.final_frame && p.x_bit());
    assert_eq!(p.fcf(), fcf::DCS);
    // the DTC family keeps its leading bit
    assert_eq!(Frame::parse(&[0xFF, 0xC8, 0x81]).unwrap().fcf(), fcf::DTC);
    assert_eq!(Frame::parse(&[]), Err(FrameError::TooShort));
    assert_eq!(Frame::parse(&[0xFF, 0xC8]), Err(FrameError::TooShort));
    assert_eq!(
        Frame::parse(&[0x00, 0xC8, 1]),
        Err(FrameError::BadAddress(0))
    );
    assert_eq!(
        Frame::parse(&[0xFF, 0x13, 1]),
        Err(FrameError::BadControl(0x13))
    );
    for e in [
        FrameError::TooShort,
        FrameError::BadAddress(1),
        FrameError::BadControl(2),
    ] {
        assert!(!e.to_string().is_empty());
    }
    for e in [DcsError::TooShort(1), DcsError::ReservedModem(0b1111)] {
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn fcf_names() {
    for (c, n) in [
        (fcf::DIS, "DIS"),
        (fcf::CSI, "CSI"),
        (fcf::NSF, "NSF"),
        (fcf::DTC, "DTC"),
        (fcf::CIG, "CIG"),
        (fcf::DCS, "DCS"),
        (fcf::TSI, "TSI"),
        (fcf::SUB, "SUB"),
        (fcf::NSS, "NSS"),
        (fcf::SID, "SID"),
        (fcf::CTC, "CTC"),
        (fcf::CFR, "CFR"),
        (fcf::FTT, "FTT"),
        (fcf::CTR, "CTR"),
        (fcf::EOM, "EOM"),
        (fcf::MPS, "MPS"),
        (fcf::EOR, "EOR"),
        (fcf::EOP, "EOP"),
        (fcf::RR, "RR"),
        (fcf::EOS, "EOS"),
        (fcf::PRI_EOM, "PRI-EOM"),
        (fcf::PRI_MPS, "PRI-MPS"),
        (fcf::PRI_EOP, "PRI-EOP"),
        (fcf::PPS, "PPS"),
        (fcf::MCF, "MCF"),
        (fcf::RTN, "RTN"),
        (fcf::RTP, "RTP"),
        (fcf::PIN, "PIN"),
        (fcf::PIP, "PIP"),
        (fcf::RNR, "RNR"),
        (fcf::ERR, "ERR"),
        (fcf::PPR, "PPR"),
        (fcf::FNV, "FNV"),
        (fcf::CRP, "CRP"),
        (fcf::DCN, "DCN"),
        (0x7E, "unknown"),
    ] {
        assert_eq!(fcf_name(c), n);
    }
}

#[test]
fn fif_bit_edges() {
    assert!(!fif_bit(&[0xFF], 0));
    assert!(fif_bit(&[0x80], 1));
    assert!(fif_bit(&[0x01], 8));
    assert!(!fif_bit(&[0xFF], 9), "beyond the field reads as 0");
    let mut v = Vec::new();
    frame::fif_bit(&v, 1);
    v.push(0);
    assert!(!fif_bit(&v, 1));
}

#[test]
fn min_scan_codes_round_trip() {
    for c in 0..8u8 {
        assert_eq!(MinScanTime::from_code(c).code(), c);
    }
    assert_eq!(MinScanTime::Other(0xFF).code(), 7);
}
