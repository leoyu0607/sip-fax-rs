//! The web interface (spec 6): the files of `web-ui/dist`, embedded by `build.rs`. The pages
//! route with the URL fragment (`/#/faxes/12`), so the server only serves `/` and the files.

use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error;

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// Whether the interface was built into this binary.
pub fn embedded() -> bool {
    asset(ASSETS, "index.html").is_some()
}

type Assets = [(&'static str, &'static str, &'static [u8])];

fn asset(table: &'static Assets, path: &str) -> Option<(&'static str, &'static [u8])> {
    table
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, ct, b)| (*ct, *b))
}

/// The page shown when the binary was built without the interface.
const NOT_BUILT: &str = "<!doctype html>\n<html lang=\"zh-TW\"><head><meta charset=\"utf-8\"><title>sip-fax</title></head>\n<body><h1>sip-fax</h1>\n<p>Web 介面沒有建置進這個執行檔：請在 <code>web-ui/</code> 執行 <code>npm ci &amp;&amp; npm run build</code> 後重新建置。API（<code>/api/v1</code>）可以正常使用。</p>\n<p lang=\"en\">The web interface was not built into this binary: run <code>npm ci &amp;&amp; npm run build</code> in <code>web-ui/</code> and build again. The API (<code>/api/v1</code>) works.</p>\n</body></html>\n";

/// Everything that is not an API route: the interface's files, else the API's 404.
pub async fn serve(req: Request) -> Response {
    serve_from(ASSETS, req).await
}

async fn serve_from(table: &'static Assets, req: Request) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/") || !(req.method() == Method::GET || req.method() == Method::HEAD) {
        return error::not_found().await.into_response();
    }
    let rel = match path.trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    let Some((ct, body)) = asset(table, rel) else {
        if rel == "index.html" {
            return page(NOT_BUILT.as_bytes(), "text/html; charset=utf-8", "no-cache");
        }
        return error::not_found().await.into_response();
    };
    // Vite names the files under assets/ by their content: they never change
    let cache = if rel.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    page(body, ct, cache)
}

fn page(body: &'static [u8], ct: &'static str, cache: &'static str) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    r
}

#[cfg(test)]
mod tests {
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
}
