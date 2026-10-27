use super::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn c01_cidr_v4_boundaries() {
    let c = Cidr::parse("192.168.10.0/24").unwrap();
    assert!(c.contains(ip("192.168.10.0")));
    assert!(c.contains(ip("192.168.10.255")));
    assert!(!c.contains(ip("192.168.9.255")));
    assert!(!c.contains(ip("192.168.11.0")));
    let odd = Cidr::parse("10.0.0.0/13").unwrap(); // 10.0.0.0 - 10.7.255.255
    assert!(odd.contains(ip("10.7.255.255")));
    assert!(!odd.contains(ip("10.8.0.0")));
    // host bits in the network address are ignored
    let host_bits = Cidr::parse("10.1.2.3/8").unwrap();
    assert!(host_bits.contains(ip("10.200.0.1")));
    assert!(!host_bits.contains(ip("11.0.0.1")));
    assert!(
        Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains(ip("203.0.113.9"))
    );
    let single = Cidr::parse("203.0.113.7").unwrap();
    assert_eq!(single.to_string(), "203.0.113.7/32");
    assert!(single.contains(ip("203.0.113.7")));
    assert!(!single.contains(ip("203.0.113.8")));
}

#[test]
fn c01_cidr_v6_and_mapped() {
    let c = Cidr::parse("2001:db8::/32").unwrap();
    assert!(c.contains(ip("2001:db8:ffff::1")));
    assert!(!c.contains(ip("2001:db9::1")));
    assert!(!c.contains(ip("192.0.2.1")), "families never mix");
    assert!(Cidr::parse("::1").unwrap().contains(ip("::1")));
    assert!(!Cidr::parse("::1").unwrap().contains(ip("::2")));
    // dual-stack socket reports ::ffff:10.0.0.5 for an IPv4 peer
    assert!(
        Cidr::parse("10.0.0.0/24")
            .unwrap()
            .contains(ip("::ffff:10.0.0.5"))
    );
    assert!(
        Cidr::parse("::ffff:10.0.0.0/120")
            .unwrap()
            .contains(ip("10.0.0.77"))
    );
}

#[test]
fn c01_cidr_rejects_garbage() {
    for bad in [
        "",
        "x",
        "1.2.3",
        "1.2.3.4/33",
        "1.2.3.4/",
        "1.2.3.4/-1",
        "1.2.3.4/a",
        "::1/129",
        "1.2.3.4/8/8",
        "300.1.1.1",
    ] {
        assert!(Cidr::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn c01_decide_per_mode() {
    let list = ["198.51.100.0/24", "203.0.113.7"];
    let drop = SourcePolicy::parse(&list, PolicyMode::Drop).unwrap();
    let rej = SourcePolicy::parse(&list, PolicyMode::Reject).unwrap();
    let log = SourcePolicy::parse(&list, PolicyMode::LogOnly).unwrap();
    for p in [&drop, &rej, &log] {
        assert_eq!(p.decide(ip("198.51.100.200")), Decision::Allow);
        assert_eq!(p.decide(ip("203.0.113.7")), Decision::Allow);
    }
    assert_eq!(drop.decide(ip("203.0.113.8")), Decision::Drop);
    assert_eq!(rej.decide(ip("203.0.113.8")), Decision::Reject403);
    assert_eq!(log.decide(ip("203.0.113.8")), Decision::AllowAndWarn);
    assert_eq!(drop.mode(), PolicyMode::Drop);
}

#[test]
fn c01_open_policy_and_empty_list() {
    assert_eq!(
        SourcePolicy::allow_all().decide(ip("1.2.3.4")),
        Decision::Allow
    );
    assert_eq!(SourcePolicy::default(), SourcePolicy::allow_all());
    let nobody = SourcePolicy::new(vec![], PolicyMode::Reject);
    assert_eq!(nobody.decide(ip("127.0.0.1")), Decision::Reject403);
    assert!(SourcePolicy::parse(&["bogus"], PolicyMode::Drop).is_err());
}
