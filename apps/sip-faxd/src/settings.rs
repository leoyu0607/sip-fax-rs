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
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<AppConfig, String> {
        AppConfig::parse(text, Path::new("test.toml"))
    }

    const TRUNK: &str = "[[trunks]]\nname = \"t\"\nallowed_sources = [\"10.0.0.0/8\"]\n";

    #[test]
    fn s10_defaults_without_database_and_retention_sections() {
        let c = parse(TRUNK).unwrap();
        assert_eq!(c.database, DatabaseSection::default());
        assert_eq!(c.database.url, "sqlite://data/sip-faxd.db");
        assert!(c.database.auto_migrate);
        assert_eq!(c.database.max_connections, 5);
        assert_eq!(
            (
                c.retention.files_days,
                c.retention.records_days,
                c.retention.run_at.as_str()
            ),
            (90, 365, "03:17")
        );
    }

    #[test]
    fn s10_the_sections_are_read() {
        let c = parse(&format!(
            "[database]\nurl = \"postgres://u:pw@db/fax\"\nauto_migrate = false\nmax_connections = 12\n\
             [retention]\nfiles_days = 30\nrecords_days = 0\nrun_at = \"23:05\"\n{TRUNK}"
        ))
        .unwrap();
        assert_eq!(c.database.url, "postgres://u:pw@db/fax");
        assert!(!c.database.auto_migrate);
        assert_eq!(c.database.max_connections, 12);
        let s = c.retention.settings();
        assert_eq!((s.policy.files_days, s.policy.records_days), (30, 0));
        assert_eq!(s.run_at, "23:05");
    }

    #[test]
    fn s10_bad_values_are_refused_with_the_section_named() {
        for (text, needle) in [
            ("[database]\nurl = \"oracle://x\"\n", "[database]"),
            ("[database]\nmax_connections = 0\n", "max_connections"),
            ("[database]\nsurprise = 1\n", "surprise"),
            (
                "[retention]\nfiles_days = 90\nrecords_days = 30\n",
                "records_days",
            ),
            ("[retention]\nrun_at = \"3:17\"\n", "run_at"),
            ("[retention]\nrun_at = \"24:00\"\n", "run_at"),
            ("[retention]\nfiles_days = -1\n", "[retention]"),
        ] {
            let e = parse(&format!("{text}{TRUNK}"))
                .err()
                .unwrap_or_else(|| panic!("{text} accepted"));
            assert!(e.contains(needle), "{text}: {e}");
        }
    }

    #[test]
    fn the_example_configuration_file_parses_and_documents_every_new_option() {
        let startup = include_str!("../../../conf/sip-faxd.toml.example");
        let runtime = include_str!("../../../conf/runtime.toml.example");
        let c = parse(startup).expect("conf/sip-faxd.toml.example is valid");
        assert!(c.database.auto_migrate);
        // the configuration file holds the startup settings only (ADR-060) ...
        assert!(c.file_sections.is_empty(), "{:?}", c.file_sections);
        // ... and the runtime example is a file `config import --file` reads
        let r = parse(runtime).expect("conf/runtime.toml.example is valid");
        assert!(!r.file_sections.is_empty());
        for (file, keys) in [
            (
                startup,
                &[
                    "[database]",
                    "type",
                    "path",
                    "password_file",
                    "trust_server_certificate",
                    "db check",
                ][..],
            ),
            (
                runtime,
                &["[sip]", "[[trunks]]", "config import --file"][..],
            ),
        ] {
            for key in keys {
                assert!(file.contains(key), "the example does not mention {key}");
            }
        }
        let text = format!("{startup}{runtime}");
        for key in [
            "[database]",
            "auto_migrate",
            "max_connections",
            "[retention]",
            "files_days",
            "records_days",
            "run_at",
            "[trunks.numbers]",
            "ani_sources",
            "dnis_sources",
            "strip_plus",
            "ani_strip_prefix",
            "dnis_strip_prefix",
            "[trunks.fax]",
            "resolutions",
            "ecm",
            "[service]",
            "pid_file",
            "socket",
            "stop_timeout_secs",
            "start_timeout_secs",
            "[web]",
            "scheme",
            "tls_names",
            "session_idle_hours",
            "max_size_mb",
            "keep",
            "compress",
            "daily",
        ] {
            assert!(text.contains(key), "the example does not mention {key}");
        }
    }

    #[test]
    fn urls_are_masked_and_sqlite_paths_resolved() {
        assert_eq!(
            mask_url("postgres://u:secret@db:5432/fax"),
            "postgres://u:***@db:5432/fax"
        );
        assert_eq!(
            mask_url("mssql://sa:p%40ss@h/db?encrypt=on"),
            "mssql://sa:***@h/db?encrypt=on"
        );
        assert_eq!(mask_url("mysql://u@h/db"), "mysql://u@h/db");
        assert_eq!(mask_url("sqlite://data/x.db"), "sqlite://data/x.db");
        assert_eq!(mask_url("not a url"), "not a url");
        let b = Path::new("/opt/sip-faxd");
        assert_eq!(
            resolve_database_url("sqlite://data/x.db", b),
            "sqlite:///opt/sip-faxd/data/x.db"
        );
        assert_eq!(
            resolve_database_url("sqlite:data/x.db?mode=rwc", b),
            "sqlite:///opt/sip-faxd/data/x.db?mode=rwc"
        );
        assert_eq!(
            resolve_database_url("sqlite:///var/db/x.db", b),
            "sqlite:///var/db/x.db"
        );
        assert_eq!(
            resolve_database_url("sqlite::memory:", b),
            "sqlite::memory:"
        );
        assert_eq!(
            resolve_database_url("postgres://h/db", b),
            "postgres://h/db"
        );
    }
}

#[cfg(test)]
mod routes_tests {
    //! F01..F05 (configuration side of M3f).
    use super::*;
    use delivery::DeliveryConfig;

    fn parse(text: &str) -> Result<AppConfig, String> {
        AppConfig::parse(text, Path::new("test.toml"))
    }

    const TRUNK: &str = "[[trunks]]\nname = \"t\"\nallowed_sources = [\"10.0.0.0/8\"]\n";

    #[test]
    fn f01_without_routes_or_a_delivery_section_the_defaults_apply() {
        let c = parse(TRUNK).unwrap();
        assert!(c.routes.routes.is_empty());
        assert_eq!(c.delivery, DeliverySection::default());
        assert_eq!(c.delivery.catch_up_hours, 24);
        assert_eq!(c.delivery.max_attempts, 10);
    }

    #[test]
    fn f02_routes_and_the_delivery_section_are_read_with_every_kind() {
        let c = parse(&format!(
            "[delivery]\ncatch_up_hours = 6\nmax_attempts = 3\n{TRUNK}\
             [[routes]]\nname = \"sales\"\ndnis_prefix = \"2557\"\ndeliver_partial = true\n\
             [[routes.deliveries]]\nkind = \"folder\"\ndir = \"/srv/in\"\n\
             [[routes.deliveries]]\nkind = \"email\"\nhost = \"smtp.example.com\"\nfrom = \"fax@example.com\"\nto = [\"a@example.com\"]\n\
             [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://example.com/hook\"\n\
             [[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/srv/other\"\n"
        ))
        .unwrap();
        assert_eq!((c.delivery.catch_up_hours, c.delivery.max_attempts), (6, 3));
        assert_eq!(c.routes.routes.len(), 2);
        let r = &c.routes.routes[0];
        assert_eq!(r.name.as_deref(), Some("sales"));
        assert_eq!(r.dnis_prefix.as_deref(), Some("2557"));
        assert!(r.deliver_partial);
        let kinds: Vec<_> = r.deliveries.iter().map(DeliveryConfig::kind).collect();
        assert_eq!(kinds, ["folder", "email", "webhook"]);
        assert!(c.routes.routes[1].default);
        assert!(!c.routes.routes[1].deliver_partial);
    }

    #[test]
    fn f03_invalid_routes_stop_the_start_and_name_every_problem() {
        let e = parse(&format!(
            "{TRUNK}[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/a\"\n\
             [[routes]]\ndnis = \"1\"\n"
        ))
        .err()
        .expect("a default route that is not last, a route without deliveries");
        assert!(e.contains("[[routes]]"), "{e}");
        assert!(e.contains("default route must be last"), "{e}");
        assert!(e.contains("no deliveries"), "{e}");
        for (text, needle) in [
            ("[[routes]]\ndnis = \"1\"\nbogus = 1\n", "bogus"),
            (
                "[[routes]]\ndnis = \"1\"\n[[routes.deliveries]]\nkind = \"ftp\"\n",
                "ftp",
            ),
            ("[delivery]\nmax_attempts = 0\n", "max_attempts"),
            ("[delivery]\nsurprise = 1\n", "surprise"),
            ("routes = 5\n", "[[routes]]"),
        ] {
            let e = parse(&format!("{text}{TRUNK}"))
                .err()
                .unwrap_or_else(|| panic!("{text} accepted"));
            assert!(e.contains(needle), "{text}: {e}");
        }
    }

    #[test]
    fn f04_webhook_urls_are_masked_for_display() {
        assert_eq!(
            mask_http_url("https://user:hunter2@example.com/hook?token=abc&x=1#frag"),
            "https://user:***@example.com/hook?token=***&x=***"
        );
        assert_eq!(
            mask_http_url("http://example.com:8080/a/b"),
            "http://example.com:8080/a/b"
        );
        assert_eq!(mask_http_url("http://h/p?k"), "http://h/p?k=***");
    }

    #[test]
    fn f06_the_routes_in_the_example_file_are_valid_once_uncommented() {
        let text = include_str!("../../../conf/runtime.toml.example");
        let begin = text.find("# routes-example-begin").expect("begin marker");
        let end = text.find("# routes-example-end").expect("end marker");
        let routes: String = text[begin..end]
            .lines()
            .skip(1)
            .map(|l| {
                l.strip_prefix("# ")
                    .or_else(|| l.strip_prefix('#'))
                    .unwrap_or(l)
            })
            .collect::<Vec<_>>()
            .join("\n");
        // the example file itself delivers nothing ...
        assert!(parse(text).unwrap().routes.routes.is_empty());
        // ... and its documented routes work
        let c = parse(&format!("{TRUNK}{routes}")).unwrap();
        assert_eq!(c.routes.routes.len(), 2);
        assert_eq!(c.routes.routes[0].deliveries.len(), 3);
        assert!(c.routes.routes[1].default);
        let at = parse(text).unwrap().delivery;
        assert_eq!((at.catch_up_hours, at.max_attempts), (24, 10));
        assert!(text.contains("[delivery]") && text.contains("catch_up_hours"));
    }
}
