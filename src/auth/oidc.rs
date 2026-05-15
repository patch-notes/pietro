//! OIDC login + callback + logout (§10).
//!
//! Flow at a glance:
//!
//! ```text
//! GET /api/auth/login
//!   - mint (state, nonce, PKCE)
//!   - stash them in a short-lived signed `pietro_flow` cookie
//!   - 302 to the IdP's authorize URL
//!
//! GET /api/auth/callback?code=…&state=…
//!   - verify state matches the flow cookie
//!   - exchange code (with PKCE verifier) for tokens
//!   - verify ID token (signature, iss, aud, nonce, exp)
//!   - enforce allowed_email_domains
//!   - upsert users row, insert sessions row, set pietro_session cookie
//!   - 302 to /
//!
//! POST /api/auth/logout
//!   - delete sessions row
//!   - clear pietro_session cookie
//!   - 204
//! ```
//!
//! ID token verification uses `nonce` (replay defence) and the upstream JWKS
//! (signature). The `openidconnect` crate handles `iss`, `aud`, and `exp`.

use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use axum_extra::extract::cookie::SignedCookieJar;
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreProviderMetadata, CoreResponseType,
};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use openidconnect::{EndpointMaybeSet, EndpointNotSet, EndpointSet};
use reqwest::redirect;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use url::Url;

use crate::auth::session::{
    self, FLOW_COOKIE, SESSION_COOKIE, build_flow_cookie, build_session_cookie, clear_cookie,
};
use crate::config::Config;
use crate::errors::Error;
use crate::routes::AppState;

/// Concrete `openidconnect` client type. Once we discover provider metadata
/// and bind a redirect URI, the type parameters track that.
pub type PietroOidcClient = CoreClient<
    EndpointSet,      // AuthUrl
    EndpointNotSet,   // DeviceAuthUrl
    EndpointNotSet,   // IntrospectionUrl
    EndpointNotSet,   // RevocationUrl
    EndpointMaybeSet, // UserInfoUrl
    EndpointMaybeSet, // TokenUrl
>;

/// HTTP client used for OIDC discovery + token exchange. Per the
/// openidconnect SSRF guidance, redirects are disabled.
pub struct OidcHttpClient(pub reqwest::Client);

impl OidcHttpClient {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .redirect(redirect::Policy::none())
            .build()
            .context("building reqwest client for OIDC")?;
        Ok(OidcHttpClient(client))
    }
}

/// Wrapper holding the discovered client and the redirect URI. Built once at
/// startup so handlers don't pay discovery cost per request.
pub struct OidcState {
    pub client: PietroOidcClient,
    pub http: reqwest::Client,
    pub scopes: Vec<String>,
    pub allowed_email_domains: Vec<String>,
}

impl OidcState {
    /// Build the OIDC state at startup: discover provider metadata, configure
    /// the client, capture the policy bits from config.
    pub async fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let http = OidcHttpClient::new()?.0;
        // OIDC requires an exact string match between our issuer and the
        // discovery document's `issuer` field. `IssuerUrl::from_url()` normalises
        // the URL (adding a trailing slash), so we strip it from the raw string
        // and use `IssuerUrl::new()` which preserves the original representation.
        let issuer_str = cfg
            .oidc
            .issuer_url
            .as_str()
            .trim_end_matches('/')
            .to_string();
        let issuer_url = IssuerUrl::new(issuer_str)
            .context("invalid oidc.issuer_url (cannot create IssuerUrl)")?;
        let metadata = CoreProviderMetadata::discover_async(issuer_url, &http)
            .await
            .context("OIDC discovery failed")?;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(cfg.oidc.client_id.clone()),
            Some(ClientSecret::new(cfg.oidc.client_secret.expose().clone())),
        )
        .set_redirect_uri(
            RedirectUrl::new(callback_url(&cfg.public_url))
                .context("building OIDC redirect_uri")?,
        );
        Ok(OidcState {
            client,
            http,
            scopes: cfg.oidc.scopes.clone(),
            allowed_email_domains: cfg.oidc.allowed_email_domains.clone(),
        })
    }
}

/// Build the URL the IdP redirects back to after authn — must match exactly
/// what's registered at the IdP.
pub fn callback_url(public_url: &Url) -> String {
    let mut u = public_url.clone();
    // `Url::join` is the safe way; we re-base on `/api/auth/callback`.
    if !u.path().ends_with('/') {
        // Ensure trailing slash so `.join` replaces the path, not the last segment.
        let with_slash = format!("{}/", u.path());
        u.set_path(&with_slash);
    }
    u.join("api/auth/callback")
        .expect("static suffix")
        .to_string()
}

// -- /api/auth/login ---------------------------------------------------------

/// What we stash in the signed flow cookie across the IdP redirect.
#[derive(Debug, Serialize, Deserialize)]
struct FlowState {
    csrf: String,
    nonce: String,
    pkce_verifier: String,
}

pub async fn login(State(state): State<AppState>, jar: SignedCookieJar) -> impl IntoResponse {
    let oidc = &state.oidc;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    // Build the authorize URL. CsrfToken + Nonce are random; we capture both
    // values to verify them back from the callback.
    let mut auth_req = oidc.client.authorize_url(
        CoreAuthenticationFlow::AuthorizationCode,
        CsrfToken::new_random,
        Nonce::new_random,
    );
    // `from_provider_metadata` configures the client with `use_openid_scope = true`,
    // which adds "openid" automatically. We only add the operator-configured
    // extras here to avoid emitting "openid" twice in the scope parameter.
    for s in &oidc.scopes {
        auth_req = auth_req.add_scope(Scope::new(s.clone()));
    }
    let (auth_url, csrf, nonce) = auth_req.set_pkce_challenge(pkce_challenge).url();

    let flow = FlowState {
        csrf: csrf.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: pkce_verifier.secret().clone(),
    };
    let cookie_value = match serde_json::to_string(&flow) {
        Ok(s) => s,
        Err(err) => {
            warn!(error = %err, "serializing flow cookie");
            return Error::Internal(err.into()).into_response();
        }
    };
    let jar = jar.add(build_flow_cookie(cookie_value, state.cookie_secure));

    (jar, Redirect::to(auth_url.as_str())).into_response()
}

// -- /api/auth/callback ------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[allow(dead_code, reason = "carried by some IdPs; we log it if present")]
    error_description: Option<String>,
}

pub async fn callback(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(q): Query<CallbackQuery>,
) -> Result<axum::response::Response, Error> {
    // 1. Bail out fast if the IdP told us something went wrong.
    if let Some(err) = q.error {
        warn!(error = %err, "OIDC callback returned error from IdP");
        return Err(Error::Unauthorized);
    }
    let code = q.code.ok_or(Error::BadRequest("missing code"))?;
    let state_param = q.state.ok_or(Error::BadRequest("missing state"))?;

    // 2. Recover the flow cookie. Missing or unsigned → request was forged
    //    or stale → 400.
    let flow_cookie = jar
        .get(FLOW_COOKIE)
        .ok_or(Error::BadRequest("flow cookie missing"))?;
    let flow: FlowState = serde_json::from_str(flow_cookie.value())
        .map_err(|_| Error::BadRequest("flow cookie malformed"))?;

    // 3. CSRF: state from the IdP must match what we put in the flow cookie.
    if !constant_time_eq(state_param.as_bytes(), flow.csrf.as_bytes()) {
        warn!("OIDC callback state mismatch");
        return Err(Error::Unauthorized);
    }

    // 4. Exchange the auth code for tokens (uses the PKCE verifier).
    let token_response = state
        .oidc
        .client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| Error::Internal(anyhow::anyhow!("exchange_code setup failed: {e}")))?
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier))
        .request_async(&state.oidc.http)
        .await
        .map_err(|e| {
            warn!(error = %e, "OIDC token exchange failed");
            Error::Unauthorized
        })?;

    // 5. Verify the ID token.
    let id_token = token_response
        .id_token()
        .ok_or(Error::Unauthorized)
        .inspect_err(|_| warn!("OIDC token response missing id_token"))?;
    let id_token_verifier = state.oidc.client.id_token_verifier();
    let nonce = Nonce::new(flow.nonce);
    let claims = id_token.claims(&id_token_verifier, &nonce).map_err(|e| {
        warn!(error = %e, "ID token verification failed");
        Error::Unauthorized
    })?;

    // 6. Enforce email allowlist (Q3 resolved: email-only).
    let email = claims
        .email()
        .ok_or(Error::Unauthorized)
        .inspect_err(|_| warn!("ID token has no email claim"))?
        .as_str()
        .to_string();
    if !email_is_allowed(&email, &state.oidc.allowed_email_domains) {
        warn!(email = %redact_email(&email), "email domain not in allowlist");
        return Err(Error::Forbidden);
    }

    // 7. Upsert the user.
    let sub = claims.subject().as_str().to_string();
    let display_name = claims
        .name()
        .and_then(|m| m.get(None).map(|v| v.as_str().to_string()));

    upsert_user(&state.pool, &sub, &email, display_name.as_deref()).await?;

    // 8. Mint a session, set the cookie, drop the flow cookie, redirect home.
    let session_id = session::create_session(&state.pool, &sub).await?;
    let jar = jar
        .remove(axum_extra::extract::cookie::Cookie::from(FLOW_COOKIE))
        .add(build_session_cookie(session_id, state.cookie_secure));

    info!(user_id = %sub, "OIDC login complete");
    Ok((jar, Redirect::to("/")).into_response())
}

async fn upsert_user(
    pool: &sqlx::SqlitePool,
    sub: &str,
    email: &str,
    display_name: Option<&str>,
) -> Result<(), Error> {
    // Upsert keeps `last_seen_at` fresh on every login while preserving the
    // original `created_at`. `display_name` overwrites only when supplied.
    sqlx::query(
        "INSERT INTO users (id, email, display_name) VALUES (?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
             email = excluded.email, \
             display_name = COALESCE(excluded.display_name, users.display_name), \
             last_seen_at = datetime('now')",
    )
    .bind(sub)
    .bind(email)
    .bind(display_name)
    .execute(pool)
    .await
    .map_err(|e| Error::Internal(e.into()))?;
    Ok(())
}

// -- /api/auth/logout --------------------------------------------------------

pub async fn logout(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<axum::response::Response, Error> {
    if let Some(c) = jar.get(SESSION_COOKIE) {
        session::delete_session(&state.pool, c.value()).await?;
    }
    let jar = jar.remove(axum_extra::extract::cookie::Cookie::from(SESSION_COOKIE));
    // Belt-and-braces: also stamp out the cookie at the response layer for
    // any browser that doesn't honor the jar removal directive.
    let mut response = axum::http::Response::builder()
        .status(axum::http::StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .expect("static response");
    let clear = clear_cookie(SESSION_COOKIE);
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        clear.to_string().parse().expect("ascii cookie value"),
    );
    Ok((jar, response).into_response())
}

// -- helpers -----------------------------------------------------------------

fn email_is_allowed(email: &str, allowed_domains: &[String]) -> bool {
    if allowed_domains.is_empty() {
        return true;
    }
    let Some((_, domain)) = email.rsplit_once('@') else {
        return false;
    };
    let domain = domain.to_ascii_lowercase();
    allowed_domains
        .iter()
        .any(|d| d.eq_ignore_ascii_case(&domain))
}

/// Redact the local-part for log lines. We log domain-only.
fn redact_email(email: &str) -> String {
    match email.rsplit_once('@') {
        Some((_, domain)) => format!("***@{domain}"),
        None => "***".to_string(),
    }
}

/// Constant-time byte comparison so the state check doesn't leak a timing
/// side-channel. (For ~32-byte states this is theoretical, but cheap.)
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// Suppress dead-code warning on the `Arc` import; reserved for future use
// when handlers need to clone OidcState behind an Arc explicitly.
#[allow(dead_code)]
fn _arc_marker() -> Option<Arc<()>> {
    None
}

// Silence the unused `CoreResponseType` import on toolchains that don't
// inline it in the `authorize_url` builder. (openidconnect re-exports it
// through the type parameters, but the explicit import documents intent.)
const _: Option<CoreResponseType> = None;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_allowlist_empty_lets_anything_through() {
        assert!(email_is_allowed("anyone@anywhere.tld", &[]));
    }

    #[test]
    fn email_allowlist_matches_case_insensitively() {
        let allow = vec!["Example.com".to_string()];
        assert!(email_is_allowed("user@example.com", &allow));
        assert!(email_is_allowed("user@EXAMPLE.COM", &allow));
    }

    #[test]
    fn email_allowlist_rejects_others() {
        let allow = vec!["example.com".to_string()];
        assert!(!email_is_allowed("user@other.org", &allow));
        assert!(!email_is_allowed("no-at-sign", &allow));
    }

    #[test]
    fn callback_url_handles_trailing_slash_and_path() {
        let base = Url::parse("https://pietro.example.com").unwrap();
        assert_eq!(
            callback_url(&base),
            "https://pietro.example.com/api/auth/callback"
        );

        let with_slash = Url::parse("https://pietro.example.com/").unwrap();
        assert_eq!(
            callback_url(&with_slash),
            "https://pietro.example.com/api/auth/callback"
        );

        let sub = Url::parse("https://example.com/pietro").unwrap();
        assert_eq!(
            callback_url(&sub),
            "https://example.com/pietro/api/auth/callback"
        );
    }

    #[test]
    fn constant_time_eq_actually_compares() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hello!"));
    }

    #[test]
    fn redact_email_keeps_domain() {
        assert_eq!(redact_email("alice@example.com"), "***@example.com");
        assert_eq!(redact_email("garbage"), "***");
    }
}
