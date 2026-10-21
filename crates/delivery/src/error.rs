//! Error types.

use std::fmt;

/// Whether a failed attempt is worth repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Connection failure, timeout, HTTP 5xx / 408 / 425 / 429, SMTP 4xx, filesystem errors
    /// (permission denied, missing directory): retried on the back-off schedule.
    Transient,
    /// HTTP 4xx (other than the above), SMTP 5xx, missing source file, bad configuration: retrying
    /// cannot help, the attempt is recorded as `failed` and no further attempt is scheduled.
    Permanent,
}

/// Outcome of one failed delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryError {
    /// Retry classification.
    pub kind: FailureKind,
    /// Text stored in `delivery_attempts.last_error`.
    pub message: String,
}

impl DeliveryError {
    /// A retryable failure.
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Transient,
            message: message.into(),
        }
    }
    /// A failure that will not be retried.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Permanent,
            message: message.into(),
        }
    }
    /// Whether the attempt should be retried.
    pub fn is_transient(&self) -> bool {
        self.kind == FailureKind::Transient
    }
}

impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for DeliveryError {}

/// Errors of the delivery engine itself (not of a single attempt).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Database error.
    #[error("store: {0}")]
    Store(#[from] store::Error),
    /// Invalid `[[routes]]` configuration; one message per problem.
    #[error("invalid delivery configuration: {}", .0.join("; "))]
    Config(Vec<String>),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;
