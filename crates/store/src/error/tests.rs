use super::*;

#[test]
fn classifies_sqlx_errors() {
    assert!(Error::from_sqlx(sqlx::Error::PoolTimedOut).is_transient());
    assert!(Error::from_sqlx(sqlx::Error::PoolClosed).is_transient());
    assert!(
        Error::from_sqlx(sqlx::Error::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        )))
        .is_transient()
    );
    assert!(!Error::from_sqlx(sqlx::Error::RowNotFound).is_transient());
    assert!(matches!(
        Error::from_sqlx(sqlx::Error::RowNotFound),
        Error::Backend(_)
    ));
}

#[test]
fn classifies_tiberius_errors() {
    use tiberius::error::Error as T;
    let io = T::Io {
        kind: std::io::ErrorKind::ConnectionReset,
        message: "reset".into(),
    };
    assert!(Error::from_tiberius(io).is_transient());
    assert!(!Error::from_tiberius(T::Conversion("x".into())).is_transient());
}

#[test]
fn messages_are_actionable() {
    let e = Error::SchemaTooNew {
        db: 9,
        supported: 1,
    };
    assert!(e.to_string().contains("newer") && e.to_string().contains('9'));
    assert!(
        Error::SchemaOutdated { db: 0, required: 1 }
            .to_string()
            .contains("db migrate")
    );
}
