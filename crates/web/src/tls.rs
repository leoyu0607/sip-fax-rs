//! TLS for `scheme = "https"` (spec `docs/design/m4-web.md` 5.2): the configured certificate, or a
//! self-signed one (ECDSA P-256, 825 days) made in `conf/tls/`, reused afterwards and replaced
//! when it gets close to its end (ADR-056: Apple refuses server certificates valid longer than 825
//! days, even trusted ones).
//! rustls with the ring provider, TLS 1.2 and 1.3 only.

use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::WebConfig;

/// A certificate this close to its end is warned about at every start.
pub const EXPIRY_WARNING_DAYS: i64 = 30;

/// Validity of a self-signed certificate: the longest that macOS and iOS accept.
pub const SELF_SIGNED_DAYS: i64 = 825;

/// Why HTTPS cannot start.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A file is missing or unreadable.
    #[error("{path}: {why}")]
    File {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        why: String,
    },
    /// The certificate and the key do not belong together, or rustls refuses them.
    #[error("the certificate {cert} and the key {key} cannot be used: {why}")]
    Unusable {
        /// Certificate file.
        cert: PathBuf,
        /// Key file.
        key: PathBuf,
        /// rustls' reason.
        why: String,
    },
    /// The self-signed certificate could not be made or stored.
    #[error("cannot make the self-signed certificate: {0}")]
    Generate(String),
}

/// What HTTPS serves with.
pub struct TlsMaterial {
    /// The rustls configuration (HTTP/2 and HTTP/1.1 offered by ALPN).
    pub config: Arc<ServerConfig>,
    /// SHA-256 of the leaf certificate, `AB:CD:...` like `openssl x509 -fingerprint -sha256`.
    pub fingerprint: String,
    /// End of validity of the leaf certificate.
    pub not_after: DateTime<Utc>,
    /// Subject alternative names of a certificate made now (empty when one was loaded).
    pub generated: Vec<String>,
    /// The fingerprint of the self-signed certificate that was replaced now because it was
    /// about to end (browsers have to accept the new one).
    pub replaced: Option<String>,
    /// The certificate file in use.
    pub cert_path: PathBuf,
}

impl std::fmt::Debug for TlsMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsMaterial")
            .field("fingerprint", &self.fingerprint)
            .field("not_after", &self.not_after)
            .field("generated", &self.generated)
            .field("replaced", &self.replaced)
            .field("cert_path", &self.cert_path)
            .finish_non_exhaustive()
    }
}

impl TlsMaterial {
    /// The warning for a certificate that ends within [`EXPIRY_WARNING_DAYS`] (or has ended).
    pub fn expiry_warning(&self, now: DateTime<Utc>) -> Option<String> {
        expiry_warning(&self.cert_path, self.not_after, now)
    }
}

/// The self-signed certificate and key in `<conf_dir>/tls/`.
pub fn self_signed_paths(conf_dir: &Path) -> (PathBuf, PathBuf) {
    let d = conf_dir.join("tls");
    (d.join("self-signed.crt"), d.join("self-signed.key"))
}

/// Loads the configured certificate, or the self-signed one: made now when it does not exist,
/// and made again when it ends within [`EXPIRY_WARNING_DAYS`] of `today` (a configured
/// certificate is never replaced, only warned about). `names` are the names of a new one
/// ([`subject_names`]); `today` dates it.
///
/// # Errors
/// A configured file is missing or unreadable, the pair does not match, or a new certificate
/// cannot be written.
pub fn load(
    cfg: &WebConfig,
    base: &Path,
    conf_dir: &Path,
    names: &[String],
    today: NaiveDate,
) -> Result<TlsMaterial, TlsError> {
    if let Some((c, k)) = cfg.cert_files(base) {
        return from_files(&c, &k, Vec::new());
    }
    let (c, k) = self_signed_paths(conf_dir);
    if c.is_file() && k.is_file() {
        // an unreadable pair of ours is made again as well
        let replaced = match from_files(&c, &k, Vec::new()) {
            Ok(m) if !ends_soon(m.not_after, today) => return Ok(m),
            Ok(m) => Some(m.fingerprint),
            Err(e) => {
                tracing::warn!(
                    "web: the self-signed certificate is not usable ({e}); a new one is made"
                );
                None
            }
        };
        write_self_signed(&c, &k, names, today)?;
        let mut m = from_files(&c, &k, names.to_vec())?;
        m.replaced = replaced;
        return Ok(m);
    }
    // a half-written pair (one file of the two) is replaced: these files are ours
    write_self_signed(&c, &k, names, today)?;
    from_files(&c, &k, names.to_vec())
}

/// Whether a certificate ending at `not_after` ends within [`EXPIRY_WARNING_DAYS`] of `today`.
fn ends_soon(not_after: DateTime<Utc>, today: NaiveDate) -> bool {
    not_after.date_naive() - today < Duration::days(EXPIRY_WARNING_DAYS)
}

fn from_files(cert: &Path, key: &Path, generated: Vec<String>) -> Result<TlsMaterial, TlsError> {
    let file_err = |p: &Path, why: String| TlsError::File {
        path: p.to_path_buf(),
        why,
    };
    if !cert.is_file() {
        return Err(file_err(cert, "the certificate file does not exist".into()));
    }
    if !key.is_file() {
        return Err(file_err(key, "the key file does not exist".into()));
    }
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| file_err(cert, e.to_string()))?
        .collect::<Result<_, _>>()
        .map_err(|e| file_err(cert, format!("not a PEM certificate: {e}")))?;
    let Some(leaf) = chain.first().cloned() else {
        return Err(file_err(cert, "no certificate in the file".into()));
    };
    let pkey = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| file_err(key, format!("not a PEM private key: {e}")))?;
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|e| file_err(cert, format!("cannot read the certificate: {e}")))?;
    let not_after = DateTime::from_timestamp(parsed.validity().not_after.timestamp(), 0)
        .ok_or_else(|| file_err(cert, "the end of validity is out of range".into()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| TlsError::Generate(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(chain, pkey)
        .map_err(|e| TlsError::Unusable {
            cert: cert.to_path_buf(),
            key: key.to_path_buf(),
            why: e.to_string(),
        })?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsMaterial {
        config: Arc::new(config),
        fingerprint: fingerprint(leaf.as_ref()),
        not_after,
        generated,
        replaced: None,
        cert_path: cert.to_path_buf(),
    })
}

/// `AB:CD:...`, the SHA-256 of `der` in the form `openssl x509 -fingerprint -sha256` prints.
pub fn fingerprint(der: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, der);
    d.as_ref()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The warning for a certificate that ends within [`EXPIRY_WARNING_DAYS`] of `now`, or has
/// ended.
pub fn expiry_warning(cert: &Path, not_after: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    if not_after <= now {
        Some(format!(
            "the web certificate {} expired on {}: browsers refuse it; replace it (for the self-signed one: delete both files and restart)",
            cert.display(),
            not_after.format("%Y-%m-%d")
        ))
    } else if not_after - now < Duration::days(EXPIRY_WARNING_DAYS) {
        Some(format!(
            "the web certificate {} expires on {} ({} day(s) left)",
            cert.display(),
            not_after.format("%Y-%m-%d"),
            (not_after - now).num_days()
        ))
    } else {
        None
    }
}

/// The names of a new self-signed certificate: the host name, `localhost`, `127.0.0.1`, the bind
/// address (every non-loopback address of the machine when it is unspecified) and `tls_names`.
/// Duplicates are dropped; the order is stable.
pub fn subject_names(
    hostname: Option<&str>,
    bind: IpAddr,
    local_ips: &[IpAddr],
    tls_names: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };
    if let Some(h) = hostname {
        push(h.trim().to_string());
    }
    push("localhost".into());
    push("127.0.0.1".into());
    if bind.is_unspecified() {
        for ip in local_ips.iter().filter(|ip| !ip.is_loopback()) {
            push(ip.to_string());
        }
    } else {
        push(bind.to_string());
    }
    for n in tls_names {
        push(n.trim().to_string());
    }
    out
}

/// The host name of this machine.
pub fn hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let u = rustix::system::uname();
        let n = u.nodename().to_string_lossy().trim().to_string();
        (!n.is_empty()).then_some(n)
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").ok().filter(|n| !n.is_empty())
    }
}

/// The addresses of the machine's interfaces (empty when they cannot be listed).
pub fn local_ips() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .map(|v| v.into_iter().map(|i| i.ip()).collect())
        .unwrap_or_default()
}

fn write_self_signed(
    cert: &Path,
    key: &Path,
    names: &[String],
    today: NaiveDate,
) -> Result<(), TlsError> {
    let g = |e: &dyn std::fmt::Display| TlsError::Generate(e.to_string());
    let mut params = rcgen::CertificateParams::new(names.to_vec()).map_err(|e| g(&e))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "sip-faxd");
    // what browsers check for a server certificate (macOS refuses one without them)
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    let end = today + Duration::days(SELF_SIGNED_DAYS);
    params.not_before = rcgen::date_time_ymd(today.year(), today.month() as u8, today.day() as u8);
    params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
    let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|e| g(&e))?;
    let c = params.self_signed(&pair).map_err(|e| g(&e))?;
    if let Some(d) = cert.parent() {
        fs::create_dir_all(d).map_err(|e| g(&e))?;
    }
    write_private(key, pair.serialize_pem().as_bytes()).map_err(|e| g(&e))?;
    fs::write(cert, c.pem()).map_err(|e| g(&e))?;
    Ok(())
}

/// Writes a private key readable by the owner only (0600 from the start on unix).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    // an existing file keeps its mode on open: make sure
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests;
