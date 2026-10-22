//! Secrets out of URLs before they are shown (logs, `config check`, the web interface).

/// The URL for display: the password of `user:password@host` is replaced by `***`.
pub fn mask_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, tail) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    match userinfo.split_once(':') {
        Some((user, _)) => format!("{scheme}://{user}:***@{host}{tail}"),
        None => url.to_string(),
    }
}

/// A webhook URL for display: the password of the user info and every query value (tokens) are
/// replaced by `***`.
pub fn mask_http_url(url: &str) -> String {
    let masked = mask_url(url);
    let (base, query) = match masked.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (masked.as_str(), None),
    };
    let base = base.split('#').next().unwrap_or(base);
    match query {
        Some(q) => {
            let q = q.split('#').next().unwrap_or(q);
            let keys: Vec<String> = q
                .split('&')
                .map(|kv| format!("{}=***", kv.split('=').next().unwrap_or("")))
                .collect();
            format!("{base}?{}", keys.join("&"))
        }
        None => base.to_string(),
    }
}
