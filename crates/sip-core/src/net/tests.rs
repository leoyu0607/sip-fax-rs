use super::*;

#[test]
fn a_wildcard_host_becomes_the_address_towards_the_peer() {
    let peer: SocketAddr = "127.0.0.1:5060".parse().unwrap();
    assert_eq!(concrete_host("0.0.0.0", peer), "127.0.0.1");
    assert_eq!(local_ip_towards(peer), Some("127.0.0.1".parse().unwrap()));
    // a configured address or a name is never touched
    assert_eq!(concrete_host("192.0.2.7", peer), "192.0.2.7");
    assert_eq!(concrete_host("fax.example", peer), "fax.example");
    let v6: SocketAddr = "[::1]:5060".parse().unwrap();
    if local_ip_towards(v6).is_some() {
        assert_eq!(concrete_host("::", v6), "::1");
    }
}

#[test]
fn host_handles_ipv4_ipv6_and_names() {
    assert_eq!(host("10.0.0.1:5060"), "10.0.0.1");
    assert_eq!(host("10.0.0.1"), "10.0.0.1");
    assert_eq!(host("[2001:db8::1]:5060"), "2001:db8::1");
    assert_eq!(host("[2001:db8::1]"), "2001:db8::1");
    assert_eq!(
        host("2001:db8::1"),
        "2001:db8::1",
        "沒有中括號的 IPv6 不可被截斷"
    );
    assert_eq!(host("::1"), "::1");
    assert_eq!(host("pbx.example:5060"), "pbx.example");
    assert_eq!(host("pbx.example"), "pbx.example");
}

#[test]
fn uri_host_brackets_ipv6_only() {
    assert_eq!(uri_host("[2001:db8::1]:5070"), "[2001:db8::1]");
    assert_eq!(uri_host("2001:db8::1"), "[2001:db8::1]");
    assert_eq!(uri_host("10.0.0.1:5070"), "10.0.0.1");
    assert_eq!(uri_host("pbx.example"), "pbx.example");
}

#[test]
fn join_brackets_ipv6() {
    assert_eq!(join("10.0.0.1", 16000), "10.0.0.1:16000");
    assert_eq!(join("2001:db8::1", 16000), "[2001:db8::1]:16000");
    assert_eq!(join("[2001:db8::1]", 16000), "[2001:db8::1]:16000");
    assert!(join("::1", 5060).parse::<SocketAddr>().is_ok());
}

#[test]
fn detects_ipv6_and_picks_bind_family() {
    assert!(is_ipv6("::1") && is_ipv6("[2001:db8::1]"));
    assert!(!is_ipv6("10.0.0.1") && !is_ipv6("pbx.example"));
    assert_eq!(unspecified_for(&"[::1]:5060".parse().unwrap()), "[::]:0");
    assert_eq!(
        unspecified_for(&"10.0.0.1:5060".parse().unwrap()),
        "0.0.0.0:0"
    );
}
