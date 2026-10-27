use super::HangupCode;

/// Adding a variant fails to compile here until `ALL` (and so the web interface's list of
/// texts, test U02) is thought about.
fn index(c: HangupCode) -> usize {
    match c {
        HangupCode::Completed => 0,
        HangupCode::PeerHangup => 1,
        HangupCode::PeerHangupNoFax => 2,
        HangupCode::T30Failed => 3,
        HangupCode::NoFaxSignal => 4,
        HangupCode::T38Rejected => 5,
        HangupCode::CallerCancelled => 6,
        HangupCode::Rejected => 7,
        HangupCode::AckTimeout => 8,
        HangupCode::SessionExpired => 9,
        HangupCode::MaxDuration => 10,
        HangupCode::Shutdown => 11,
        HangupCode::InternalError => 12,
    }
}

#[test]
fn all_lists_every_code_once_with_distinct_text() {
    let mut seen = std::collections::BTreeSet::new();
    for (i, c) in HangupCode::ALL.iter().enumerate() {
        assert_eq!(index(*c), i, "ALL is in declaration order");
        assert!(seen.insert(c.as_str()), "{} twice", c.as_str());
    }
    assert_eq!(seen.len(), 13);
}
