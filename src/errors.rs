//! Pietro's one and only error type (§17).
//!
//! Every variant maps to one HTTP status and one machine-readable code string.
//! The handler-side `IntoResponse` impl (and the matching JSON shape) will be
//! added in M2/M3 once we have a real session/auth surface — for M1, the only
//! responses we produce are `/healthz`'s 200, so we don't need it yet.

// M1 only emits `/healthz`. The variants here are part of the locked plan
// (§17) and will be consumed as the milestones land. Annotate as a single
// expectation so a future PR removes the attribute when the items are wired.
#![allow(dead_code)]

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    #[error("upstream timed out")]
    UpstreamTimeout,
    #[error("upstream unreachable")]
    UpstreamUnreachable,
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Project-wide `Result` alias. Handlers return this; the boundary layer turns
/// it into a JSON response.
pub type Result<T> = std::result::Result<T, Error>;
