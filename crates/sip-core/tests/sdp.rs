//! D01 - D07: SDP model, T.38 attributes, offer/answer.

use proptest::prelude::*;
use sip_core::sdp::negotiate::{
    AnswerPolicy, MediaPreference, answer_audio, answer_offer, answer_t38,
};
use sip_core::sdp::t38::{RateManagement, T38Capabilities, T38Params, UdpEc};
use sip_core::sdp::*;

fn vector(name: &str) -> String {
    let p = format!(
        "{}/tests/vectors/sdp/{name}.sdp",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn parse(name: &str) -> SessionDescription {
    SessionDescription::parse(&vector(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

const VECTORS: &[&str] = &[
    "sonus_audio_offer",
    "audiocodes_audio_offer",
    "audiocodes_t38_reinvite",
    "sonus_t38_reinvite_mixed_case",
    "t38_minimal_offer",
    "audio_and_t38_offer",
    "audio_no_common_codec_offer",
];

// ---------------------------------------------------------------- D01

#[test]
fn d01_every_vector_round_trips_byte_for_byte() {
    for name in VECTORS {
        let text = vector(name);
        let sdp = SessionDescription::parse(&text).unwrap();
        assert_eq!(
            sdp.to_sdp_string(),
            text,
            "{name}: serialization differs from the input"
        );
        // and through the bytes entry point
        assert_eq!(parse_sdp(text.as_bytes()).unwrap(), sdp, "{name}");
        assert_eq!(sdp.to_bytes(), text.as_bytes(), "{name}");
    }
}

#[test]
fn d01_sonus_audio_offer_fields() {
    let s = parse("sonus_audio_offer");
    assert_eq!(s.version, 0);
    assert_eq!(s.origin.username, "Sonus_UAC");
    assert_eq!(
        (s.origin.sess_id, s.origin.sess_version),
        (1523094871, 1523094871)
    );
    assert_eq!(s.origin.address, "10.20.30.40");
    assert_eq!(s.session_name.as_deref(), Some("SIP Media Capabilities"));
    assert_eq!(s.connection.as_ref().unwrap().address, "10.20.30.41");
    assert_eq!(s.times, vec![Timing { start: 0, stop: 0 }]);
    assert_eq!(s.media.len(), 1);
    let m = &s.media[0];
    assert_eq!(
        (m.media.as_str(), m.port, m.proto.as_str()),
        ("audio", 16384, "RTP/AVP")
    );
    assert_eq!(m.formats, ["0", "8", "101"]);
    assert_eq!(
        m.rtpmaps()
            .iter()
            .map(|r| (r.payload_type, r.encoding.as_str(), r.clock_rate))
            .collect::<Vec<_>>(),
        [
            (0, "PCMU", 8000),
            (8, "PCMA", 8000),
            (101, "telephone-event", 8000)
        ]
    );
    assert_eq!(m.fmtp("101"), Some("0-15"));
    assert_eq!(m.attribute("ptime").unwrap().value.as_deref(), Some("20"));
    assert_eq!(m.direction(), Direction::SendRecv);
    assert!(!m.is_t38());
}

#[test]
fn d01_audiocodes_audio_offer_fields() {
    let s = parse("audiocodes_audio_offer");
    assert_eq!(s.origin.username, "AudiocodesGW");
    let m = &s.media[0];
    assert_eq!(
        (m.port, m.formats.as_slice()),
        (6000, ["8".to_string(), "0".into(), "101".into()].as_slice())
    );
    assert_eq!(s.effective_connection(m).unwrap().address, "192.168.10.5");
}

#[test]
fn d01_audiocodes_t38_reinvite_fields() {
    let s = parse("audiocodes_t38_reinvite");
    assert_eq!(s.origin.sess_version, 1128373, "re-INVITE bumps o= version");
    let m = &s.media[0];
    assert!(m.is_t38());
    assert_eq!(
        (
            m.media.as_str(),
            m.port,
            m.proto.as_str(),
            m.formats.as_slice()
        ),
        ("image", 6002, "udptl", ["t38".to_string()].as_slice())
    );
    let p = T38Params::from_media(m).unwrap();
    assert_eq!(
        p,
        T38Params {
            version: 0,
            max_bit_rate: 14400,
            rate_management: RateManagement::TransferredTcf,
            max_buffer: 1024,
            max_datagram: 122,
            udp_ec: UdpEc::Redundancy,
            fill_bit_removal: true,
            transcoding_mmr: true,
            transcoding_jbig: true,
            vendor_info: None,
        }
    );
}

#[test]
fn d01_sonus_t38_mixed_case_fields() {
    let s = parse("sonus_t38_reinvite_mixed_case");
    assert_eq!(s.media.len(), 2);
    assert_eq!(s.media[0].port, 0, "audio already disabled");
    let m = &s.media[1];
    assert!(m.is_t38());
    assert_eq!(
        T38Params::from_media(m).unwrap(),
        T38Params {
            version: 3,
            max_bit_rate: 33600,
            rate_management: RateManagement::LocalTcf,
            max_buffer: 1800,
            max_datagram: 316,
            udp_ec: UdpEc::Fec,
            fill_bit_removal: false,
            transcoding_mmr: true,
            transcoding_jbig: false,
            vendor_info: Some("0 0 0".into()),
        }
    );
    // original attribute spelling is preserved in the model
    assert_eq!(m.attributes[0].name, "T38faxVersion");
}

#[test]
fn d01_media_level_connection_and_extra_lines_are_preserved() {
    let s = parse("t38_minimal_offer");
    assert!(s.connection.is_none());
    let m = &s.media[0];
    assert_eq!(s.effective_connection(m).unwrap().address, "203.0.113.9");
    // unknown lines survive and come out at the RFC position
    let text = "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\ns=-\r\ni=info\r\nu=http://x\r\nc=IN IP4 1.2.3.4\r\nb=AS:64\r\nt=0 0\r\nk=prompt\r\na=x\r\nm=audio 1 RTP/AVP 0\r\ni=title\r\nc=IN IP4 5.6.7.8\r\nb=AS:8\r\na=sendonly\r\n";
    let p = SessionDescription::parse(text).unwrap();
    assert_eq!(p.extra.len(), 4);
    assert_eq!(p.media[0].extra.len(), 2);
    assert_eq!(p.to_sdp_string(), text);
    // out-of-order input is normalised to RFC order
    let messy = "v=0\r\ns=-\r\no=- 1 1 IN IP4 1.2.3.4\r\nt=0 0\r\nc=IN IP4 1.2.3.4\r\n";
    assert_eq!(
        SessionDescription::parse(messy).unwrap().to_sdp_string(),
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\ns=-\r\nc=IN IP4 1.2.3.4\r\nt=0 0\r\n"
    );
}

#[test]
fn d01_parse_errors_are_specific() {
    use SdpError::*;
    assert_eq!(SessionDescription::parse(""), Err(Empty));
    assert_eq!(SessionDescription::parse("\r\n\r\n"), Err(Empty));
    assert_eq!(
        SessionDescription::parse("o=- 1 1 IN IP4 1.2.3.4\r\n"),
        Err(MissingVersion)
    );
    assert_eq!(
        SessionDescription::parse("v=0\r\ns=-\r\n"),
        Err(MissingOrigin)
    );
    assert_eq!(
        SessionDescription::parse("v=0\r\nbogus\r\n"),
        Err(MalformedLine { line: 2 })
    );
    assert_eq!(
        SessionDescription::parse("v=0\r\nX=1\r\n"),
        Err(MalformedLine { line: 2 })
    );
    for bad in [
        "v=x\r\no=- 1 1 IN IP4 1.2.3.4\r\n",
        "v=0\r\no=- 1 1 IN IP4\r\n",
        "v=0\r\no=- a 1 IN IP4 1.2.3.4\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\no=- 1 1 IN IP4 1.2.3.4\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\nc=IN IP4\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\nt=1\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\nt=a b\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\nm=audio 70000 RTP/AVP 0\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\nm=audio 5004 RTP/AVP\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\nm=audio\r\n",
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\na=\r\n",
    ] {
        assert!(
            matches!(SessionDescription::parse(bad), Err(Invalid { .. })),
            "expected Invalid for {bad:?}, got {:?}",
            SessionDescription::parse(bad)
        );
    }
    let big = vec![b'a'; MAX_SDP_SIZE + 1];
    assert_eq!(parse_sdp(&big), Err(TooLarge));
    let many = format!(
        "v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\n{}",
        "m=audio 1 RTP/AVP 0\r\n".repeat(MAX_MEDIA + 1)
    );
    assert_eq!(SessionDescription::parse(&many), Err(TooManyMedia));
    // bare LF line endings are accepted
    assert!(SessionDescription::parse("v=0\no=- 1 1 IN IP4 1.2.3.4\ns=-\n").is_ok());
}

// ---------------------------------------------------------------- D02

fn image(attrs: &[&str]) -> MediaDescription {
    let mut text =
        String::from("v=0\r\no=- 1 1 IN IP4 1.2.3.4\r\ns=-\r\nt=0 0\r\nm=image 5004 udptl t38\r\n");
    for a in attrs {
        text.push_str(&format!("a={a}\r\n"));
    }
    SessionDescription::parse(&text).unwrap().media.remove(0)
}

#[test]
fn d02_missing_attributes_get_documented_defaults() {
    let p = T38Params::from_media(&image(&[])).unwrap();
    assert_eq!(p, T38Params::default());
    assert_eq!(p.version, 0, "spec: version 0 when absent");
    assert_eq!(p.max_bit_rate, 14400);
    assert_eq!(p.rate_management, RateManagement::TransferredTcf);
    assert_eq!(p.max_buffer, 1800);
    assert_eq!(p.max_datagram, 260);
    assert_eq!(
        p.udp_ec,
        UdpEc::Redundancy,
        "absent UdpEC = redundancy baseline"
    );
    assert!(!p.fill_bit_removal && !p.transcoding_mmr && !p.transcoding_jbig);
    assert_eq!(p.vendor_info, None);

    // minimal vector: only T38MaxBitRate present, everything else defaulted
    let m = parse("t38_minimal_offer");
    let p = T38Params::from_media(&m.media[0]).unwrap();
    assert_eq!(p.max_bit_rate, 9600);
    assert_eq!(
        T38Params {
            max_bit_rate: 14400,
            ..p
        },
        T38Params::default()
    );
}

#[test]
fn d02_attribute_name_case_variants() {
    let spellings = [
        [
            "T38FaxVersion:2",
            "T38MaxBitRate:9600",
            "T38FaxRateManagement:localTCF",
            "T38FaxUdpEC:t38UDPNoEC",
        ],
        [
            "t38faxversion:2",
            "t38maxbitrate:9600",
            "t38faxratemanagement:LOCALTCF",
            "t38faxudpec:T38UDPNOEC",
        ],
        [
            "T38faxVersion:2",
            "T38MAXBITRATE:9600",
            "T38FaxRatemanagement:localtcf",
            "T38FaxUDPEC:t38udpnoec",
        ],
    ];
    for set in spellings {
        let p = T38Params::from_media(&image(&set)).unwrap();
        assert_eq!(p.version, 2, "{set:?}");
        assert_eq!(p.max_bit_rate, 9600);
        assert_eq!(p.rate_management, RateManagement::LocalTcf);
        assert_eq!(p.udp_ec, UdpEc::NoEc);
    }
}

#[test]
fn d02_boolean_attribute_spellings() {
    for name in [
        "T38FaxFillBitRemoval",
        "T38FaxTranscodingMMR",
        "T38FaxTranscodingJBIG",
    ] {
        let get = |m: &MediaDescription| {
            let p = T38Params::from_media(m).unwrap();
            match name {
                "T38FaxFillBitRemoval" => p.fill_bit_removal,
                "T38FaxTranscodingMMR" => p.transcoding_mmr,
                _ => p.transcoding_jbig,
            }
        };
        assert!(get(&image(&[name])), "{name} bare = true");
        assert!(get(&image(&[&format!("{name}:1")])), "{name}:1 = true");
        assert!(!get(&image(&[&format!("{name}:0")])), "{name}:0 = false");
        assert!(!get(&image(&[])), "{name} absent = false");
        assert!(
            get(&image(&[&name.to_ascii_lowercase()])),
            "lower case bare"
        );
    }
    assert!(T38Params::from_media(&image(&["T38FaxFillBitRemoval:maybe"])).is_err());
}

#[test]
fn d02_invalid_values_are_errors() {
    for bad in [
        "T38FaxVersion:x",
        "T38FaxVersion",
        "T38MaxBitRate:-1",
        "T38FaxRateManagement:sometimes",
        "T38FaxMaxBuffer:abc",
        "T38FaxMaxDatagram:",
        "T38FaxUdpEC:t38UDPMagic",
        "T38FaxUdpEC",
    ] {
        assert!(
            matches!(
                T38Params::from_media(&image(&[bad])),
                Err(SdpError::Invalid { .. })
            ),
            "{bad}"
        );
    }
    // unknown T38-ish attributes are ignored
    assert_eq!(
        T38Params::from_media(&image(&["T38FaxMaxIFP:3"])).unwrap(),
        T38Params::default()
    );
}

#[test]
fn d02_to_attributes_emits_all_fields_and_reparses_equal() {
    let p = T38Params {
        version: 3,
        max_bit_rate: 33600,
        rate_management: RateManagement::LocalTcf,
        max_buffer: 2000,
        max_datagram: 300,
        udp_ec: UdpEc::Fec,
        fill_bit_removal: true,
        transcoding_mmr: true,
        transcoding_jbig: true,
        vendor_info: Some("1 2 3".into()),
    };
    let mut m = image(&[]);
    m.attributes = p.to_attributes();
    let names: Vec<_> = m.attributes.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "T38FaxVersion",
            "T38MaxBitRate",
            "T38FaxFillBitRemoval",
            "T38FaxTranscodingMMR",
            "T38FaxTranscodingJBIG",
            "T38FaxRateManagement",
            "T38FaxMaxBuffer",
            "T38FaxMaxDatagram",
            "T38FaxUdpEC",
            "T38VendorInfo"
        ]
    );
    assert_eq!(
        m.attribute("T38FaxUdpEC").unwrap().value.as_deref(),
        Some("t38UDPFEC")
    );
    assert_eq!(T38Params::from_media(&m).unwrap(), p);
    let q = T38Params {
        fill_bit_removal: false,
        vendor_info: None,
        ..p
    };
    assert_eq!(
        q.to_attributes().len(),
        8,
        "false booleans and absent vendor info are omitted"
    );
}

// ---------------------------------------------------------------- D03

fn offer_text(
    version: u8,
    rate: u32,
    rm: &str,
    ec: &str,
    fbr: &str,
    mmr: &str,
    jbig: &str,
) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 192.0.2.1\r\ns=-\r\nc=IN IP4 192.0.2.1\r\nt=0 0\r\nm=image 4000 udptl t38\r\n\
         a=T38FaxVersion:{version}\r\na=T38MaxBitRate:{rate}\r\na=T38FaxRateManagement:{rm}\r\n\
         a=T38FaxMaxBuffer:2222\r\na=T38FaxMaxDatagram:333\r\na=T38FaxUdpEC:{ec}\r\n{fbr}{mmr}{jbig}"
    )
}

#[test]
fn d03_t38_answer_matrix() {
    let rates = [2400u32, 4800, 7200, 9600, 12000, 14400, 33600];
    let rms = [
        ("localTCF", RateManagement::LocalTcf),
        ("transferredTCF", RateManagement::TransferredTcf),
    ];
    let ecs = [
        ("t38UDPFEC", UdpEc::Fec),
        ("t38UDPRedundancy", UdpEc::Redundancy),
        ("t38UDPNoEC", UdpEc::NoEc),
    ];
    let locals = [
        T38Capabilities {
            max_version: 0,
            max_bit_rate: 14400,
            fec: false,
            max_buffer: 1800,
            max_datagram: 260,
            ..Default::default()
        },
        T38Capabilities {
            max_version: 1,
            max_bit_rate: 9600,
            fec: true,
            max_buffer: 1000,
            max_datagram: 120,
            fill_bit_removal: true,
            transcoding_mmr: true,
            transcoding_jbig: false,
        },
        T38Capabilities {
            max_version: 3,
            max_bit_rate: 33600,
            fec: true,
            max_buffer: 4096,
            max_datagram: 480,
            fill_bit_removal: true,
            transcoding_mmr: true,
            transcoding_jbig: true,
        },
    ];
    let mut cells = 0;
    for (li, local) in locals.iter().enumerate() {
        for version in 0u8..=3 {
            for &rate in &rates {
                for (rm_s, rm) in rms {
                    for (ec_s, ec) in ecs {
                        let bools = [("", false), ("a=T38FaxFillBitRemoval\r\n", true)];
                        for (fbr_s, fbr) in bools {
                            let text = offer_text(
                                version,
                                rate,
                                rm_s,
                                ec_s,
                                fbr_s,
                                "a=T38FaxTranscodingMMR:1\r\n",
                                "a=T38FaxTranscodingJBIG:0\r\n",
                            );
                            let offer = SessionDescription::parse(&text).unwrap();
                            let ans = answer_t38(&offer.media[0], local, 5555).unwrap();
                            let p = T38Params::from_media(&ans).unwrap();
                            let ctx =
                                format!("local#{li} v{version} {rate} {rm_s} {ec_s} fbr={fbr}");
                            // spec section 6 rules, written out per field
                            assert_eq!(
                                p.version,
                                version.min(local.max_version),
                                "{ctx}: version = min"
                            );
                            assert_eq!(
                                p.max_bit_rate,
                                rate.min(local.max_bit_rate),
                                "{ctx}: bit rate = min"
                            );
                            assert_eq!(
                                p.rate_management, rm,
                                "{ctx}: rate management follows the offer"
                            );
                            let want_ec = if ec == UdpEc::Fec && !local.fec {
                                UdpEc::Redundancy
                            } else {
                                ec
                            };
                            assert_eq!(p.udp_ec, want_ec, "{ctx}: UdpEC");
                            assert_eq!(
                                p.max_datagram, local.max_datagram,
                                "{ctx}: datagram is ours"
                            );
                            assert_eq!(p.max_buffer, local.max_buffer, "{ctx}: buffer is ours");
                            assert_eq!(
                                p.fill_bit_removal,
                                fbr && local.fill_bit_removal,
                                "{ctx}: FBR intersection"
                            );
                            assert_eq!(
                                p.transcoding_mmr, local.transcoding_mmr,
                                "{ctx}: MMR (offered true)"
                            );
                            assert!(!p.transcoding_jbig, "{ctx}: JBIG offered :0");
                            // media line shape
                            assert_eq!(
                                (ans.media.as_str(), ans.port, ans.proto.as_str()),
                                ("image", 5555, "udptl")
                            );
                            assert_eq!(ans.formats, ["t38"]);
                            cells += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cells, 3 * 4 * 7 * 2 * 3 * 2);
}

#[test]
fn d03_golden_rows_with_literal_expectations() {
    // Independent of the formulas above: literal wire strings.
    let local = T38Capabilities {
        max_version: 1,
        max_bit_rate: 9600,
        fec: false,
        max_buffer: 1000,
        max_datagram: 120,
        fill_bit_removal: true,
        transcoding_mmr: true,
        transcoding_jbig: true,
    };
    let offer = SessionDescription::parse(&offer_text(
        3,
        33600,
        "localTCF",
        "t38UDPFEC",
        "a=T38FaxFillBitRemoval\r\n",
        "",
        "",
    ))
    .unwrap();
    let ans = answer_t38(&offer.media[0], &local, 6000).unwrap();
    let lines: Vec<String> = ans
        .attributes
        .iter()
        .map(|a| format!("{}:{}", a.name, a.value.clone().unwrap_or_default()))
        .collect();
    assert_eq!(
        lines,
        [
            "T38FaxVersion:1",
            "T38MaxBitRate:9600",
            "T38FaxFillBitRemoval:",
            "T38FaxRateManagement:localTCF",
            "T38FaxMaxBuffer:1000",
            "T38FaxMaxDatagram:120",
            "T38FaxUdpEC:t38UDPRedundancy",
        ]
    );
    // an offer that is lower than we are keeps the offer's numbers
    let offer = SessionDescription::parse(&offer_text(
        0,
        4800,
        "transferredTCF",
        "t38UDPNoEC",
        "",
        "",
        "",
    ))
    .unwrap();
    let p = T38Params::from_media(&answer_t38(&offer.media[0], &local, 6000).unwrap()).unwrap();
    assert_eq!(
        (p.version, p.max_bit_rate, p.udp_ec),
        (0, 4800, UdpEc::NoEc)
    );
    // invalid offer attribute -> error, not a silent default
    let bad =
        SessionDescription::parse(&offer_text(0, 4800, "bogus", "t38UDPNoEC", "", "", "")).unwrap();
    assert!(matches!(
        answer_t38(&bad.media[0], &local, 6000),
        Err(NegotiateError::InvalidT38(_))
    ));
}

#[test]
fn d03_full_answer_for_the_audiocodes_vector() {
    let offer = parse("audiocodes_t38_reinvite");
    let mut policy = AnswerPolicy::new("198.51.100.7", 20000, 20002);
    policy.t38.max_bit_rate = 9600;
    let mut origin = OriginTracker::new("-", 4242, 1, "198.51.100.7");
    let ans = answer_offer(&offer, &policy, &mut origin).unwrap();
    let text = ans.to_sdp_string();
    assert_eq!(
        text,
        "v=0\r\no=- 4242 1 IN IP4 198.51.100.7\r\ns=-\r\nc=IN IP4 198.51.100.7\r\nt=0 0\r\n\
         m=image 20002 udptl t38\r\n\
         a=T38FaxVersion:0\r\na=T38MaxBitRate:9600\r\na=T38FaxRateManagement:transferredTCF\r\n\
         a=T38FaxMaxBuffer:1800\r\na=T38FaxMaxDatagram:260\r\na=T38FaxUdpEC:t38UDPRedundancy\r\n"
    );
    assert_eq!(SessionDescription::parse(&text).unwrap(), ans);
}

// ---------------------------------------------------------------- D04

fn audio_media(offer_name: &str) -> MediaDescription {
    parse(offer_name).media.remove(0)
}

#[test]
fn d04_default_preference_pcmu_then_pcma_and_telephone_event() {
    let codecs = [AudioCodec::PCMU, AudioCodec::PCMA];
    // AudioCodes offer lists PCMA first; the answer follows OUR preference order
    let a = answer_audio(&audio_media("audiocodes_audio_offer"), &codecs, 30000).unwrap();
    assert_eq!(a.port, 30000);
    assert_eq!(a.formats, ["0", "8", "101"]);
    let maps: Vec<_> = a
        .attributes
        .iter()
        .map(|x| format!("{}={}", x.name, x.value.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(
        maps,
        [
            "rtpmap=0 PCMU/8000",
            "rtpmap=8 PCMA/8000",
            "rtpmap=101 telephone-event/8000",
            "fmtp=101 0-15"
        ]
    );
}

#[test]
fn d04_intersection_only_and_no_telephone_event_when_not_offered() {
    let offer = SessionDescription::parse(
        "v=0\r\no=- 1 1 IN IP4 1.1.1.1\r\ns=-\r\nc=IN IP4 1.1.1.1\r\nt=0 0\r\nm=audio 4000 RTP/AVP 8 18\r\na=rtpmap:18 G729/8000\r\n",
    )
    .unwrap();
    let a = answer_audio(&offer.media[0], &[AudioCodec::PCMU, AudioCodec::PCMA], 1234).unwrap();
    assert_eq!(
        a.formats,
        ["8"],
        "only PCMA is common; no telephone-event was offered"
    );
    assert_eq!(a.attributes.len(), 1);
    // a single-codec preference list
    let a = answer_audio(&audio_media("sonus_audio_offer"), &[AudioCodec::PCMA], 1234).unwrap();
    assert_eq!(a.formats, ["8", "101"]);
}

#[test]
fn d04_dynamic_payload_type_for_a_known_codec_is_matched_by_name() {
    let offer = SessionDescription::parse(
        "v=0\r\no=- 1 1 IN IP4 1.1.1.1\r\ns=-\r\nt=0 0\r\nm=audio 4000 RTP/AVP 96 97\r\na=rtpmap:96 pcmu/8000\r\na=rtpmap:97 telephone-event/8000\r\n",
    )
    .unwrap();
    let a = answer_audio(&offer.media[0], &[AudioCodec::PCMU], 1).unwrap();
    assert_eq!(
        a.formats,
        ["96", "97"],
        "the offerer's payload type numbers are echoed"
    );
    assert_eq!(
        a.attribute("rtpmap").unwrap().value.as_deref(),
        Some("96 PCMU/8000")
    );
}

#[test]
fn d04_no_common_codec() {
    let r = answer_audio(
        &audio_media("audio_no_common_codec_offer"),
        &[AudioCodec::PCMU, AudioCodec::PCMA],
        1,
    );
    assert_eq!(r, Err(NegotiateError::NoCommonCodec));
    // telephone-event alone is not a codec
    let te_only = SessionDescription::parse(
        "v=0\r\no=- 1 1 IN IP4 1.1.1.1\r\ns=-\r\nt=0 0\r\nm=audio 4000 RTP/AVP 101\r\na=rtpmap:101 telephone-event/8000\r\n",
    )
    .unwrap();
    assert_eq!(
        answer_audio(&te_only.media[0], &[AudioCodec::PCMU], 1),
        Err(NegotiateError::NoCommonCodec)
    );
    // via the full API as well
    let offer = parse("audio_no_common_codec_offer");
    let mut policy = AnswerPolicy::new("10.0.0.1", 2000, 2002);
    policy.preferred = MediaPreference::Audio;
    let mut o = OriginTracker::new("-", 1, 1, "10.0.0.1");
    assert_eq!(
        answer_offer(&offer, &policy, &mut o),
        Err(NegotiateError::NoCommonCodec)
    );
}

#[test]
fn d04_direction_is_mirrored_and_srtp_is_refused() {
    let mut m = audio_media("sonus_audio_offer");
    m.attributes.retain(|a| a.name != "sendrecv");
    for (offered, answered) in [
        ("sendonly", "recvonly"),
        ("recvonly", "sendonly"),
        ("inactive", "inactive"),
    ] {
        let mut mm = m.clone();
        mm.attributes.push(Attribute::flag(offered));
        assert_eq!(mm.direction().as_str(), offered);
        let a = answer_audio(&mm, &[AudioCodec::PCMU], 1).unwrap();
        assert_eq!(a.direction().as_str(), answered);
    }
    assert_eq!(
        answer_audio(&m, &[AudioCodec::PCMU], 1)
            .unwrap()
            .direction(),
        Direction::SendRecv
    );
    let mut srtp = m.clone();
    srtp.proto = "RTP/SAVP".into();
    assert_eq!(
        answer_audio(&srtp, &[AudioCodec::PCMU], 1),
        Err(NegotiateError::UnsupportedTransport("RTP/SAVP".into()))
    );
}

// ---------------------------------------------------------------- D05

#[test]
fn d05_audio_and_image_choose_one_and_reject_the_other_with_port_zero() {
    let offer = parse("audio_and_t38_offer");
    for (pref, accepted, rejected) in [
        (MediaPreference::T38, 1usize, 0usize),
        (MediaPreference::Audio, 0, 1),
    ] {
        let mut policy = AnswerPolicy::new("203.0.113.1", 40000, 40002);
        policy.preferred = pref;
        let mut o = OriginTracker::new("-", 1, 1, "203.0.113.1");
        let ans = answer_offer(&offer, &policy, &mut o).unwrap();
        assert_eq!(ans.media.len(), 2, "m-line count and order are preserved");
        assert_eq!(ans.media[0].media, "audio");
        assert_eq!(ans.media[1].media, "image");
        assert_ne!(ans.media[accepted].port, 0);
        let r = &ans.media[rejected];
        assert_eq!(r.port, 0, "{pref:?}: rejected stream has port 0");
        // everything else retained
        assert_eq!(r.media, offer.media[rejected].media);
        assert_eq!(r.proto, offer.media[rejected].proto);
        assert_eq!(r.formats, offer.media[rejected].formats);
        assert_eq!(r.attributes, offer.media[rejected].attributes);
        // on the wire
        let text = ans.to_sdp_string();
        let want = if rejected == 0 {
            "m=audio 0 RTP/AVP 0 8 101\r\n"
        } else {
            "m=image 0 udptl t38\r\n"
        };
        assert!(text.contains(want), "{text}");
    }
}

#[test]
fn d05_reject_media_preserves_all_fields_except_port() {
    let m = audio_media("sonus_audio_offer");
    let r = reject_media(&m);
    assert_eq!(r.port, 0);
    assert_eq!(MediaDescription { port: m.port, ..r }, m);
}

#[test]
fn d05_preferred_media_missing_from_offer() {
    let offer = parse("sonus_audio_offer");
    let mut policy = AnswerPolicy::new("10.0.0.1", 2000, 2002);
    policy.preferred = MediaPreference::T38;
    let mut o = OriginTracker::new("-", 1, 1, "10.0.0.1");
    assert_eq!(
        answer_offer(&offer, &policy, &mut o),
        Err(NegotiateError::NoAcceptableMedia)
    );
    // FirstOffered skips an un-answerable first section and accepts the next one
    let offer = parse("sonus_t38_reinvite_mixed_case"); // audio at port 0 still has PCMU, t38 second
    policy.preferred = MediaPreference::FirstOffered;
    let ans = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_ne!(ans.media[0].port, 0, "first offered (audio) accepted");
    assert_eq!(
        ans.media[1].port, 0,
        "t38 rejected because only one section is accepted"
    );
    // a video-only offer
    let video = SessionDescription::parse(
        "v=0\r\no=- 1 1 IN IP4 1.1.1.1\r\ns=-\r\nt=0 0\r\nm=video 4000 RTP/AVP 31\r\n",
    )
    .unwrap();
    assert_eq!(
        answer_offer(&video, &policy, &mut o),
        Err(NegotiateError::NoAcceptableMedia)
    );
}

// ---------------------------------------------------------------- D06

#[test]
fn d06_origin_version_increments_only_when_content_changes() {
    let offer = parse("audio_and_t38_offer");
    let mut policy = AnswerPolicy::new("203.0.113.1", 40000, 40002);
    let mut o = OriginTracker::new("-", 9000, 100, "203.0.113.1");

    policy.preferred = MediaPreference::Audio;
    let a1 = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_eq!(
        a1.origin.sess_version, 100,
        "first description uses the initial version"
    );
    assert_eq!(a1.origin.sess_id, 9000);

    let a2 = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_eq!(
        a2.origin.sess_version, 100,
        "identical content: version unchanged"
    );
    assert_eq!(a1, a2);

    policy.preferred = MediaPreference::T38; // content changes
    let a3 = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_eq!(a3.origin.sess_version, 101, "changed content: +1");

    let a4 = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_eq!(
        a4.origin.sess_version, 101,
        "same as the previous one again: unchanged"
    );

    policy.audio_port = 40010; // audio is rejected (port 0) in this answer, so nothing changes
    let a5 = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_eq!(
        a5.origin.sess_version, 101,
        "audio_port is irrelevant while audio is rejected"
    );

    policy.t38_port = 40012; // the accepted media changes
    let a6 = answer_offer(&offer, &policy, &mut o).unwrap();
    assert_eq!(a6.origin.sess_version, 102);
    assert_eq!(a6.media[1].port, 40012);
}

#[test]
fn d06_direct_stamp_semantics() {
    let base = parse("sonus_audio_offer");
    let mut o = OriginTracker::new("user", 7, 5, "2001:db8::1");
    let s1 = o.stamp(base.clone());
    assert_eq!(s1.origin.sess_version, 5);
    assert_eq!(s1.origin.address, "2001:db8::1");
    assert_eq!(s1.origin.addrtype, "IP6");
    assert_eq!(s1.origin.username, "user");
    // an `o=` line already present in the input never influences the decision
    let mut with_other_version = base.clone();
    with_other_version.origin.sess_version = 99999;
    assert_eq!(o.stamp(with_other_version).origin.sess_version, 5);
    let mut changed = base.clone();
    changed.media[0].port += 2;
    assert_eq!(o.stamp(changed.clone()).origin.sess_version, 6);
    assert_eq!(o.stamp(changed.clone()).origin.sess_version, 6);
    changed.attributes.push(Attribute::flag("x"));
    assert_eq!(o.stamp(changed).origin.sess_version, 7);
    // random constructor yields values below 2^31 and an IPv4 origin line
    let r = OriginTracker::random("10.1.1.1").current_origin();
    assert!(r.sess_id < (1 << 31) && r.sess_version < (1 << 31));
    assert_eq!(r.addrtype, "IP4");
}

// ---------------------------------------------------------------- D07

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn d07_arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = parse_sdp(&data);
    }

    #[test]
    fn d07_arbitrary_text_with_sdp_alphabet_never_panics(s in "[vomcatsib=0-9 a-zA-Z:/.\r\n-]{0,600}") {
        if let Ok(sd) = parse_sdp(s.as_bytes()) {
            // whatever parsed must serialize to canonical text that is a fixed point
            let text = sd.to_sdp_string();
            let again = SessionDescription::parse(&text);
            prop_assert!(again.is_ok());
            prop_assert_eq!(again.unwrap().to_sdp_string(), text);
        }
    }

    #[test]
    fn d07_mutated_vectors_never_panic(
        which in 0usize..VECTORS.len(),
        edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..12),
    ) {
        let mut bytes = vector(VECTORS[which]).into_bytes();
        for (idx, b) in edits {
            let i = idx.index(bytes.len());
            bytes[i] = b;
        }
        if let Ok(sd) = parse_sdp(&bytes) {
            // T.38 interpretation and answering must be total as well
            for m in &sd.media {
                let _ = T38Params::from_media(m);
                let _ = answer_audio(m, &[AudioCodec::PCMU, AudioCodec::PCMA], 1);
                let _ = answer_t38(m, &T38Capabilities::default(), 1);
            }
        }
    }
}

#[test]
fn d07_regression_fuzz_found_extra_line_order_is_normalised_not_lost() {
    // Found by cargo-fuzz (parse_sdp): media-level `k=`, `s=` and `t=` lines in this order. The
    // first serialisation moves `k=` before the unknown kinds, so the *model* differs after one
    // round trip, but no line is lost and the text is stable from then on.
    let input =
        "v=0\r\no=- 1 2 IN IP4 1.2.3.4\r\nm=image 5004 udptl t38\r\ns=late\r\nk=key\r\nt=0 0\r\n";
    let first = SessionDescription::parse(input).unwrap();
    assert_eq!(
        first.media[0]
            .extra
            .iter()
            .map(|e| e.kind)
            .collect::<Vec<_>>(),
        ['s', 'k', 't']
    );
    let text = first.to_sdp_string();
    assert_eq!(
        text,
        "v=0\r\no=- 1 2 IN IP4 1.2.3.4\r\nm=image 5004 udptl t38\r\nk=key\r\ns=late\r\nt=0 0\r\n"
    );
    let second = SessionDescription::parse(&text).unwrap();
    assert_eq!(second.to_sdp_string(), text);
    assert_eq!(second.media[0].extra.len(), 3, "nothing dropped");
}

fn token() -> impl Strategy<Value = String> {
    "[A-Za-z][A-Za-z0-9]{0,8}"
}

fn arb_attr() -> impl Strategy<Value = Attribute> {
    (token(), proptest::option::of("[ -~]{0,20}"))
        .prop_map(|(name, value)| Attribute { name, value })
}

fn arb_media() -> impl Strategy<Value = MediaDescription> {
    (
        token(),
        any::<u16>(),
        proptest::option::of(1u32..5),
        "[A-Za-z/]{1,8}",
        proptest::collection::vec("[a-z0-9]{1,4}", 1..4),
        proptest::collection::vec(arb_attr(), 0..5),
    )
        .prop_map(
            |(media, port, num_ports, proto, formats, attributes)| MediaDescription {
                media,
                port,
                num_ports,
                proto,
                formats,
                connection: None,
                attributes,
                extra: vec![],
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn d07_structured_model_round_trips(
        sess_id in any::<u64>(), ver in any::<u64>(),
        name in "[ -~]{0,20}",
        attrs in proptest::collection::vec(arb_attr(), 0..4),
        media in proptest::collection::vec(arb_media(), 0..4),
        c in proptest::option::of("[0-9.]{1,15}"),
    ) {
        let sd = SessionDescription {
            version: 0,
            origin: Origin { username: "-".into(), sess_id, sess_version: ver, nettype: "IN".into(), addrtype: "IP4".into(), address: "1.2.3.4".into() },
            session_name: Some(name),
            connection: c.map(|a| Connection { nettype: "IN".into(), addrtype: "IP4".into(), address: a }),
            times: vec![Timing { start: 0, stop: 0 }],
            attributes: attrs,
            media,
            extra: vec![],
        };
        let text = sd.to_sdp_string();
        prop_assert_eq!(SessionDescription::parse(&text), Ok(sd));
    }
}
