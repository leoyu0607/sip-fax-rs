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

/// The fixed code of a status that came from the framework (an extractor's rejection, a method
/// the route does not have) and not from a handler.
fn code_of(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::REQUEST_TIMEOUT => "timeout",
        StatusCode::PAYLOAD_TOO_LARGE => "too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        StatusCode::UNPROCESSABLE_ENTITY => "unprocessable",
        s if s.is_server_error() => "internal",
        _ => "error",
    }
}

/// Turns an error answer that is not JSON (axum's plain-text rejections: a path that is not
/// UTF-8, a method the route does not have, ...) into the API's error shape (W10). The original
/// text is dropped: it is the framework's, and says more than a client needs. Headers such as
/// `Allow` stay.
pub fn json_errors(mut r: Response) -> Response {
    let status = r.status();
    let is_json = r
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !(status.is_client_error() || status.is_server_error()) || is_json {
        return r;
    }
    let mut shaped = ApiError::new(
        status,
        code_of(status),
        status.canonical_reason().unwrap_or("error").to_lowercase(),
    )
    .into_response();
    for (k, v) in r.headers_mut().drain() {
        if let Some(k) = k
            && k != axum::http::header::CONTENT_TYPE
            && k != axum::http::header::CONTENT_LENGTH
        {
            shaped.headers_mut().insert(k, v);
        }
    }
    shaped
}
