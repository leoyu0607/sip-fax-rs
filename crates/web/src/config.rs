//! `[web]` (spec `docs/design/m4-web.md` 5.1): a start-up setting, read from the file only
//! (ADR-051).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `http` or `https`.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    /// Plain HTTP (passwords travel in clear text unless the bind address is loopback).
    Http,
    /// HTTPS with rustls; a self-signed certificate is made when none is configured.
    #[default]
    Https,
}

/// `[web]`
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    /// Serve the web interface. Off when the file has no `[web]` table, so that an upgrade does
    /// not open a new port; the example configuration turns it on.
    pub enabled: bool,
    /// Listen address.
    pub bind: SocketAddr,
    /// `http` or `https`.
    pub scheme: Scheme,
    /// Certificate (PEM chain), relative to the executable directory. Empty: self-signed.
    pub cert: String,
    /// Private key (PEM), relative to the executable directory. Set together with `cert`.
    pub key: String,
    /// Extra subject alternative names (host names or IP addresses) of the self-signed
    /// certificate.
    pub tls_names: Vec<String>,
    /// A session ends after this many hours without a request.
    pub session_idle_hours: u32,
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig {
            enabled: false,
            bind: SocketAddr::from(([0, 0, 0, 0], 8443)),
            scheme: Scheme::Https,
            cert: String::new(),
            key: String::new(),
            tls_names: Vec::new(),
            session_idle_hours: 8,
        }
    }
}

impl WebConfig {
    /// Checks the values (the files are checked at start).
    ///
    /// # Errors
    /// One message naming the key.
    pub fn validate(&self) -> Result<(), String> {
        if self.cert.trim().is_empty() != self.key.trim().is_empty() {
            return Err("[web] cert and key are set together (or both left empty)".into());
        }
        if self.scheme == Scheme::Http && !self.cert.trim().is_empty() {
            return Err("[web] cert and key need scheme = \"https\"".into());
        }
        if self.session_idle_hours == 0 {
            return Err("[web] session_idle_hours must be at least 1".into());
        }
        for n in &self.tls_names {
            if n.trim().is_empty() || n.contains(char::is_whitespace) {
                return Err(format!(
                    "[web] tls_names: {n:?} is not a host name or address"
                ));
            }
        }
        Ok(())
    }

    /// The configured certificate and key against the executable directory (`None`: make or
    /// reuse the self-signed one).
    pub fn cert_files(&self, base: &Path) -> Option<(PathBuf, PathBuf)> {
        (!self.cert.trim().is_empty())
            .then(|| (resolve(base, &self.cert), resolve(base, &self.key)))
    }

    /// The warning for plain HTTP on an address other machines can reach.
    pub fn plaintext_warning(&self) -> Option<String> {
        (self.scheme == Scheme::Http && !self.bind.ip().is_loopback()).then(|| {
            format!(
                "[web] scheme = \"http\" on {}: passwords and faxes travel unencrypted; use \"https\" or bind to 127.0.0.1",
                self.bind
            )
        })
    }
}

fn resolve(base: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<WebConfig, String> {
        let c: WebConfig = toml::from_str(text).map_err(|e| e.to_string())?;
        c.validate()?;
        Ok(c)
    }

    #[test]
    fn h00_defaults_are_off_https_on_8443() {
        let c = WebConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.scheme, Scheme::Https);
        assert_eq!(c.bind.port(), 8443);
        assert_eq!(c.session_idle_hours, 8);
        assert_eq!(parse("").unwrap(), c);
    }

    #[test]
    fn h00_cert_and_key_go_together_and_need_https() {
        assert!(parse("cert = \"a.crt\"").unwrap_err().contains("together"));
        assert!(parse("key = \"a.key\"").unwrap_err().contains("together"));
        let e = parse("scheme = \"http\"\ncert = \"a\"\nkey = \"b\"").unwrap_err();
        assert!(e.contains("https"), "{e}");
        let c = parse("cert = \"conf/tls/web.crt\"\nkey = \"/etc/web.key\"").unwrap();
        let (crt, key) = c.cert_files(Path::new("/opt/sip-faxd")).unwrap();
        assert_eq!(crt, Path::new("/opt/sip-faxd/conf/tls/web.crt"));
        assert_eq!(key, Path::new("/etc/web.key"));
        assert!(WebConfig::default().cert_files(Path::new("/x")).is_none());
    }

    #[test]
    fn h00_bad_values_are_refused() {
        assert!(parse("session_idle_hours = 0").is_err());
        assert!(parse("tls_names = [\"a b\"]").is_err());
        assert!(parse("tls_names = [\"\"]").is_err());
        assert!(parse("bogus = 1").is_err());
        assert!(parse("scheme = \"ftp\"").is_err());
        assert!(parse("bind = \"not an address\"").is_err());
    }

    #[test]
    fn h06_plain_http_off_loopback_is_warned_about() {
        let mut c = parse("scheme = \"http\"\nbind = \"0.0.0.0:8080\"").unwrap();
        assert!(c.plaintext_warning().unwrap().contains("unencrypted"));
        c.bind = "127.0.0.1:8080".parse().unwrap();
        assert!(c.plaintext_warning().is_none());
        c.bind = "[::1]:8080".parse().unwrap();
        assert!(c.plaintext_warning().is_none());
        c.scheme = Scheme::Https;
        c.bind = "0.0.0.0:8443".parse().unwrap();
        assert!(c.plaintext_warning().is_none());
    }
}
