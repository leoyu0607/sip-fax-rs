//! Store error type.

/// Errors returned by the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database is unreachable (pool timeout, connection lost, I/O error).
    /// Callers (fax-in's write queue) should retry later.
    #[error("database unavailable: {0}")]
    Unavailable(String),
    /// Any other database error.
    #[error("database error: {0}")]
    Backend(String),
    /// Optimistic-lock failure: somebody else changed the row first.
    #[error("conflict: {0}")]
    Conflict(String),
    /// The row does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// Unique constraint violation (for example a second insert of the same SIP Call-ID).
    #[error("duplicate: {0}")]
    Duplicate(String),
    /// A value exceeds the column limit.
    #[error("{field} too long: {len} > {max}")]
    TooLong {
        /// Field name.
        field: &'static str,
        /// Actual length in characters.
        len: usize,
        /// Maximum length in characters.
        max: usize,
    },
    /// The database schema is newer than this binary understands.
    #[error(
        "database schema version {db} is newer than supported version {supported}; upgrade sip-faxd"
    )]
    SchemaTooNew {
        /// Version found in the database.
        db: i32,
        /// Highest version this binary knows.
        supported: i32,
    },
    /// The database has migrations pending and `auto_migrate` is off.
    #[error(
        "database schema version {db} is older than required {required}; run `sip-faxd db migrate`"
    )]
    SchemaOutdated {
        /// Version found in the database.
        db: i32,
        /// Version required by this binary.
        required: i32,
    },
    /// Bad connection URL or option.
    #[error("invalid database url: {0}")]
    BadUrl(String),
    /// Invalid argument (cursor, retention policy, ...).
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// Unexpected data in the database (unknown enum text, malformed timestamp).
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// True when the failure is transient and the operation should be retried later.
    pub fn is_transient(&self) -> bool {
        matches!(self, Error::Unavailable(_))
    }

    pub(crate) fn from_sqlx(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::Io(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_) => Error::Unavailable(e.to_string()),
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                Error::Duplicate(d.message().to_string())
            }
            _ => Error::Backend(e.to_string()),
        }
    }

    pub(crate) fn from_tiberius(e: tiberius::error::Error) -> Self {
        use tiberius::error::Error as T;
        match &e {
            T::Io { .. } | T::Tls(_) | T::Protocol(_) | T::Routing { .. } => {
                Error::Unavailable(e.to_string())
            }
            T::Server(s) if matches!(s.code(), 2601 | 2627) => {
                Error::Duplicate(s.message().to_string())
            }
            _ => Error::Backend(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
