// Derived from Sipress (MIT, Copyright (c) 2025 LeoYu) core/src/sip/register.rs
// (`DigestChallenge`, `authorization_header`, helpers). REGISTER string templates removed;
// see docs/sipress-extraction.md for the list of changes.

//! Digest authentication for the UAC role (RFC 2617, RFC 3261 section 22.4).
//!
//! Parses a `WWW-Authenticate` / `Proxy-Authenticate` challenge from a 401/407 response and
//! computes the matching `Authorization` / `Proxy-Authorization` header. Only MD5 is supported.

use md5::{Digest, Md5};

/// A Digest challenge parsed from a 401 / 407 response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestChallenge {
    /// `realm` parameter.
    pub realm: String,
    /// `nonce` parameter.
    pub nonce: String,
    /// `algorithm` parameter (usually `MD5`, which is also the default).
    pub algorithm: String,
    /// `qop` parameter as offered by the server, e.g. `auth` or `auth,auth-int`.
    pub qop: Option<String>,
    /// `opaque` parameter, echoed back verbatim.
    pub opaque: Option<String>,
    /// `true` when the challenge came from `Proxy-Authenticate` (407); the answer must then be
    /// sent in `Proxy-Authorization`.
    pub proxy: bool,
}

impl DigestChallenge {
    /// Parses the challenge out of a raw SIP response (header block as text, lines separated by
    /// CRLF or LF). Returns `None` when there is no Digest challenge or it lacks realm/nonce.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut auth_value = String::new();
        let mut joining = false;
        let mut proxy = false;
        for line in raw.lines() {
            let lower = line.to_lowercase();
            if lower.starts_with("www-authenticate:") || lower.starts_with("proxy-authenticate:") {
                let val = line.split_once(':')?.1.trim();
                auth_value = val.to_string();
                joining = true;
                proxy = lower.starts_with("proxy-authenticate:");
            } else if joining && (line.starts_with(' ') || line.starts_with('\t')) {
                // folded continuation line
                auth_value.push(' ');
                auth_value.push_str(line.trim());
            } else if joining {
                break;
            }
        }
        Self::from_header_value(&auth_value, proxy)
    }

    /// Parses a challenge from the value of a `WWW-Authenticate` / `Proxy-Authenticate` header
    /// (for example `Digest realm="x", nonce="y"`). `proxy` selects the answer header name.
    pub fn from_header_value(value: &str, proxy: bool) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let scheme_end = value.find(|c: char| c.is_ascii_whitespace())?;
        let (scheme, after_digest) = value.split_at(scheme_end);
        if !scheme.eq_ignore_ascii_case("digest") {
            return None;
        }

        let mut realm = String::new();
        let mut nonce = String::new();
        let mut algorithm = "MD5".to_string();
        let mut qop = None;
        let mut opaque = None;

        for part in split_quoted_csv(after_digest) {
            let mut kv = part.splitn(2, '=');
            let k = kv.next().map(str::trim).unwrap_or("");
            let v_raw = kv.next().map(str::trim).unwrap_or("");
            let v = v_raw.trim_matches('"');
            match k.to_lowercase().as_str() {
                "realm" => realm = v.to_string(),
                "nonce" => nonce = v.to_string(),
                "algorithm" => algorithm = v.to_string(),
                "qop" => qop = Some(v.to_string()),
                "opaque" => opaque = Some(v.to_string()),
                _ => {}
            }
        }

        if realm.is_empty() || nonce.is_empty() {
            return None;
        }
        Some(Self {
            realm,
            nonce,
            algorithm,
            qop,
            opaque,
            proxy,
        })
    }

    /// Header name matching the challenge source (RFC 3261 section 22.3: 407 uses
    /// `Proxy-Authorization`).
    pub fn header_name(&self) -> &'static str {
        if self.proxy {
            "Proxy-Authorization"
        } else {
            "Authorization"
        }
    }

    /// Builds the complete header line (`"Authorization: Digest ..."` or
    /// `"Proxy-Authorization: Digest ..."`), without a trailing CRLF.
    pub fn authorization_header(
        &self,
        username: &str,
        password: &str,
        method: &str,
        uri: &str,
    ) -> String {
        format!(
            "{}: {}",
            self.header_name(),
            self.build_authorization(username, password, method, uri)
        )
    }

    /// Computes the response and builds the header *value*. A random client nonce is used when
    /// the server offered `qop`.
    pub fn build_authorization(
        &self,
        username: &str,
        password: &str,
        method: &str,
        uri: &str,
    ) -> String {
        let cnonce = format!("{:08x}", rand::random::<u32>());
        self.build_authorization_with_cnonce(username, password, method, uri, &cnonce)
    }

    /// Same as [`Self::build_authorization`] with a caller-chosen client nonce (deterministic,
    /// used for known-answer tests). `nc` is always `00000001`.
    pub fn build_authorization_with_cnonce(
        &self,
        username: &str,
        password: &str,
        method: &str,
        uri: &str,
        cnonce: &str,
    ) -> String {
        let ha1 = md5_hex(&format!("{}:{}:{}", username, self.realm, password));
        let ha2 = md5_hex(&format!("{}:{}", method, uri));

        let (response, qop_part, nc_part, cnonce_part) = if let Some(qop_full) = &self.qop {
            // first offered qop value ("auth, auth-int" -> "auth")
            let qop_val = qop_full.split(',').next().unwrap_or("auth").trim();
            let nc = "00000001";
            let resp = md5_hex(&format!(
                "{}:{}:{}:{}:{}:{}",
                ha1, self.nonce, nc, cnonce, qop_val, ha2
            ));
            (
                resp,
                format!(", qop={}", qop_val),
                format!(", nc={}", nc),
                format!(", cnonce=\"{}\"", cnonce),
            )
        } else {
            // RFC 2069 fallback: response = MD5(HA1:nonce:HA2)
            let resp = md5_hex(&format!("{}:{}:{}", ha1, self.nonce, ha2));
            (resp, String::new(), String::new(), String::new())
        };

        let opaque_part = self
            .opaque
            .as_ref()
            .map(|o| format!(", opaque=\"{}\"", o))
            .unwrap_or_default();

        format!(
            "Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", \
             uri=\"{uri}\", response=\"{resp}\", algorithm={algo}{qop}{nc}{cnonce}{op}",
            user = username,
            realm = self.realm,
            nonce = self.nonce,
            uri = uri,
            resp = response,
            algo = self.algorithm,
            qop = qop_part,
            nc = nc_part,
            cnonce = cnonce_part,
            op = opaque_part,
        )
    }
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    let out = h.finalize();
    let mut s = String::with_capacity(32);
    for b in out.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Splits `key=val, key="val,with,comma"`; commas inside quotes do not separate.
fn split_quoted_csv(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            ',' if !in_quote => {
                let t = cur.trim().to_string();
                if !t.is_empty() {
                    result.push(t);
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() {
        result.push(t);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- tests carried over from Sipress ----

    #[test]
    fn parse_digest_challenge() {
        let raw = "SIP/2.0 401 Unauthorized\r\n\
                   Via: SIP/2.0/UDP 1.2.3.4:5060\r\n\
                   WWW-Authenticate: Digest realm=\"sip.example.com\", \
                   nonce=\"abc123\", algorithm=MD5, qop=\"auth\"\r\n\
                   \r\n";
        let c = DigestChallenge::parse(raw).expect("should parse");
        assert_eq!(c.realm, "sip.example.com");
        assert_eq!(c.nonce, "abc123");
        assert_eq!(c.algorithm, "MD5");
        assert_eq!(c.qop.as_deref(), Some("auth"));
    }

    #[test]
    fn www_authenticate_uses_authorization_header() {
        let raw = "SIP/2.0 401 Unauthorized\r\n\
                   WWW-Authenticate: Digest realm=\"r\", nonce=\"n\"\r\n\r\n";
        let c = DigestChallenge::parse(raw).unwrap();
        assert!(!c.proxy);
        assert!(
            c.authorization_header("u", "p", "REGISTER", "sip:x")
                .starts_with("Authorization: Digest ")
        );
    }

    #[test]
    fn proxy_authenticate_uses_proxy_authorization_header() {
        let raw = "SIP/2.0 407 Proxy Authentication Required\r\n\
                   Proxy-Authenticate: Digest realm=\"r\", nonce=\"n\"\r\n\r\n";
        let c = DigestChallenge::parse(raw).unwrap();
        assert!(c.proxy);
        assert!(
            c.authorization_header("u", "p", "REGISTER", "sip:x")
                .starts_with("Proxy-Authorization: Digest ")
        );
    }

    #[test]
    fn build_auth_response() {
        let c = DigestChallenge {
            realm: "sip.example.com".into(),
            nonce: "abc123".into(),
            algorithm: "MD5".into(),
            qop: None,
            opaque: None,
            proxy: false,
        };
        let h = c.build_authorization("alice", "secret", "REGISTER", "sip:1.2.3.4:5060");
        assert!(h.contains("username=\"alice\""));
        assert!(h.contains("response=\""));
        // Independent check of the RFC 2069 response: MD5(HA1:nonce:HA2) computed by hand here.
        let ha1 = md5_hex("alice:sip.example.com:secret");
        let ha2 = md5_hex("REGISTER:sip:1.2.3.4:5060");
        let expected = md5_hex(&format!("{ha1}:abc123:{ha2}"));
        assert!(h.contains(&format!("response=\"{expected}\"")), "{h}");
    }

    // ---- additional tests ----

    /// RFC 2617 section 3.5 worked example (MD5, qop=auth) as an independent known answer.
    #[test]
    fn rfc2617_known_answer() {
        let c = DigestChallenge::from_header_value(
            "Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", \
             nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", \
             opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"",
            false,
        )
        .unwrap();
        let v = c.build_authorization_with_cnonce(
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            "0a4f113b",
        );
        assert!(
            v.contains("response=\"6629fae49393a05397450978507c4ef1\""),
            "{v}"
        );
        assert!(v.contains("qop=auth, nc=00000001, cnonce=\"0a4f113b\""));
        assert!(v.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
    }

    #[test]
    fn random_cnonce_is_used_and_varies() {
        let c = DigestChallenge::from_header_value(
            "Digest realm=\"r\", nonce=\"n\", qop=\"auth\"",
            false,
        )
        .unwrap();
        let a = c.build_authorization("u", "p", "INVITE", "sip:x");
        let b = c.build_authorization("u", "p", "INVITE", "sip:x");
        assert!(a.contains("cnonce=\""));
        assert_ne!(a, b, "two 32-bit random cnonces collided");
    }

    #[test]
    fn rejects_non_digest_and_incomplete_challenges() {
        assert!(DigestChallenge::from_header_value("Basic realm=\"x\"", false).is_none());
        assert!(DigestChallenge::from_header_value("Digest realm=\"x\"", false).is_none());
        assert!(DigestChallenge::from_header_value("Digest", false).is_none());
        assert!(DigestChallenge::from_header_value("", false).is_none());
        assert!(DigestChallenge::parse("SIP/2.0 401 Unauthorized\r\n\r\n").is_none());
    }

    #[test]
    fn folded_challenge_and_quoted_comma() {
        let raw = "SIP/2.0 401 Unauthorized\r\n\
                   WWW-Authenticate: Digest realm=\"a,b\",\r\n nonce=\"n1\"\r\n\
                   Content-Length: 0\r\n\r\n";
        let c = DigestChallenge::parse(raw).unwrap();
        assert_eq!(c.realm, "a,b");
        assert_eq!(c.nonce, "n1");
        assert_eq!(c.algorithm, "MD5", "default algorithm");
    }
}
