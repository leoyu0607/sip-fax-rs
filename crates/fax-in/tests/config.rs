//! Configuration model: presets, validation messages and the trunk table (C02, pure part).

use fax_in::config::{
    ConfigError, FaxInConfig, KeepMode, T38Policy, TransportName, UnknownSource, resolve_peer,
    split_host_port,
};
use fax_in::trunk::TrunkTable;
use sip_core::transport::{Decision, PolicyMode};

fn parse(t: &str) -> FaxInConfig {
    FaxInConfig::parse(t).unwrap_or_else(|e| panic!("{e}\n{t}"))
}

fn err(t: &str) -> String {
    match FaxInConfig::parse(t) {
        Ok(_) => panic!("expected an error for:\n{t}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn defaults_apply_to_an_empty_file() {
    let c = parse("");
    assert_eq!(c.sip.bind.to_string(), "0.0.0.0:5060");
    assert_eq!(c.sip.unknown_source, UnknownSource::Drop);
    assert_eq!(c.media.audio_port, 20000);
    assert_eq!(c.media.port_range, "30000-30999");
    assert_eq!(c.media.port_range().unwrap().capacity(), 500);
    assert!(c.capture.enabled);
    assert_eq!(c.capture.keep, KeepMode::All);
    assert_eq!(c.capture.dir.to_str(), Some("logs/calls"));
    assert_eq!(c.call.max_duration_secs, 3600);
    assert!(c.trunks.is_empty());
    assert_eq!(c.transports(), vec![TransportName::Udp]);
    assert!(c.warnings().iter().any(|w| w.contains("no [[trunks]]")));
}

#[test]
fn presets_generic_and_cht_cs210_are_equivalent_today_and_can_be_overridden() {
    let c = parse(
        "[[trunks]]\nname = \"g\"\npeer = \"198.51.100.1:5060\"\n\
         [[trunks]]\nname = \"cht\"\npreset = \"cht-cs210\"\npeer = \"203.0.113.10\"\n\
         [[trunks]]\nname = \"tuned\"\npreset = \"cht-cs210\"\npeer = \"203.0.113.11:5070\"\n\
         t38_policy = \"wait-peer\"\ninitiate_after_secs = 7.5\ntransport = \"tcp\"\n\
         [trunks.t38]\nmax_datagram = 480\n[trunks.compat]\naccept_unknown_require = true\n\
         [trunks.register]\nenabled = true\nusername = \"u\"\npassword = \"p\"\n",
    );
    let (g, cht, tuned) = (&c.trunks[0], &c.trunks[1], &c.trunks[2]);
    assert_eq!(g.preset, "generic");
    assert_eq!(cht.preset, "cht-cs210");
    for t in [g, cht] {
        assert_eq!(t.transport, TransportName::Udp);
        assert_eq!(t.t38_policy, T38Policy::Auto);
        assert_eq!(t.initiate_after_secs, 3.0);
        assert_eq!(t.t38.max_datagram, 260);
        assert!(!t.compat.accept_unknown_require);
        assert!(!t.register.enabled);
    }
    assert_eq!(tuned.transport, TransportName::Tcp);
    assert_eq!(tuned.t38_policy, T38Policy::WaitPeer);
    assert_eq!(tuned.initiate_after_secs, 7.5);
    assert_eq!(tuned.t38.max_datagram, 480);
    assert_eq!(
        tuned.t38.max_bit_rate, 14400,
        "untouched keys keep the default"
    );
    assert!(tuned.compat.accept_unknown_require);
    assert_eq!(tuned.register.expires, 3600);
    // transports are derived from the trunks when [sip] does not say
    assert_eq!(c.transports(), vec![TransportName::Udp, TransportName::Tcp]);
}

#[test]
fn mistakes_are_reported_with_the_key_and_the_trunk() {
    let e = err("[sip]\nbogus = 1\n");
    assert!(
        e.contains("unknown field `bogus`") && e.contains("sip"),
        "{e}"
    );
    let e = err("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\nt38_polcy = \"auto\"\n");
    assert!(e.contains("trunk \"a\"") && e.contains("t38_polcy"), "{e}");
    let e = err("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\nt38_policy = \"sometimes\"\n");
    assert!(
        e.contains("sometimes") && e.contains("auto"),
        "variants are listed: {e}"
    );
    let e = err("[[trunks]]\nname = \"a\"\npreset = \"nope\"\n");
    assert!(
        e.contains("unknown preset \"nope\"") && e.contains("cht-cs210"),
        "{e}"
    );
    let e = err("[[trunks]]\npeer = \"1.2.3.4\"\n");
    assert!(e.contains("non-empty `name`"), "{e}");
    let e = err("[[trunks]]\nname = \"a\"\n");
    assert!(e.contains("allowed_sources") && e.contains("peer"), "{e}");
    let e = err("[[trunks]]\nname = \"a\"\nallowed_sources = [\"10.0.0.0/33\"]\n");
    assert!(e.contains("10.0.0.0/33"), "{e}");
    let e = err(
        "[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[[trunks]]\nname = \"a\"\npeer = \"1.2.3.5\"\n",
    );
    assert!(e.contains("duplicate trunk name \"a\""), "{e}");
    let e = err("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\naudio_codecs = [\"G729\"]\n");
    assert!(e.contains("G729"), "{e}");
    let e = err("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\ninitiate_after_secs = -1\n");
    assert!(e.contains("initiate_after_secs"), "{e}");
    let e =
        err("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.register]\nenabled = true\n");
    assert!(e.contains("register.username"), "{e}");
    let e = err(
        "[[trunks]]\nname = \"a\"\nallowed_sources = [\"1.2.3.4\"]\n[trunks.register]\nenabled = true\nusername = \"u\"\npassword = \"p\"\n",
    );
    assert!(e.contains("registrar"), "{e}");
    let e = err(
        "[sip]\ntransports = [\"udp\"]\n[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\ntransport = \"tcp\"\n",
    );
    assert!(e.contains("does not open it"), "{e}");
    let e = err("[call]\nmax_duration_secs = 0\n");
    assert!(e.contains("max_duration_secs"), "{e}");
    let e = err("this is not toml");
    assert!(e.contains("invalid TOML"), "{e}");
    let e = err("trunks = 5\n");
    assert!(e.contains("array of tables"), "{e}");
    assert!(matches!(
        FaxInConfig::parse("[sip\n"),
        Err(ConfigError::Toml(_))
    ));
}

#[test]
fn addresses_and_peers() {
    assert_eq!(
        resolve_peer("203.0.113.9:5070").unwrap().to_string(),
        "203.0.113.9:5070"
    );
    assert_eq!(
        resolve_peer("203.0.113.9").unwrap().to_string(),
        "203.0.113.9:5060"
    );
    assert_eq!(
        resolve_peer("[2001:db8::1]:5080").unwrap().to_string(),
        "[2001:db8::1]:5080"
    );
    assert_eq!(resolve_peer("localhost:5061").unwrap().port(), 5061);
    assert!(resolve_peer("no-such-host.invalid:5060").is_err());
    assert_eq!(
        split_host_port("example.com:5070"),
        ("example.com", Some(5070))
    );
    assert_eq!(split_host_port("example.com"), ("example.com", None));
    assert_eq!(
        split_host_port("[2001:db8::1]:5070"),
        ("2001:db8::1", Some(5070))
    );
    assert_eq!(split_host_port("2001:db8::1"), ("2001:db8::1", None));
    assert_eq!(split_host_port("[::1]"), ("::1", None));
}

#[test]
fn media_address_falls_back_from_media_to_advertised_to_bind() {
    assert_eq!(
        parse("[sip]\nbind = \"192.0.2.1:5060\"\n").media_address(),
        "192.0.2.1"
    );
    assert_eq!(
        parse("[sip]\nbind = \"0.0.0.0:5060\"\nadvertised = \"198.51.100.5:5070\"\n")
            .media_address(),
        "198.51.100.5"
    );
    assert_eq!(
        parse("[sip]\nadvertised = \"198.51.100.5\"\n[media]\naddress = \"203.0.113.1\"\n")
            .media_address(),
        "203.0.113.1"
    );
    let w = parse("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n").warnings();
    assert!(w.iter().any(|w| w.contains("wildcard")), "{w:?}");
}

/// C02, the pure part: lookup rules of the trunk table.
#[test]
fn c02_trunk_lookup_first_match_wins_and_peer_is_the_default_source() {
    let c = parse(
        "[[trunks]]\nname = \"narrow\"\nallowed_sources = [\"10.1.2.0/24\"]\n\
         [[trunks]]\nname = \"wide\"\nallowed_sources = [\"10.0.0.0/8\", \"2001:db8::/32\"]\n\
         [[trunks]]\nname = \"by-peer\"\npeer = \"192.0.2.77:5060\"\n",
    );
    let t = TrunkTable::build(&c).unwrap();
    let name = |ip: &str| t.lookup(ip.parse().unwrap()).map(|t| t.name().to_string());
    assert_eq!(
        name("10.1.2.3").as_deref(),
        Some("narrow"),
        "first match in config order"
    );
    assert_eq!(name("10.1.3.3").as_deref(), Some("wide"));
    assert_eq!(name("2001:db8::5").as_deref(), Some("wide"));
    assert_eq!(name("192.0.2.77").as_deref(), Some("by-peer"));
    assert_eq!(name("192.0.2.78"), None);
    assert_eq!(
        name("::ffff:10.9.9.9").as_deref(),
        Some("wide"),
        "IPv4-mapped IPv6"
    );
    assert_eq!(t.fallback().name(), "unknown");
}

#[test]
fn the_transport_source_policy_is_the_union_of_all_trunks_with_the_configured_mode() {
    for (mode, pm) in [
        ("drop", PolicyMode::Drop),
        ("reject", PolicyMode::Reject),
        ("log", PolicyMode::LogOnly),
    ] {
        let c = parse(&format!(
            "[sip]\nunknown_source = \"{mode}\"\n\
             [[trunks]]\nname = \"a\"\nallowed_sources = [\"10.0.0.0/24\"]\n\
             [[trunks]]\nname = \"b\"\nallowed_sources = [\"172.16.0.1\"]\n"
        ));
        let p = TrunkTable::build(&c).unwrap().source_policy();
        assert_eq!(p.mode(), pm);
        assert_eq!(p.decide("10.0.0.200".parse().unwrap()), Decision::Allow);
        assert_eq!(p.decide("172.16.0.1".parse().unwrap()), Decision::Allow);
        let outside = p.decide("8.8.8.8".parse().unwrap());
        assert_eq!(
            outside,
            match pm {
                PolicyMode::Drop => Decision::Drop,
                PolicyMode::Reject => Decision::Reject403,
                PolicyMode::LogOnly => Decision::AllowAndWarn,
            }
        );
    }
}

#[test]
fn t38_policy_helpers_and_capability_mapping() {
    assert!(T38Policy::Auto.initiates() && T38Policy::Initiate.initiates());
    assert!(!T38Policy::WaitPeer.initiates() && !T38Policy::AudioOnly.initiates());
    assert!(!T38Policy::AudioOnly.accepts_t38() && T38Policy::WaitPeer.accepts_t38());
    let c = parse(
        "[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\naudio_codecs = [\"pcma\", \"PCMU\"]\n\
         [trunks.t38]\nudp_ec = \"fec\"\nmax_version = 2\nrate_management = \"localTCF\"\n",
    );
    let t = &c.trunks[0];
    assert_eq!(
        t.codecs().iter().map(|c| c.name).collect::<Vec<_>>(),
        vec!["PCMA", "PCMU"]
    );
    let caps = t.t38_capabilities();
    assert!(caps.fec, "udp_ec = fec implies FEC support");
    assert_eq!(caps.max_version, 2);
    let offer = t.t38_offer_params();
    assert_eq!(format!("{:?}", offer.udp_ec), "Fec");
    assert_eq!(format!("{:?}", offer.rate_management), "LocalTcf");
}

// ================================================================ port_range (M2a, spec section 4)

#[test]
fn port_range_parses_and_counts_even_ports() {
    use fax_in::config::PortRange;
    let r = PortRange::parse("30000-30999").unwrap();
    assert_eq!((r.low, r.high, r.capacity()), (30000, 30999, 500));
    assert_eq!(
        PortRange::parse(" 30001 - 30004 ")
            .unwrap()
            .ports()
            .collect::<Vec<_>>(),
        vec![30002, 30004]
    );
    assert_eq!(PortRange::parse("30000-30003").unwrap().capacity(), 2);
    let c = parse("[media]\nport_range = \"40000-40009\"\n");
    assert_eq!(c.media.port_range().unwrap().capacity(), 5);
}

#[test]
fn port_range_rejects_bad_values() {
    for (v, needle) in [
        ("30000", "low-high"),
        ("abc-def", "low-high"),
        ("30000-", "low-high"),
        ("-30000", "low-high"),
        ("30000-70000", "low-high"),
        ("31000-30000", "above the high port"),
        ("0-100", "port 0"),
        ("30001-30001", "no even port"),
    ] {
        let e = err(&format!("[media]\nport_range = \"{v}\"\n"));
        assert!(e.contains(needle), "port_range {v}: {e}");
    }
    assert!(err("[media]\nport_range = 30000\n").contains("port_range"));
}

#[test]
fn legacy_t38_port_is_refused_with_a_pointer_to_port_range() {
    let e = err("[media]\nt38_port = 30000\n");
    assert!(e.contains("t38_port") && e.contains("port_range"), "{e}");
    assert!(
        e.contains("30000-30999"),
        "the message suggests an equivalent range: {e}"
    );
    // even together with a valid port_range
    let e = err("[media]\nt38_port = 30000\nport_range = \"30000-30999\"\n");
    assert!(e.contains("no longer supported"), "{e}");
    // other unknown keys are still rejected by deny_unknown_fields
    assert!(err("[media]\nt38port = 1\n").contains("t38port"));
}

#[test]
fn m2d_storage_fax_session_station_id_and_udptl_options() {
    let c = parse("");
    assert_eq!(c.storage.data_dir, std::path::PathBuf::from("data"));
    assert_eq!(c.call.fax_session, fax_in::FaxSessionKind::T38);
    assert_eq!(c.call.station_id, "");
    let c = parse(
        "[storage]\ndata_dir = \"/srv/faxes\"\n[call]\nfax_session = \"hold\"\nstation_id = \"+886 2 1234\"\n\
         [[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.t38]\nredundancy_depth = 5\nfec_span = 2\nfec_entries = 1\n",
    );
    assert_eq!(c.storage.data_dir, std::path::PathBuf::from("/srv/faxes"));
    assert_eq!(c.call.fax_session, fax_in::FaxSessionKind::Hold);
    assert_eq!(c.call.station_id, "+886 2 1234");
    let t = &c.trunks[0].t38;
    assert_eq!((t.redundancy_depth, t.fec_span, t.fec_entries), (5, 2, 1));
    // defaults
    let c = parse("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n");
    let t = &c.trunks[0].t38;
    assert_eq!((t.redundancy_depth, t.fec_span, t.fec_entries), (3, 3, 3));
}

#[test]
fn m2d_bad_udptl_settings_and_unknown_storage_keys_are_rejected() {
    for bad in [
        "redundancy_depth = 9",
        "fec_span = 0",
        "fec_span = 9",
        "fec_entries = 0",
        "fec_entries = 9",
    ] {
        let e = err(&format!(
            "[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.t38]\n{bad}\n"
        ));
        assert!(e.contains("[trunks.t38]"), "{bad}: {e}");
    }
    assert!(err("[storage]\ndata = \"x\"\n").contains("data"));
    assert!(
        err("[call]\nfax_session = \"nope\"\n").contains("fax_session")
            || err("[call]\nfax_session = \"nope\"\n").contains("nope")
    );
}

#[test]
fn m3d_fax_section_defaults_and_values() {
    let c = parse("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n");
    let f = &c.trunks[0].fax;
    assert_eq!(f.resolutions, ["standard", "fine"]);
    // ECM is off by default until verified against a real gateway (ADR-035).
    assert!(!f.ecm && f.fine());
    let c = parse(
        "[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.fax]\nresolutions = [\"standard\"]\necm = false\n",
    );
    let f = &c.trunks[0].fax;
    assert!(!f.ecm && !f.fine());
    let c = parse(
        "[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.fax]\nresolutions = [\"fine\"]\n",
    );
    assert!(c.trunks[0].fax.fine() && !c.trunks[0].fax.ecm);
}

#[test]
fn m3d_fax_section_rejects_invalid_values() {
    for (bad, needle) in [
        ("resolutions = []", "empty"),
        ("resolutions = [\"superfine\"]", "superfine"),
        ("resolutions = [\"standard\", \"Fine\"]", "Fine"),
        ("resolutions = \"fine\"", "resolutions"),
        ("ecm = \"yes\"", "ecm"),
        ("mmr = true", "mmr"),
    ] {
        let e = err(&format!(
            "[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.fax]\n{bad}\n"
        ));
        assert!(e.contains(needle), "{bad}: {e}");
    }
    let e = err("[[trunks]]\nname = \"a\"\npeer = \"1.2.3.4\"\n[trunks.fax]\nresolutions = []\n");
    assert!(e.contains("[trunks.fax]") && e.contains("\"a\""), "{e}");
}
