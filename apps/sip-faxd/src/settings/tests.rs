
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
    let startup = include_str!("../../../../conf/sip-faxd.toml.example");
    let runtime = include_str!("../../../../conf/runtime.toml.example");
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
