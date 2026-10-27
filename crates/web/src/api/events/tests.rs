use super::*;
use axum::http::HeaderValue;

fn h(origin: Option<&str>, host: &str) -> HeaderMap {
    let mut m = HeaderMap::new();
    m.insert(header::HOST, HeaderValue::from_str(host).unwrap());
    if let Some(o) = origin {
        m.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
    }
    m
}

#[test]
fn the_origin_has_to_be_this_server() {
    assert!(same_origin(&h(None, "fax:8443")).is_ok());
    assert!(same_origin(&h(Some("https://fax:8443"), "fax:8443")).is_ok());
    assert!(same_origin(&h(Some("https://FAX:8443"), "fax:8443")).is_ok());
    assert!(same_origin(&h(Some("https://evil.example"), "fax:8443")).is_err());
    assert!(same_origin(&h(Some("https://fax:9999"), "fax:8443")).is_err());
    assert!(same_origin(&h(Some("null"), "fax:8443")).is_err());
}
