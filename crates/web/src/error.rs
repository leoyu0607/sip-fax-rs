//! The one error shape of the API (spec 3): `{"error": {"code", "message", "fields"?}}`. The
//! front end translates `code`; `message` is English for logs and never contains paths or SQL.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

/// An API error.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Fixed code (`not_found`, `invalid_credentials`, ...).
    pub code: &'static str,
    /// English explanation.
    pub message: String,
    /// Per-field details (validation errors), or extra values (a conflict's current state).
    pub fields: Option<Map<String, Value>>,
}

impl ApiError {
    /// An error without fields.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
            fields: None,
        }
    }

    /// 404 `not_found`.
    pub fn not_found(what: &str) -> ApiError {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{what} not found"),
        )
    }

    /// 400 `bad_request`.
    pub fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// 500 `internal`; the cause goes to the log only.
    pub fn internal(cause: impl std::fmt::Display) -> ApiError {
        tracing::error!("web: internal error: {cause}");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error (see the server log)",
        )
    }

    /// 503 `unavailable` (the database does not answer).
    pub fn unavailable() -> ApiError {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the database is not reachable, try again later",
        )
    }

    /// With details.
    pub fn with_fields(mut self, fields: Map<String, Value>) -> ApiError {
        self.fields = Some(fields);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut e = json!({"code": self.code, "message": self.message});
        if let Some(f) = self.fields {
            e["fields"] = Value::Object(f);
        }
        (self.status, Json(json!({ "error": e }))).into_response()
    }
}

/// The fallback of the router: everything that is not a route.
pub async fn not_found() -> ApiError {
    ApiError::not_found("resource")
}
