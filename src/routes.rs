//! Router assembly (§7).
//!
//! Routes mounted today:
//!   * `GET /healthz`               — liveness probe; pings the DB pool.
//!   * `GET /api/auth/login`        — start OIDC flow.
//!   * `GET /api/auth/callback`     — handle IdP callback.
//!   * `POST /api/auth/logout`      — clear session.
//!   * `GET /api/me`                — session-guarded; returns user info.
//!
//! Per §7 the entire app is one flat `axum::Router` — no nested apps.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum_extra::extract::cookie::Key;
use serde::Serialize;
use sqlx::SqlitePool;
use tracing::warn;

use crate::auth::oidc::{self, OidcState};
use crate::auth::session::{AuthenticatedUser, UserId};
use crate::config::Config;
use crate::errors::Error;

/// Application state shared across handlers. Cheap to clone (`Arc` for config
/// and OIDC state, sqlx pool which is reference-counted internally, and a
/// cookie `Key` which is a few bytes that `cookie` itself wraps in an Arc).
#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code, reason = "consumed by /api/services in M4")]
    pub config: Arc<Config>,
    pub pool: SqlitePool,
    pub cookie_key: Key,
    /// Whether to set the `Secure` flag on cookies (true iff `public_url` is https).
    pub cookie_secure: bool,
    pub oidc: Arc<OidcState>,
}

// Lets axum-extra extract the signing key from AppState (see
// `SignedCookieJar` docs).
impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/me", get(me))
        .route("/api/auth/login", get(oidc::login))
        .route("/api/auth/callback", get(oidc::callback))
        .route("/api/auth/logout", post(oidc::logout))
        .with_state(state)
}

/// Liveness probe (§7, §16). Returns 200 "ok" if the DB pool can serve a
/// `SELECT 1`. If the DB is down, returns 503 — a load balancer that watches
/// `/healthz` will then route away from this instance.
async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (StatusCode::OK, "ok"),
        Err(err) => {
            warn!(error = %err, "healthz: DB ping failed");
            (StatusCode::SERVICE_UNAVAILABLE, "db unavailable")
        }
    }
}

#[derive(Serialize)]
struct MeResponse {
    user_id: String,
    email: String,
    display_name: Option<String>,
}

/// Session-guarded user info (§7).
async fn me(
    State(state): State<AppState>,
    AuthenticatedUser(UserId(user_id)): AuthenticatedUser,
) -> Result<Json<MeResponse>, Error> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT email, display_name FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| Error::Internal(e.into()))?;
    let (email, display_name) = row.ok_or(Error::Unauthorized)?;
    Ok(Json(MeResponse {
        user_id,
        email,
        display_name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Build an `AppState` for tests. The OIDC state is *not* real — we never
    /// discover against the IdP — but it has the right shape so handlers that
    /// don't touch the network can run.
    pub(crate) async fn dummy_state() -> AppState {
        let yaml = r#"
listen: "0.0.0.0:8080"
public_url: "http://localhost:8080"
database_path: ":memory:"
cookie_key: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
api_key_pepper: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
oidc:
  issuer_url: "http://localhost:9999"
  client_id: "pietro"
  client_secret: "shhh"
services:
  - id: "openai"
    display_name: "OpenAI"
    upstream_url: "https://api.openai.com"
    auth:
      kind: bearer
      value: "sk-test"
"#;
        let cfg = Config::from_yaml_str(yaml).unwrap();
        let pool = crate::db::connect(":memory:").await.unwrap();
        let key_bytes = crate::config::decode_key_material(cfg.cookie_key.expose()).unwrap();
        let cookie_key = Key::derive_from(&key_bytes);

        let oidc = Arc::new(OidcState {
            client: build_offline_oidc_client(),
            http: reqwest::Client::new(),
            scopes: cfg.oidc.scopes.clone(),
            allowed_email_domains: cfg.oidc.allowed_email_domains.clone(),
        });

        AppState {
            config: Arc::new(cfg),
            pool,
            cookie_key,
            cookie_secure: false,
            oidc,
        }
    }

    /// Construct a CoreClient via `from_provider_metadata` with hand-built
    /// metadata — for tests that don't dial the IdP (e.g. /api/me,
    /// /api/auth/logout). This is the only way to get the same concrete type
    /// as `OidcState::from_config` produces without performing discovery.
    fn build_offline_oidc_client() -> crate::auth::oidc::PietroOidcClient {
        use openidconnect::core::{
            CoreClient, CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreResponseType,
            CoreSubjectIdentifierType,
        };
        use openidconnect::{
            AuthUrl, ClientId, ClientSecret, EmptyAdditionalProviderMetadata, IssuerUrl,
            JsonWebKeySetUrl, ResponseTypes, TokenUrl,
        };

        let metadata = CoreProviderMetadata::new(
            IssuerUrl::new("http://idp.test".to_string()).unwrap(),
            AuthUrl::new("http://idp.test/authorize".to_string()).unwrap(),
            JsonWebKeySetUrl::new("http://idp.test/jwks".to_string()).unwrap(),
            vec![ResponseTypes::new(vec![CoreResponseType::Code])],
            vec![CoreSubjectIdentifierType::Public],
            vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
            EmptyAdditionalProviderMetadata {},
        )
        .set_token_endpoint(Some(TokenUrl::new("http://idp.test/token".to_string()).unwrap()));
        CoreClient::from_provider_metadata(
            metadata,
            ClientId::new("pietro".into()),
            Some(ClientSecret::new("shhh".into())),
        )
    }

    #[tokio::test]
    async fn healthz_returns_ok_when_db_is_up() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn me_without_session_returns_401_json() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(Request::builder().uri("/api/me").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "unauthorized");
    }

    /// Full flow test: start a wiremock OIDC issuer, run real discovery,
    /// then hit /api/auth/login and verify the redirect target + query
    /// parameters + Set-Cookie. This is the cheapest test that proves
    /// `OidcState::from_config` works against a server-shaped surface and
    /// that the login handler builds a spec-compliant authorize URL.
    #[tokio::test]
    async fn login_redirects_to_idp_with_pkce_and_state() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let idp = MockServer::start().await;
        let base = idp.uri();
        // The openidconnect crate normalises the IssuerUrl to include a
        // trailing slash; the `issuer` claim returned by discovery must match
        // *exactly*. Mirror the normalisation here so the test passes.
        let issuer = format!("{base}/");
        let discovery = serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "jwks_uri": format!("{base}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "scopes_supported": ["openid", "profile", "email"],
        });
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
            .mount(&idp)
            .await;
        // discover_async ALSO fetches the JWKS after the config doc. Serve
        // an empty key set — we don't verify any signatures in this test.
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "keys": [] })),
            )
            .mount(&idp)
            .await;

        // Build AppState using *real* OIDC discovery against the mock.
        let yaml = format!(
            r#"
listen: "0.0.0.0:8080"
public_url: "http://localhost:8080"
database_path: ":memory:"
cookie_key: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
api_key_pepper: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
oidc:
  issuer_url: "{base}"
  client_id: "pietro"
  client_secret: "shhh"
  scopes: ["profile", "email"]
services:
  - id: "openai"
    display_name: "OpenAI"
    upstream_url: "https://api.openai.com"
    auth:
      kind: bearer
      value: "sk-test"
"#
        );
        let cfg = Config::from_yaml_str(&yaml).unwrap();
        let pool = crate::db::connect(":memory:").await.unwrap();
        let key_bytes = crate::config::decode_key_material(cfg.cookie_key.expose()).unwrap();
        let cookie_key = Key::derive_from(&key_bytes);
        let oidc = OidcState::from_config(&cfg).await.unwrap();
        let state = AppState {
            config: Arc::new(cfg),
            pool,
            cookie_key,
            cookie_secure: false,
            oidc: Arc::new(oidc),
        };
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);

        let location = resp
            .headers()
            .get(axum::http::header::LOCATION)
            .expect("redirect target")
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            location.starts_with(&format!("{base}/authorize")),
            "expected redirect to {base}/authorize, got: {location}"
        );

        let url = url::Url::parse(&location).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert!(q.contains_key("state"), "missing state: {q:?}");
        assert!(q.contains_key("nonce"), "missing nonce: {q:?}");
        assert!(q.contains_key("code_challenge"), "missing PKCE: {q:?}");
        assert_eq!(
            q.get("code_challenge_method").map(|s| s.as_str()),
            Some("S256")
        );
        assert_eq!(q.get("client_id").map(|s| s.as_str()), Some("pietro"));
        assert_eq!(
            q.get("redirect_uri").map(|s| s.as_str()),
            Some("http://localhost:8080/api/auth/callback")
        );
        let scope = q.get("scope").cloned().unwrap_or_default();
        assert!(
            scope.split(' ').any(|s| s == "openid"),
            "scope missing openid: {scope}"
        );

        // The handler must emit a flow cookie (`pietro_flow`) carrying the
        // stashed state/nonce/PKCE verifier; we don't decrypt it here, just
        // verify it was sent.
        let cookies: Vec<String> = resp
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert!(
            cookies.iter().any(|c| c.contains("pietro_flow")),
            "missing pietro_flow cookie: {cookies:?}"
        );
    }

    #[tokio::test]
    async fn logout_returns_204_and_clears_cookie() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/logout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let cookies: Vec<String> = resp
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert!(
            cookies.iter().any(|c| c.contains("pietro_session")),
            "logout should clear pietro_session: {cookies:?}"
        );
    }
}
