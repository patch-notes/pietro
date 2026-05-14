//! Auth: OIDC login + DB-backed sessions (§10).
//!
//! Two submodules:
//!   * [`session`] — session DB rows and the `AuthenticatedUser` extractor.
//!   * [`oidc`]    — OIDC client builder and the `/api/auth/*` handlers.

pub mod oidc;
pub mod session;
