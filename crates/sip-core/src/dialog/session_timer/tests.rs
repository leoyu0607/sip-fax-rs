use super::*;

#[test]
fn parses_session_expires() {
    let se = parse_session_expires("1800 ; refresher=UAS").unwrap();
    assert_eq!(se.delta, 1800);
    assert_eq!(se.refresher, Some(Refresher::Uas));
    assert_eq!(parse_session_expires("90").unwrap().refresher, None);
    assert_eq!(
        parse_session_expires("90;refresher=bogus")
            .unwrap()
            .refresher,
        None
    );
    assert!(parse_session_expires("abc").is_none());
}

#[test]
fn schedule_arithmetic() {
    assert_eq!(refresh_after(90), Duration::from_secs(45));
    assert_eq!(expire_after(90), Duration::from_secs(60)); // 90 - min(32, 30)
    assert_eq!(expire_after(1800), Duration::from_secs(1768)); // 1800 - 32
}
