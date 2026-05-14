//! Pietro's one and only error type (§17).
//!
//! Every variant maps to one HTTP status and one machine-readable code string.
//! Handlers return `Result<T, Error>`; the `IntoResponse` impl converts that
//! into the project-wide JSON shape:
//!
//! ```text
//! { "error": { "code": "<machine_code>", "message": "<human_message>" } }
//! ```

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("not found")]
    #[allow(dead_code, reason = "constructed by M4 /api/keys handlers")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("conflict: {0}")]
    #[allow(dead_code, reason = "constructed by M4 'key_already_exists' path")]
    Conflict(&'static str),
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    #[error("upstream timed out")]
    #[allow(dead_code, reason = "consumed by M5 proxy handler")]
    UpstreamTimeout,
    #[error("upstream unreachable")]
    #[allow(dead_code, reason = "consumed by M5 proxy handler")]
    UpstreamUnreachable,
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl Error {
    /// Machine-readable error code that appears in the JSON `code` field.
    /// Kept stable across versions — clients may switch on it.
    fn code(&self) -> &'static str {
        match self {
            Error::NotFound => "not_found",
            Error::Unauthorized => "unauthorized",
            Error::Forbidden => "forbidden",
            Error::Conflict(_) => "conflict",
            Error::BadRequest(_) => "bad_request",
            Error::UpstreamTimeout => "upstream_timeout",
            Error::UpstreamUnreachable => "upstream_unreachable",
            Error::Internal(_) => "internal",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Error::NotFound => StatusCode::NOT_FOUND,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::Forbidden => StatusCode::FORBIDDEN,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::UpstreamTimeout => StatusCode::GATEWAY_TIMEOUT,
            Error::UpstreamUnreachable => StatusCode::BAD_GATEWAY,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Human message — safe to show to API consumers. For `Internal`, we
    /// deliberately do NOT echo the underlying error (it goes to logs); the
    /// public surface stays generic so we don't leak implementation details.
    fn public_message(&self) -> String {
        match self {
            Error::Internal(_) => "internal server error".to_string(),
            other => other.to_string(),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        // Log internal errors at error level so they're never silent. The
        // `tracing` span (which carries the request_id per §16) is picked up
        // automatically.
        if let Error::Internal(err) = &self {
            error!(error = %err, "internal error");
        }
        let body = Json(json!({
            "error": {
                "code": self.code(),
                "message": self.public_message(),
            }
        }));
        (self.status(), body).into_response()
    }
}

/// Project-wide `Result` alias. Handlers return this; the boundary layer turns
/// it into a JSON response.
#[allow(dead_code, reason = "alias for handler signatures introduced in M4")]
pub type Result<T> = std::result::Result<T, Error>;
