use super::*;

#[test]
fn file_stems_are_safe_and_unique() {
    assert_eq!(file_stem("abc@10.0.0.1"), "abc@10.0.0.1");
    let a = file_stem("a/b");
    let b = file_stem("a_b");
    assert_ne!(a, b, "{a} {b}");
    assert!(!a.contains('/'));
    let long = "x".repeat(300);
    let s = file_stem(&long);
    assert!(s.len() < 120 && s.starts_with("xxx"));
    assert_ne!(s, file_stem(&format!("{long}y")));
    assert!(!file_stem("..").contains(".."));
}

#[test]
fn recovery_follows_the_negotiated_scheme() {
    let s = crate::session::UdptlSettings {
        redundancy_depth: 2,
        fec_span: 4,
        fec_entries: 1,
    };
    assert_eq!(
        recovery_for("t38UDPRedundancy", &s),
        ErrorRecovery::Redundancy { depth: 2 }
    );
    assert_eq!(
        recovery_for("t38UDPFEC", &s),
        ErrorRecovery::Fec {
            span: 4,
            entries: 1
        }
    );
    assert_eq!(recovery_for("t38UDPNoEC", &s), ErrorRecovery::None);
    assert_eq!(
        recovery_for("whatever", &s),
        ErrorRecovery::Redundancy { depth: 2 }
    );
}

#[test]
fn peer_address_parsing() {
    assert_eq!(
        parse_peer(Some("10.0.0.1:5004")),
        Some("10.0.0.1:5004".parse().unwrap())
    );
    assert_eq!(
        parse_peer(Some("[::1]:5004")),
        Some("[::1]:5004".parse().unwrap())
    );
    assert_eq!(parse_peer(Some("10.0.0.1:0")), None);
    assert_eq!(parse_peer(Some("host:5004")), None);
    assert_eq!(parse_peer(None), None);
}

#[test]
fn modem_set_follows_the_bit_rate() {
    assert!(modems_for(14_400).v17);
    assert!(!modems_for(4_800).v29);
    assert!(modems_for(4_800).v27ter);
}
