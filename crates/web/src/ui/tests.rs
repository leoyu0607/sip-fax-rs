use super::*;
use axum::body::{Body, to_bytes};

static BUILT: &Assets = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        b"<html>app</html>",
    ),
    ("favicon.svg", "image/svg+xml", b"<svg/>"),
    (
        "assets/index-abc.js",
        "text/javascript; charset=utf-8",
        b"1;",
    ),
];
static EMPTY: &Assets = &[];

async fn get(table: &'static Assets, method: Method, uri: &str) -> (StatusCode, Response) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let r = serve_from(table, req).await;
    (r.status(), r)
}

fn header<'a>(r: &'a Response, name: &str) -> &'a str {
    r.headers().get(name).unwrap().to_str().unwrap()
}

#[tokio::test]
async fn u05_the_not_built_page_is_served_at_the_root_when_there_is_no_dist() {
    let (s, r) = get(EMPTY, Method::GET, "/").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(header(&r, "content-type"), "text/html; charset=utf-8");
    assert_eq!(header(&r, "cache-control"), "no-cache");
    let body = to_bytes(r.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("npm ci"), "the page says how to build it");
    assert!(text.contains("/api/v1"), "and that the API works");
    // everything else is still the API's 404
    let (s, r) = get(EMPTY, Method::GET, "/assets/index-abc.js").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(header(&r, "content-type"), "application/json");
}

#[tokio::test]
async fn u05_files_have_their_type_and_only_hashed_assets_are_immutable() {
    let (s, r) = get(BUILT, Method::GET, "/").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(header(&r, "cache-control"), "no-cache");
    assert_eq!(header(&r, "content-type"), "text/html; charset=utf-8");
    let (s, r) = get(BUILT, Method::GET, "/assets/index-abc.js").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        header(&r, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(header(&r, "content-type"), "text/javascript; charset=utf-8");
    let (s, r) = get(BUILT, Method::GET, "/favicon.svg").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(header(&r, "cache-control"), "no-cache");
    assert_eq!(header(&r, "content-type"), "image/svg+xml");
    // a HEAD is answered like a GET
    let (s, _) = get(BUILT, Method::HEAD, "/").await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn u05_unknown_paths_other_methods_and_api_paths_are_the_api_404() {
    for (m, uri) in [
        (Method::GET, "/no-such-page"),
        (Method::GET, "/assets/none.js"),
        (Method::POST, "/"),
        (Method::PUT, "/index.html"),
        (Method::GET, "/api/v1/none"),
    ] {
        let (s, r) = get(BUILT, m.clone(), uri).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {uri}");
        assert_eq!(header(&r, "content-type"), "application/json", "{m} {uri}");
        let body = to_bytes(r.into_body(), 1 << 20).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "not_found", "{m} {uri}");
    }
}
