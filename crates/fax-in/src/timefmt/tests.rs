use super::*;

#[test]
fn rfc3339_round_trip() {
    for t in [
        UNIX_EPOCH,
        UNIX_EPOCH + Duration::new(1_709_251_199, 123_000_000),
        UNIX_EPOCH + Duration::new(1_792_540_800, 999_000_000),
    ] {
        assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t));
    }
    assert_eq!(parse_rfc3339("not a time"), None);
    // before the epoch
    assert_eq!(
        parse_rfc3339("1969-12-31T23:59:59.000Z"),
        Some(UNIX_EPOCH - Duration::from_secs(1))
    );
}

#[test]
fn known_instants() {
    assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    // 2024-02-29T23:59:59.123Z (leap day) = 1709251199.123
    let t = UNIX_EPOCH + Duration::new(1_709_251_199, 123_000_000);
    assert_eq!(rfc3339(t), "2024-02-29T23:59:59.123Z");
    // 2026-10-20T00:00:00Z = 1792454400
    assert_eq!(
        rfc3339(UNIX_EPOCH + Duration::from_secs(1_792_454_400)),
        "2026-10-20T00:00:00.000Z"
    );
    // 2000-03-01T00:00:00Z = 951868800
    assert_eq!(
        rfc3339(UNIX_EPOCH + Duration::from_secs(951_868_800)),
        "2000-03-01T00:00:00.000Z"
    );
}
