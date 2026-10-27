//! `[[routes]]` / `[[routes.deliveries]]` configuration (spec section 9 and 11).
//!
//! ```toml
//! [[routes]]
//! dnis_prefix = "2557"
//! deliver_partial = true            # default false
//! [[routes.deliveries]]
//! kind = "folder"
//! dir = "/srv/fax-inbox"
//! filename = "{date}_{ani}_{id}"    # extension is added per format
//! formats = ["pdf", "tiff"]         # default ["pdf", "tiff"]
//! [[routes.deliveries]]
//! kind = "email"
//! host = "smtp.example.com"
//! security = "starttls"             # starttls (default) | smtps | none
//! username = "fax"
//! password = "secret"
//! from = "Fax <fax@example.com>"
//! to = ["ops@example.com"]
//! [[routes.deliveries]]
//! kind = "webhook"
//! url = "https://example.com/hook"
//! secret = "hmac-key"               # optional: adds X-Sipfax-Signature: sha256=<hex>
//! attach_pdf = true                 # optional: multipart/form-data with the PDF
//!
//! [[routes]]
//! default = true                    # must be the last route
//! [[routes.deliveries]]
//! kind = "folder"
//! dir = "/srv/fax-unrouted"
//! ```

use crate::template::Template;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use store::{FaxRecord, FaxStatus};

fn yes() -> bool {
    true
}

/// All routes of the configuration file (`[[routes]]`). Use `#[serde(flatten)]` or a plain
/// `Vec<Route>` field in the daemon configuration; both work.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Routes {
    /// Routes in match order.
    #[serde(default)]
    pub routes: Vec<Route>,
}

/// One route: match conditions (ANDed) and what to do with a matching fax.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Optional label for logs.
    #[serde(default)]
    pub name: Option<String>,
    /// Exact called number.
    #[serde(default)]
    pub dnis: Option<String>,
    /// Called number prefix.
    #[serde(default)]
    pub dnis_prefix: Option<String>,
    /// Trunk name.
    #[serde(default)]
    pub trunk: Option<String>,
    /// Matches every fax (must be the last route).
    #[serde(default)]
    pub default: bool,
    /// Also deliver `partial` faxes (default false).
    #[serde(default)]
    pub deliver_partial: bool,
    /// What to do with matching faxes.
    #[serde(default)]
    pub deliveries: Vec<DeliveryConfig>,
}

impl Route {
    /// Whether `rec` satisfies every condition of this route.
    pub fn matches(&self, rec: &FaxRecord) -> bool {
        if self.default {
            return true;
        }
        let mut any = false;
        if let Some(d) = &self.dnis {
            any = true;
            if rec.dnis.as_deref() != Some(d.as_str()) {
                return false;
            }
        }
        if let Some(p) = &self.dnis_prefix {
            any = true;
            if !rec
                .dnis
                .as_deref()
                .is_some_and(|d| d.starts_with(p.as_str()))
            {
                return false;
            }
        }
        if let Some(t) = &self.trunk {
            any = true;
            if rec.trunk != *t {
                return false;
            }
        }
        any
    }

    /// Whether a fax with this status is delivered by this route.
    pub fn wants(&self, status: FaxStatus) -> bool {
        match status {
            FaxStatus::Received => true,
            FaxStatus::Partial => self.deliver_partial,
            _ => false,
        }
    }

    fn label(&self, idx: usize) -> String {
        match &self.name {
            Some(n) => format!("routes[{idx}] ({n})"),
            None => format!("routes[{idx}]"),
        }
    }
}

/// One delivery target of a route.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DeliveryConfig {
    /// Copy files into a directory.
    Folder(FolderConfig),
    /// Send an e-mail with the PDF attached.
    Email(EmailConfig),
    /// POST the record as JSON.
    Webhook(WebhookConfig),
}

/// File formats a folder delivery can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// `.pdf`
    Pdf,
    /// `.tif`
    Tiff,
}

impl Format {
    /// File extension written.
    pub fn ext(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Tiff => "tif",
        }
    }
}

fn default_formats() -> Vec<Format> {
    vec![Format::Pdf, Format::Tiff]
}

/// `kind = "folder"`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderConfig {
    /// Destination directory.
    pub dir: String,
    /// File name template without directory. A trailing `.pdf`/`.tif`/`.tiff` is ignored; the
    /// extension of each format is appended.
    #[serde(default = "default_filename")]
    pub filename: String,
    /// Which files to copy.
    #[serde(default = "default_formats")]
    pub formats: Vec<Format>,
    /// Create `dir` (and parents) when it does not exist (default true). When false, a missing
    /// directory is a transient failure and the attempt is retried (for example an unmounted share).
    #[serde(default = "yes")]
    pub create_dir: bool,
}

fn default_filename() -> String {
    "{date}_{time}_{ani}_{id}".into()
}

/// Transport security of an e-mail delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// Plain connection upgraded with STARTTLS (required; default, port 587).
    #[default]
    Starttls,
    /// Implicit TLS (SMTPS, port 465).
    Smtps,
    /// No encryption (testing only).
    None,
}

fn default_timeout() -> u64 {
    30
}

fn default_subject() -> String {
    "Fax from {ani} ({pages} pages)".into()
}

fn default_body() -> String {
    "Fax received.\n\nFrom: {ani}\nTo: {dnis}\nTrunk: {trunk}\nTime: {started_at}\nPages: {pages}\nStatus: {status}\nID: {id}\n"
        .into()
}

/// `kind = "email"`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmailConfig {
    /// SMTP server host name.
    pub host: String,
    /// SMTP port (default: 587 for starttls, 465 for smtps, 25 for none).
    #[serde(default)]
    pub port: Option<u16>,
    /// Transport security.
    #[serde(default)]
    pub security: Security,
    /// Accept any server certificate (self-signed test servers). Default false.
    #[serde(default)]
    pub tls_insecure: bool,
    /// AUTH user name (AUTH PLAIN/LOGIN is used when set).
    #[serde(default)]
    pub username: Option<String>,
    /// AUTH password.
    #[serde(default)]
    pub password: Option<String>,
    /// Sender (`addr` or `Name <addr>`).
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Subject template.
    #[serde(default = "default_subject")]
    pub subject: String,
    /// Body template (plain text).
    #[serde(default = "default_body")]
    pub body: String,
    /// Attachment file name template (default `fax-{id}`; `.pdf` is appended).
    #[serde(default)]
    pub attachment_name: Option<String>,
    /// Also attach the TIFF.
    #[serde(default)]
    pub attach_tiff: bool,
    /// Per-connection timeout in seconds (default 30).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl EmailConfig {
    /// Effective port.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.security {
            Security::Starttls => 587,
            Security::Smtps => 465,
            Security::None => 25,
        })
    }
}

fn default_sig_header() -> String {
    "X-Sipfax-Signature".into()
}

/// `kind = "webhook"`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    /// Endpoint (`http://` or `https://`).
    pub url: String,
    /// HMAC-SHA256 key. When set, the request carries `<signature_header>: sha256=<hex>` computed
    /// over the exact request body.
    #[serde(default)]
    pub secret: Option<String>,
    /// Name of the signature header.
    #[serde(default = "default_sig_header")]
    pub signature_header: String,
    /// Send `multipart/form-data` (part `record` = JSON, part `pdf` = file) instead of plain JSON.
    #[serde(default)]
    pub attach_pdf: bool,
    /// Extra request headers.
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Request timeout in seconds (default 30).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl DeliveryConfig {
    /// Kind label stored in `delivery_attempts.kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            DeliveryConfig::Folder(_) => "folder",
            DeliveryConfig::Email(_) => "email",
            DeliveryConfig::Webhook(_) => "webhook",
        }
    }

    /// Stable identifier stored in `delivery_attempts.target`; also how a pending attempt finds
    /// its configuration again after a restart. Never contains credentials.
    pub fn target(&self) -> String {
        match self {
            DeliveryConfig::Folder(f) => format!("folder:{}", f.dir),
            DeliveryConfig::Email(e) => format!("email:{}", e.to.join(",")),
            DeliveryConfig::Webhook(w) => format!("webhook:{}", redact_url(&w.url)),
        }
    }

    fn validate(&self, who: &str, errs: &mut Vec<String>) {
        match self {
            DeliveryConfig::Folder(f) => {
                if f.dir.trim().is_empty() {
                    errs.push(format!("{who}: folder dir is empty"));
                }
                if f.formats.is_empty() {
                    errs.push(format!("{who}: folder formats is empty"));
                }
                let mut seen = HashSet::new();
                if !f.formats.iter().all(|x| seen.insert(*x)) {
                    errs.push(format!("{who}: folder formats has duplicates"));
                }
                if let Err(e) = Template::parse(&f.filename) {
                    errs.push(format!("{who}: filename: {e}"));
                }
            }
            DeliveryConfig::Email(e) => {
                if e.host.trim().is_empty() {
                    errs.push(format!("{who}: email host is empty"));
                }
                if e.to.is_empty() {
                    errs.push(format!("{who}: email to is empty"));
                }
                if e.from.parse::<lettre::message::Mailbox>().is_err() {
                    errs.push(format!("{who}: invalid from address {:?}", e.from));
                }
                for a in &e.to {
                    if a.parse::<lettre::message::Mailbox>().is_err() {
                        errs.push(format!("{who}: invalid recipient {a:?}"));
                    }
                }
                if e.username.is_some() != e.password.is_some() {
                    errs.push(format!("{who}: email username and password go together"));
                }
                if e.security == Security::None && e.username.is_some() {
                    // lettre refuses to send credentials over a plain connection only when asked to;
                    // the operator must opt in by using a TLS mode.
                    errs.push(format!(
                        "{who}: AUTH over an unencrypted connection (security = \"none\") is not allowed"
                    ));
                }
                for (n, t) in [
                    ("subject", &e.subject),
                    ("body", &e.body),
                    (
                        "attachment_name",
                        e.attachment_name.as_ref().unwrap_or(&String::new()),
                    ),
                ] {
                    if let Err(er) = Template::parse(t) {
                        errs.push(format!("{who}: {n}: {er}"));
                    }
                }
                if e.timeout_secs == 0 {
                    errs.push(format!("{who}: timeout_secs must be > 0"));
                }
            }
            DeliveryConfig::Webhook(w) => {
                match reqwest::Url::parse(&w.url) {
                    Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => {}
                    _ => errs.push(format!(
                        "{who}: webhook url must be http(s)://host/...: {:?}",
                        w.url
                    )),
                }
                if w.secret.as_deref() == Some("") {
                    errs.push(format!("{who}: webhook secret is empty"));
                }
                if reqwest::header::HeaderName::from_bytes(w.signature_header.as_bytes()).is_err() {
                    errs.push(format!(
                        "{who}: invalid signature_header {:?}",
                        w.signature_header
                    ));
                }
                for (k, v) in &w.headers {
                    if reqwest::header::HeaderName::from_bytes(k.as_bytes()).is_err()
                        || reqwest::header::HeaderValue::from_str(v).is_err()
                    {
                        errs.push(format!("{who}: invalid header {k:?}"));
                    }
                }
                if w.timeout_secs == 0 {
                    errs.push(format!("{who}: timeout_secs must be > 0"));
                }
            }
        }
        let t = self.target();
        if t.encode_utf16().count() > 255 {
            errs.push(format!("{who}: target is longer than 255 characters ({t})"));
        }
    }
}

/// `scheme://host/path` without user info and query (credentials and tokens must not reach the
/// database or logs).
pub fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => url.split(['?', '#']).next().unwrap_or("").to_string(),
    }
}

impl Routes {
    /// Wrap a list of routes.
    pub fn new(routes: Vec<Route>) -> Self {
        Routes { routes }
    }

    /// Check the configuration (`sip-faxd config check`). Returns every problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        let last = self.routes.len().saturating_sub(1);
        for (i, r) in self.routes.iter().enumerate() {
            let who = r.label(i);
            let conds =
                r.dnis.is_some() as u8 + r.dnis_prefix.is_some() as u8 + r.trunk.is_some() as u8;
            if r.default && conds > 0 {
                errs.push(format!(
                    "{who}: default = true cannot be combined with dnis/dnis_prefix/trunk"
                ));
            }
            if !r.default && conds == 0 {
                errs.push(format!(
                    "{who}: needs dnis, dnis_prefix, trunk or default = true"
                ));
            }
            if r.dnis.as_deref() == Some("")
                || r.dnis_prefix.as_deref() == Some("")
                || r.trunk.as_deref() == Some("")
            {
                errs.push(format!("{who}: empty match value"));
            }
            if r.default && i != last {
                errs.push(format!(
                    "{who}: default route must be last (later routes would never match)"
                ));
            }
            if r.deliveries.is_empty() {
                errs.push(format!("{who}: no deliveries"));
            }
            let mut targets = HashSet::new();
            for (j, d) in r.deliveries.iter().enumerate() {
                let dw = format!("{who}.deliveries[{j}]");
                d.validate(&dw, &mut errs);
                if !targets.insert(d.target()) {
                    errs.push(format!(
                        "{dw}: duplicate target {} in this route",
                        d.target()
                    ));
                }
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
}

#[cfg(test)]
mod tests;
