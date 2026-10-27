//! The configuration file: the startup tables (`[log]`, `[database]`, `[storage]`) and the runtime
//! tables (`[sip]`, `[media]`, `[call]`, `[capture]`, `[[trunks]]`, `[[routes]]`, `[delivery]`,
//! `[retention]`), which are imported into the database on the first start and ignored afterwards
//! (ADR-051; see `runtime_config`).

use std::path::{Path, PathBuf};

use fax_in::FaxInConfig;
use runtime_config::{RuntimeConfig, RuntimeFile};
use serde::Deserialize;

// `[log]` and `[service]` live with the service management code (M4d).
pub use crate::service::config::{LogSection, ServiceSection};

/// `[database]`, as the rest of the program uses it: one URL, whichever way the file wrote it
/// (`url = ...`, or the fields `type`, `host`, `port`, `name`, `user`, `password` /
/// `password_file`, `trust_server_certificate`, `path`; ADR-060).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseSection {
    /// `sqlite://path`, `postgres://...`, `mysql://...` (or `mariadb://`), `mssql://...`, with the
    /// password in it (show it with `mask_url`). A relative SQLite path is relative to the
    /// executable directory.
    pub url: String,
    /// Apply pending migrations at start (default). With `false` an outdated schema stops the
    /// start and `sip-faxd db migrate` has to be run by hand.
    pub auto_migrate: bool,
    /// Connections in the pool.
    pub max_connections: u32,
    /// Things to fix that do not stop the start (a password file others can read).
    pub warnings: Vec<String>,
}

impl Default for DatabaseSection {
    fn default() -> Self {
        DatabaseSection {
            url: DEFAULT_DATABASE_URL.into(),
            auto_migrate: true,
            max_connections: 5,
            warnings: Vec::new(),
        }
    }
}

/// The database when `[database]` names none.
pub const DEFAULT_DATABASE_URL: &str = "sqlite://data/sip-faxd.db";

/// `[database]` as written in the file.
#[derive(Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields)]
struct DatabaseFile {
    url: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    name: Option<String>,
    user: Option<String>,
    password: Option<String>,
    password_file: Option<PathBuf>,
    trust_server_certificate: Option<bool>,
    path: Option<String>,
    auto_migrate: Option<bool>,
    max_connections: Option<u32>,
}

/// Percent-encoding of a user name or password in a URL: everything but letters and digits.
const USERINFO: &percent_encoding::AsciiSet = percent_encoding::NON_ALPHANUMERIC;

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, USERINFO).to_string()
}

impl DatabaseFile {
    /// The URL (and warnings); `base` resolves a relative `password_file`.
    fn into_section(self, base: &Path) -> Result<DatabaseSection, String> {
        let mut warnings = Vec::new();
        let url = match (&self.url, &self.kind) {
            (Some(_), Some(_)) => {
                return Err(
                    "[database] url and type: write either the url or the fields, not both".into(),
                );
            }
            (Some(u), None) => {
                if let Some(f) = self.split_field() {
                    return Err(format!(
                        "[database] {f}: only with type = ... (the url already says it)"
                    ));
                }
                u.clone()
            }
            (None, Some(kind)) => self.build(kind, base, &mut warnings)?,
            (None, None) => {
                if let Some(f) = self.split_field() {
                    return Err(format!(
                        "[database] {f}: needs type = \"sqlite\" | \"postgres\" | \"mysql\" | \"mariadb\" | \"mssql\""
                    ));
                }
                DEFAULT_DATABASE_URL.into()
            }
        };
        Ok(DatabaseSection {
            url,
            auto_migrate: self.auto_migrate.unwrap_or(true),
            max_connections: self.max_connections.unwrap_or(5),
            warnings,
        })
    }

    /// The first split field that is set (they need `type`).
    fn split_field(&self) -> Option<&'static str> {
        [
            ("host", self.host.is_some()),
            ("port", self.port.is_some()),
            ("name", self.name.is_some()),
            ("user", self.user.is_some()),
            ("password", self.password.is_some()),
            ("password_file", self.password_file.is_some()),
            (
                "trust_server_certificate",
                self.trust_server_certificate.is_some(),
            ),
            ("path", self.path.is_some()),
        ]
        .into_iter()
        .find(|(_, set)| *set)
        .map(|(f, _)| f)
    }

    fn build(&self, kind: &str, base: &Path, warnings: &mut Vec<String>) -> Result<String, String> {
        let (scheme, port) = match kind {
            "sqlite" => {
                for (f, set) in [
                    ("host", self.host.is_some()),
                    ("port", self.port.is_some()),
                    ("name", self.name.is_some()),
                    ("user", self.user.is_some()),
                    ("password", self.password.is_some()),
                    ("password_file", self.password_file.is_some()),
                    (
                        "trust_server_certificate",
                        self.trust_server_certificate.is_some(),
                    ),
                ] {
                    if set {
                        return Err(format!(
                            "[database] {f}: not used with type = \"sqlite\" (it has path)"
                        ));
                    }
                }
                let path = self.path.as_deref().unwrap_or("data/sip-faxd.db");
                if path.trim().is_empty() {
                    return Err("[database] path: empty".into());
                }
                return Ok(format!("sqlite://{path}"));
            }
            "postgres" | "postgresql" => ("postgres", 5432),
            "mysql" => ("mysql", 3306),
            "mariadb" => ("mariadb", 3306),
            "mssql" | "sqlserver" => ("mssql", 1433),
            other => {
                return Err(format!(
                    "[database] type: unknown {other:?} (sqlite, postgres, mysql, mariadb or mssql)"
                ));
            }
        };
        if self.path.is_some() {
            return Err(format!(
                "[database] path: only with type = \"sqlite\" (use host and name for {kind})"
            ));
        }
        if self.trust_server_certificate.is_some() && scheme != "mssql" {
            return Err("[database] trust_server_certificate: only with type = \"mssql\"".into());
        }
        let host = self
            .host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| format!("[database] host: required with type = \"{kind}\""))?;
        let name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                format!("[database] name: required with type = \"{kind}\" (the database name)")
            })?;
        let password = match (&self.password, &self.password_file) {
            (Some(_), Some(_)) => {
                return Err("[database] password and password_file: give one of them".into());
            }
            (Some(p), None) => Some(p.clone()),
            (None, Some(f)) => Some(read_password_file(&resolve(base, f), warnings)?),
            (None, None) => None,
        };
        if password.is_some() && self.user.is_none() {
            return Err("[database] user: required with a password".into());
        }
        let userinfo = match (&self.user, &password) {
            (Some(u), Some(p)) => format!("{}:{}@", enc(u), enc(p)),
            (Some(u), None) => format!("{}@", enc(u)),
            _ => String::new(),
        };
        // an IPv6 address goes in brackets
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let mut url = format!(
            "{scheme}://{userinfo}{host}:{}/{}",
            self.port.unwrap_or(port),
            enc(name)
        );
        if self.trust_server_certificate == Some(true) {
            url.push_str("?trust_server_certificate=true");
        }
        Ok(url)
    }
}

/// The first line of a password file, without the line end.
fn read_password_file(path: &Path, warnings: &mut Vec<String>) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "[database] password_file: cannot read {}: {e}",
            path.display()
        )
    })?;
    let pw = text
        .lines()
        .next()
        .unwrap_or("")
        .trim_end_matches('\r')
        .to_string();
    if pw.is_empty() {
        return Err(format!(
            "[database] password_file: {} is empty",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(path)
            && m.permissions().mode() & 0o077 != 0
        {
            warnings.push(format!(
                "[database] password_file: {} can be read by other users (mode {:o}); chmod 600 it",
                path.display(),
                m.permissions().mode() & 0o777
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = warnings;
    Ok(pw)
}

pub use runtime_config::{DeliverySection, RetentionSection};

/// The whole configuration file.
pub struct AppConfig {
    /// `[log]`
    pub log: LogSection,
    /// `[service]`
    pub service: ServiceSection,
    /// `[web]`
    pub web: web::config::WebConfig,
    /// `[database]`
    pub database: DatabaseSection,
    /// `[retention]` (runtime section: the file's copy, used for the first import)
    pub retention: RetentionSection,
    /// `[delivery]` (runtime section)
    pub delivery: DeliverySection,
    /// `[[routes]]` (runtime section)
    pub routes: delivery::Routes,
    /// Everything fax-in reads (the runtime sections plus `[storage]`).
    pub fax_in: FaxInConfig,
    /// The runtime sections as one value.
    pub runtime: RuntimeConfig,
    /// The runtime tables the file actually contains.
    pub file_sections: Vec<String>,
    /// The directory of the configuration file (`secret.key` and `initial-admin-password` live
    /// here).
    pub conf_dir: PathBuf,
}

fn section<T: for<'de> Deserialize<'de> + Default>(
    table: &mut toml::Table,
    name: &str,
    path: &Path,
) -> Result<T, String> {
    match table.remove(name) {
        None => Ok(T::default()),
        Some(v) => v
            .try_into()
            .map_err(|e: toml::de::Error| format!("{}: [{name}]: {}", path.display(), e.message())),
    }
}

impl AppConfig {
    /// Parses configuration text; `path` only appears in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<AppConfig, String> {
        let mut table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            format!("{}: invalid TOML: {}", path.display(), e.message())
        })?;
        let log = section(&mut table, "log", path)?;
        let service = section(&mut table, "service", path)?;
        let web = section(&mut table, "web", path)?;
        let database = section::<DatabaseFile>(&mut table, "database", path)?
            .into_section(&crate::exe_dir())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let file =
            RuntimeFile::from_table(table).map_err(|e| format!("{}: {e}", path.display()))?;
        let conf_dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let cfg = AppConfig {
            log,
            service,
            web,
            database,
            retention: file.runtime.retention.clone(),
            delivery: file.runtime.delivery.clone(),
            routes: file.runtime.routes.clone(),
            fax_in: file.fax_in,
            runtime: file.runtime,
            file_sections: file.present,
            conf_dir,
        };
        cfg.validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(cfg)
    }

    /// Reads and parses a file.
    pub fn load(path: &Path) -> Result<AppConfig, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read configuration {}: {e}", path.display()))?;
        Self::parse(&text, path)
    }

    fn validate(&self) -> Result<(), String> {
        self.log.validate()?;
        self.service.validate()?;
        self.web.validate()?;
        store::backend_for_url(&self.database.url).map_err(|e| format!("[database] {e}"))?;
        if self.database.max_connections == 0 {
            return Err("[database] max_connections must be at least 1".into());
        }
        Ok(())
    }

    /// `[storage] data_dir` against the executable directory.
    pub fn data_dir(&self, base: &Path) -> PathBuf {
        resolve(base, &self.fax_in.storage.data_dir)
    }

    /// `[capture] dir` (where `result.json` files live) against the executable directory.
    pub fn calls_dir(&self, base: &Path) -> PathBuf {
        resolve(base, &self.fax_in.capture.dir)
    }

    /// The database URL with a relative SQLite path made absolute.
    pub fn database_url(&self, base: &Path) -> String {
        resolve_database_url(&self.database.url, base)
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_relative() {
        base.join(p)
    } else {
        p.to_path_buf()
    }
}

/// `sqlite://data/x.db` -> `sqlite://<base>/data/x.db`. Other URLs, in-memory databases and
/// absolute paths are returned unchanged.
pub fn resolve_database_url(url: &str, base: &Path) -> String {
    let Some(rest) = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
    else {
        return url.to_string();
    };
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    if path.is_empty() || path.contains(":memory:") || path.starts_with("file:") {
        return url.to_string();
    }
    if Path::new(path).is_absolute() || path.starts_with('/') {
        return url.to_string();
    }
    let joined = base.join(path);
    let mut out = format!("sqlite://{}", joined.display()).replace('\\', "/");
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

// (`mask_url` / `mask_http_url` live in `delivery::mask`, shared with the web interface.)
pub use delivery::mask::{mask_http_url, mask_url};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod routes_tests;

#[cfg(test)]
mod m4f_tests;
