//! Database-free unit tests: URLs, enums, cursors, SQL helpers.

use super::*;
use crate::db::{rewrite_placeholders, split_statements};

#[test]
fn url_scheme_selects_backend() {
    use BackendKind::*;
    for (u, k) in [
        ("sqlite://data/x.db", Sqlite),
        ("sqlite::memory:", Sqlite),
        ("postgres://u:p@h/db", Postgres),
        ("postgresql://u@h/db", Postgres),
        ("mysql://u:p@h/db", Mysql),
        ("mariadb://u:p@h/db", Mysql),
        ("mssql://sa:p@h:1433/db", Mssql),
        ("sqlserver://sa@h/db", Mssql),
    ] {
        assert_eq!(backend_for_url(u).unwrap(), k, "{u}");
    }
    for bad in ["", "http://x", "oracle://x", "no-scheme"] {
        assert!(
            matches!(backend_for_url(bad), Err(Error::BadUrl(_))),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn connect_rejects_bad_urls_and_unreachable_servers() {
    assert!(matches!(
        connect("ftp://x").await.err().unwrap(),
        Error::BadUrl(_)
    ));
    assert!(matches!(
        connect("mssql://sa:p@127.0.0.1:1/db?bogus=1")
            .await
            .err()
            .unwrap(),
        Error::BadUrl(_)
    ));
    assert!(matches!(
        connect("mssql://sa:p@127.0.0.1:1/db?encrypt=maybe")
            .await
            .err()
            .unwrap(),
        Error::BadUrl(_)
    ));
    // Nothing listens on port 1: a transient (retryable) error, not a panic.
    for u in [
        "postgres://u:p@127.0.0.1:1/db",
        "mysql://u:p@127.0.0.1:1/db",
        "mssql://sa:p@127.0.0.1:1/db?trust_server_certificate=true",
    ] {
        let e = connect_with(
            u,
            ConnectOptions {
                max_connections: 1,
                ..Default::default()
            },
        )
        .await
        .err()
        .unwrap();
        assert!(
            e.is_transient() || matches!(e, Error::Backend(_)),
            "{u}: {e:?}"
        );
    }
}

#[tokio::test]
async fn sqlite_creates_parent_directory() {
    let d = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/nested/dir/x.db", d.path().display());
    let st = connect(&url).await.unwrap();
    assert_eq!(st.backend(), BackendKind::Sqlite);
    assert_eq!(
        st.schema_version().await.unwrap(),
        latest_version(BackendKind::Sqlite)
    );
    st.close().await;
    assert!(d.path().join("nested/dir/x.db").exists());
}

#[test]
fn enums_roundtrip_and_reject_unknown() {
    for s in [
        FaxStatus::Receiving,
        FaxStatus::Received,
        FaxStatus::Partial,
        FaxStatus::Failed,
        FaxStatus::NoFax,
    ] {
        assert_eq!(s.as_str().parse::<FaxStatus>().unwrap(), s);
        assert_eq!(s.to_string(), s.as_str());
    }
    for d in [
        HangupDirection::Remote,
        HangupDirection::Local,
        HangupDirection::System,
    ] {
        assert_eq!(d.as_str().parse::<HangupDirection>().unwrap(), d);
    }
    for s in [
        DeliveryStatus::Pending,
        DeliveryStatus::Done,
        DeliveryStatus::Failed,
        DeliveryStatus::GivingUp,
    ] {
        assert_eq!(s.as_str().parse::<DeliveryStatus>().unwrap(), s);
    }
    assert!(matches!(
        "RECEIVED".parse::<FaxStatus>(),
        Err(Error::Corrupt(_))
    ));
    assert!("up".parse::<HangupDirection>().is_err());
}

#[test]
fn cursor_roundtrip_and_validation() {
    let c = Cursor {
        started_ms: T0 + 5,
        id: 42,
    };
    assert_eq!(c.to_string().parse::<Cursor>().unwrap(), c);
    assert_eq!(c.started_at(), t(T0 + 5));
    for bad in [
        "",
        "1",
        "a.b",
        "1.",
        ".1",
        "1.2.x",
        "99999999999999999999.1",
    ] {
        assert!(bad.parse::<Cursor>().is_err(), "{bad:?}");
    }
}

#[test]
fn retention_policy_validation() {
    assert!(
        RetentionPolicy {
            files_days: 90,
            records_days: 365
        }
        .validate()
        .is_ok()
    );
    assert!(
        RetentionPolicy {
            files_days: 90,
            records_days: 90
        }
        .validate()
        .is_ok()
    );
    assert!(
        RetentionPolicy {
            files_days: 0,
            records_days: 5
        }
        .validate()
        .is_ok()
    );
    assert!(
        RetentionPolicy {
            files_days: 5,
            records_days: 0
        }
        .validate()
        .is_ok()
    );
    assert!(
        RetentionPolicy {
            files_days: 91,
            records_days: 90
        }
        .validate()
        .is_err()
    );
    for ok in ["00:00", "03:17", "23:59"] {
        assert!(
            RetentionSettings {
                run_at: ok.into(),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }
    for bad in ["3:17", "24:00", "12:60", "1217", "ab:cd", ""] {
        assert!(
            RetentionSettings {
                run_at: bad.into(),
                ..Default::default()
            }
            .validate()
            .is_err(),
            "{bad}"
        );
    }
}

#[test]
fn sql_helpers() {
    assert_eq!(
        rewrite_placeholders("a = ? AND b = ?", |n| format!("${n}")),
        "a = $1 AND b = $2"
    );
    assert_eq!(
        rewrite_placeholders("a = ? AND b = ?", |n| format!("@P{n}")),
        "a = @P1 AND b = @P2"
    );
    assert_eq!(
        split_statements("-- c\nCREATE TABLE a (x INT);\n\n-- more\nCREATE INDEX i ON a (x);\n"),
        vec!["CREATE TABLE a (x INT)", "CREATE INDEX i ON a (x)"]
    );
    assert!(split_statements("-- only a comment\n").is_empty());
}

#[test]
fn every_backend_ships_the_same_migration_versions() {
    use BackendKind::*;
    let versions = |k| {
        crate::migrate::migrations(k)
            .iter()
            .map(|m| (m.version, m.name))
            .collect::<Vec<_>>()
    };
    for k in [Postgres, Mysql, Mssql] {
        assert_eq!(versions(k), versions(Sqlite), "{k:?}");
    }
    for k in [Sqlite, Postgres, Mysql, Mssql] {
        let ms = crate::migrate::migrations(k);
        assert!(
            ms.windows(2).all(|w| w[0].version < w[1].version),
            "ascending"
        );
        for m in ms {
            assert!(!crate::db::split_statements(m.sql).is_empty());
            assert!(
                m.sql.contains("CREATE TABLE faxes")
                    && m.sql.contains("delivery_attempts")
                    && m.sql.contains("settings")
            );
        }
    }
}

#[test]
fn manual_clock_and_error_classes() {
    let c = ManualClock::at_millis(1_000);
    assert_eq!(Clock::now(&c).timestamp_millis(), 1_000);
    c.advance(chrono::Duration::seconds(2));
    assert_eq!(Clock::now(&c).timestamp_millis(), 3_000);
    assert!(Clock::now(&SystemClock).timestamp() > 1_700_000_000);
    assert!(Error::Unavailable("x".into()).is_transient());
    assert!(!Error::Conflict("x".into()).is_transient());
    assert!(
        Error::TooLong {
            field: "note",
            len: 5,
            max: 4
        }
        .to_string()
        .contains("note")
    );
}
