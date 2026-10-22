//! The web interface (spec 6): the files of `web-ui/dist`, embedded by `build.rs`. The pages
//! route with the URL fragment (`/#/faxes/12`), so the server only serves `/` and the files.

use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error;

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// Whether the interface was built into this binary.
pub fn embedded() -> bool {
    asset("index.html").is_some()
}

fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    ASSETS
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, ct, b)| (*ct, *b))
}

/// The page shown when the binary was built without the interface.
const NOT_BUILT: &str = "<!doctype html>\n<html lang=\"zh-TW\"><head><meta charset=\"utf-8\"><title>sip-fax</title></head>\n<body><h1>sip-fax</h1>\n<p>Web 介面沒有建置進這個執行檔：請在 <code>web-ui/</code> 執行 <code>npm ci &amp;&amp; npm run build</code> 後重新建置。API（<code>/api/v1</code>）可以正常使用。</p>\n<p lang=\"en\">The web interface was not built into this binary: run <code>npm ci &amp;&amp; npm run build</code> in <code>web-ui/</code> and build again. The API (<code>/api/v1</code>) works.</p>\n</body></html>\n";

/// Everything that is not an API route: the interface's files, else the API's 404.
pub async fn serve(req: Request) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/") || !(req.method() == Method::GET || req.method() == Method::HEAD) {
        return error::not_found().await.into_response();
    }
    let rel = match path.trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    let Some((ct, body)) = asset(rel) else {
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
