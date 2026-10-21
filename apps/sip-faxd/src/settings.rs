//! The application part of the configuration file: `[log]`, `[database]` and `[retention]`
//! (the rest belongs to `fax_in::FaxInConfig`).

use std::path::{Path, PathBuf};

use fax_in::FaxInConfig;
use serde::Deserialize;
use store::{RetentionPolicy, RetentionSettings};

/// `[log]`
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LogSection {
    /// Default log filter (overridden by `RUST_LOG`).
    pub level: String,
}

/// `[database]`
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseSection {
    /// `sqlite://path`, `postgres://...`, `mysql://...` (or `mariadb://`), `mssql://...`. A
    /// relative SQLite path is relative to the executable directory.
    pub url: String,
    /// Apply pending migrations at start (default). With `false` an outdated schema stops the
    /// start and `sip-faxd db migrate` has to be run by hand.
    pub auto_migrate: bool,
    /// Connections in the pool.
    pub max_connections: u32,
}

impl Default for DatabaseSection {
    fn default() -> Self {
        DatabaseSection {
            url: "sqlite://data/sip-faxd.db".into(),
            auto_migrate: true,
            max_connections: 5,
        }
    }
}

/// `[retention]`: written to the `settings` table on the first start; after that the database
/// is the source of truth (the web UI changes it there).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSection {
    /// Delete TIFF / PDF / PNG files after this many days (0 = never).
    pub files_days: u32,
    /// Delete the records after this many days (0 = never; at least `files_days`).
    pub records_days: u32,
    /// Daily run time `HH:MM`, server local time.
    pub run_at: String,
}

impl Default for RetentionSection {
    fn default() -> Self {
        let d = RetentionSettings::default();
        RetentionSection {
            files_days: d.policy.files_days,
            records_days: d.policy.records_days,
            run_at: d.run_at,
        }
    }
}

impl RetentionSection {
    /// As the store's settings type.
    pub fn settings(&self) -> RetentionSettings {
        RetentionSettings {
            policy: RetentionPolicy {
                files_days: self.files_days,
                records_days: self.records_days,
            },
            run_at: self.run_at.clone(),
        }
    }
}

/// The whole configuration file.
pub struct AppConfig {
    /// `[log]`
    pub log: LogSection,
    /// `[database]`
    pub database: DatabaseSection,
    /// `[retention]`
    pub retention: RetentionSection,
    /// Everything fax-in reads.
    pub fax_in: FaxInConfig,
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
        let database: DatabaseSection = section(&mut table, "database", path)?;
        let retention: RetentionSection = section(&mut table, "retention", path)?;
        let fax_in =
            FaxInConfig::from_table(table).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg = AppConfig {
            log,
            database,
            retention,
            fax_in,
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
        store::backend_for_url(&self.database.url).map_err(|e| format!("[database] {e}"))?;
        if self.database.max_connections == 0 {
            return Err("[database] max_connections must be at least 1".into());
        }
        self.retention
            .settings()
            .validate()
            .map_err(|e| format!("[retention] {e}"))?;
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
        let text = include_str!("../../../conf/sip-faxd.toml.example");
        let c = parse(text).expect("conf/sip-faxd.toml.example is valid");
        assert!(c.database.auto_migrate);
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
