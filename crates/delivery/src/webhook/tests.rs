use super::*;
use crate::error::FailureKind;

#[test]
fn d04_classify_status_table() {
    for c in [200, 201, 204, 299] {
        assert!(classify_status(c).is_ok(), "{c}");
    }
    for c in [408, 425, 429, 500, 502, 503, 599] {
        assert_eq!(
            classify_status(c).unwrap_err().kind,
            FailureKind::Transient,
            "{c}"
        );
    }
    for c in [301, 302, 400, 401, 403, 404, 410, 422] {
        assert_eq!(
            classify_status(c).unwrap_err().kind,
            FailureKind::Permanent,
            "{c}"
        );
    }
}

#[test]
fn d04_signature_known_vector() {
    // RFC 4231 test case 2.
    assert_eq!(
        signature("Jefe", b"what do ya want for nothing?"),
        "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}
