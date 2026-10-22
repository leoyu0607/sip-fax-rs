//! Extractors with the API's error shape.

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use serde::de::DeserializeOwned;

use crate::error::ApiError;
use crate::session::require_json;

/// The largest request body (W11).
pub const BODY_LIMIT: usize = 1024 * 1024;

/// A JSON body: `Content-Type: application/json` (spec 2.3), at most [`BODY_LIMIT`] bytes, and
/// the fixed error shape (400 / 403 / 413) instead of axum's plain-text rejections.
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, _: &S) -> Result<ApiJson<T>, ApiError> {
        require_json(req.headers())?;
        let bytes = axum::body::to_bytes(req.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "too_large",
                    "the request body is larger than 1 MiB",
                )
            })?;
        serde_json::from_slice(&bytes)
            .map(ApiJson)
            .map_err(|e| ApiError::bad_request(format!("the request body is not valid: {e}")))
    }
}
