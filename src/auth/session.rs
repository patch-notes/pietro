//! Sessions + the `AuthenticatedUser` extractor (§10).
//!
//! A session is a row in the `sessions` table keyed by a random 128-bit id.
//! The id is carried in a signed cookie (`pietro_session`) so a tampered
//! cookie is rejected at the signature layer before we ever touch the DB.
//!
//! Logout (§10.3) deletes the row, so a signed cookie that the user already
//! has becomes invalid the next time it's presented. This is the whole reason
//! we keep sessions in the DB and not in a self-contained signed cookie.

use std::str::FromStr;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use rand::RngCore;
use sqlx::SqlitePool;
use time::OffsetDateTime;

use crate::errors::Error;
use crate::routes::AppState;

/// Cookie name carrying the session id. Single source of truth.
pub const SESSION_COOKIE: &str = "pietro_session";

/// How long a session lives before the user must re-authenticate (§10.5).
pub const SESSION_TTL: time::Duration = time::Duration::hours(12);

/// Cookie name carrying the in-flight OIDC flow state (state + nonce + PKCE
/// verifier). Short TTL — only valid across the IdP redirect.
pub const FLOW_COOKIE: &str = "pietro_flow";
pub const FLOW_TTL: time::Duration = time::Duration::minutes(5);

/// The OIDC subject (`sub` claim) for an authenticated user. Opaque to us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserId(pub String);

impl UserId {
    #[allow(dead_code, reason = "consumed by /api/keys handlers in M4")]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An axum extractor that yields the authenticated user behind a valid
/// session cookie, or rejects with `Error::Unauthorized`.
///
/// Handlers take this by argument; there is no `Option<UserId>` in handler
/// signatures (§10.2).
pub struct AuthenticatedUser(pub UserId);

impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 1. Verify cookie signature via SignedCookieJar.
        let jar = SignedCookieJar::<Key>::from_headers(&parts.headers, state.cookie_key.clone());
        let Some(cookie) = jar.get(SESSION_COOKIE) else {
            return Err(Error::Unauthorized);
        };
        let session_id = cookie.value().to_string();

        // 2. Look the row up. The query uses `expires_at > now()` so an
        //    expired row is treated as if it doesn't exist.
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT user_id FROM sessions \
             WHERE id = ? AND expires_at > datetime('now')",
        )
        .bind(&session_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| Error::Internal(e.into()))?;

        match row {
            Some((user_id,)) => Ok(AuthenticatedUser(UserId(user_id))),
            None => Err(Error::Unauthorized),
        }
    }
}

/// Generate a fresh random session id (128 bits, hex-encoded → 32 chars).
pub fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Insert a fresh session row for `user_id`. Returns the session id.
pub async fn create_session(pool: &SqlitePool, user_id: &str) -> Result<String, Error> {
    let id = new_session_id();
    let expires_at = OffsetDateTime::now_utc() + SESSION_TTL;
    // SQLite's `datetime('now')` is UTC seconds without timezone suffix.
    // Format to match (`%Y-%m-%d %H:%M:%S`) so the comparator above sees a
    // value of the same shape.
    let expires_str = format_sqlite_datetime(expires_at);

    sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(user_id)
        .bind(&expires_str)
        .execute(pool)
        .await
        .map_err(|e| Error::Internal(e.into()))?;

    Ok(id)
}

/// Delete a session row (logout). Idempotent — a missing row is a no-op.
pub async fn delete_session(pool: &SqlitePool, session_id: &str) -> Result<(), Error> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(|e| Error::Internal(e.into()))?;
    Ok(())
}

/// Construct the session cookie the response will set after a successful
/// login. `secure` is true iff the public URL is HTTPS (§10.1).
pub fn build_session_cookie(session_id: String, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(SESSION_COOKIE, session_id);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_secure(secure);
    c.set_path("/");
    c.set_max_age(SESSION_TTL);
    c
}

/// Construct the short-lived flow cookie that stashes (state, nonce, pkce)
/// across the IdP redirect (§10.1).
pub fn build_flow_cookie(value: String, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(FLOW_COOKIE, value);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_secure(secure);
    c.set_path("/api/auth/");
    c.set_max_age(FLOW_TTL);
    c
}

/// Build a cookie that clears a previously-set cookie of the same name.
pub fn clear_cookie(name: &'static str) -> Cookie<'static> {
    let mut c = Cookie::from(name);
    c.set_path("/");
    c.set_max_age(time::Duration::ZERO);
    c
}

/// SQLite `datetime('now')` returns values like `"2026-05-14 11:23:30"` (UTC,
/// no tz suffix). Format an `OffsetDateTime` to match so direct text
/// comparisons in WHERE clauses behave correctly.
fn format_sqlite_datetime(t: OffsetDateTime) -> String {
    let utc = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        utc.year(),
        utc.month() as u8,
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second()
    )
}

impl FromStr for UserId {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(UserId(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_unique_and_hex() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b, "session ids must be random");
        assert_eq!(a.len(), 32, "16 random bytes → 32 hex chars");
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sqlite_datetime_format_matches_sqlite_shape() {
        // 2026-05-14T07:08:09Z
        let t = OffsetDateTime::from_unix_timestamp(1_778_742_489).unwrap();
        assert_eq!(format_sqlite_datetime(t), "2026-05-14 07:08:09");
    }

    #[tokio::test]
    async fn create_and_delete_session_round_trip() {
        let pool = crate::db::connect(":memory:").await.unwrap();
        sqlx::query("INSERT INTO users (id, email) VALUES ('u', 'u@example.com')")
            .execute(&pool)
            .await
            .unwrap();

        let id = create_session(&pool, "u").await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 1);

        delete_session(&pool, &id).await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 0);
    }
}
