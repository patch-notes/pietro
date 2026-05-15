//! Router assembly (§7).
//!
//! Routes mounted today:
//!   * `GET /healthz`               — liveness probe; pings the DB pool.
//!   * `GET /api/auth/login`        — start OIDC flow.
//!   * `GET /api/auth/callback`     — handle IdP callback.
//!   * `POST /api/auth/logout`      — clear session.
//!   * `GET /api/me`                — session-guarded; returns user info.
//!   * `GET /api/services`          — list configured upstreams (no secrets).
//!   * `GET /api/keys`              — list current user's keys.
//!   * `POST /api/keys`             — mint a key; plaintext returned once.
//!   * `DELETE /api/keys/:key_id`   — soft-revoke.
//!   * `GET /assets/{*path}`        — embedded SPA static files (M7).
//!   * fallback                     — embedded SPA `index.html` for any other
//!     path; JSON 404 under `/api/` and `/proxy/` (§7).
//!
//! Per §7 the entire app is one flat `axum::Router` — no nested apps.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{FromRef, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum_extra::extract::cookie::Key;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::warn;

use crate::auth::oidc::{self, OidcState};
use crate::auth::session::{AuthenticatedUser, UserId};
use crate::config::Config;
use crate::errors::Error;
use crate::keys::{self, KeyId, KeyRecord};

/// Application state shared across handlers. Cheap to clone (`Arc` for config
/// and OIDC state, sqlx pool which is reference-counted internally, and a
/// cookie `Key` which is a few bytes that `cookie` itself wraps in an Arc).
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: SqlitePool,
    pub cookie_key: Key,
    /// Whether to set the `Secure` flag on cookies (true iff `public_url` is https).
    pub cookie_secure: bool,
    pub oidc: Arc<OidcState>,
    /// API-key pepper, decoded from `cfg.api_key_pepper` once at startup.
    /// Held as `Arc<Vec<u8>>` so cloning the state stays cheap.
    pub pepper: Arc<Vec<u8>>,
    /// HTTP client used by the proxy forwarder (M5). Separate from the
    /// OIDC client because the two have different timeout + redirect
    /// policies.
    pub proxy_client: reqwest::Client,
    /// Shared usage batcher consumed by the proxy hot path.
    pub proxy_usage: Arc<crate::proxy::UsageBatcher>,
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
        .route("/api/services", get(list_services))
        .route("/api/keys", get(list_keys).post(mint_key))
        .route("/api/keys/{key_id}", delete(revoke_key))
        .route("/api/auth/login", get(oidc::login))
        .route("/api/auth/callback", get(oidc::callback))
        .route("/api/auth/logout", post(oidc::logout))
        // The proxy catches every method via `any`. Three routes share two
        // handlers: bare `/proxy/{service_id}` and `/proxy/{service_id}/`
        // both call `forward_bare` (empty tail → upstream root); the
        // wildcard `{*path}` requires ≥1 segment after the slash and goes
        // through `forward`.
        .route(
            "/proxy/{service_id}",
            axum::routing::any(crate::proxy::forward_bare),
        )
        .route(
            "/proxy/{service_id}/",
            axum::routing::any(crate::proxy::forward_bare),
        )
        .route(
            "/proxy/{service_id}/{*path}",
            axum::routing::any(crate::proxy::forward),
        )
        // M7: embedded SPA. Assets are content-addressed by Vite, so a miss
        // is a real miss (no fallback inside `/assets/`).
        .route("/assets/{*path}", get(crate::spa::serve_asset))
        // M7: everything else falls through to the SPA handler, which
        // serves `index.html` for history-mode routes and JSON-404s any
        // unmatched `/api/*` or `/proxy/*` path (§7).
        .fallback(crate::spa::fallback)
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

// -- /api/services -----------------------------------------------------------

#[derive(Serialize)]
struct ServiceListItem {
    id: String,
    display_name: String,
    description: Option<String>,
}

/// List the configured upstreams (§7). Returns id + human metadata only —
/// **never** the upstream credential or upstream URL. The URL is internal
/// implementation: callers don't need it because they talk to `/proxy/:svc`,
/// not to the upstream directly.
async fn list_services(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
) -> Json<Vec<ServiceListItem>> {
    let items = state
        .config
        .services
        .iter()
        .map(|s| ServiceListItem {
            id: s.id.as_str().to_string(),
            display_name: s.display_name.clone(),
            description: s.description.clone(),
        })
        .collect();
    Json(items)
}

// -- /api/keys ---------------------------------------------------------------

#[derive(Deserialize)]
struct MintKeyRequest {
    service_id: String,
    label: String,
}

/// What we return on `POST /api/keys`. `plaintext` is the *only* time this
/// value ever leaves the server — §11.2.
#[derive(Serialize)]
struct MintKeyResponse {
    key_id: String,
    /// **Show this once.** Calling clients should write it down or pipe it
    /// to whatever secret manager they use.
    plaintext: String,
    prefix: String,
    last4: String,
    service_id: String,
    label: String,
}

async fn list_keys(
    State(state): State<AppState>,
    AuthenticatedUser(UserId(user_id)): AuthenticatedUser,
) -> Result<Json<Vec<KeyRecord>>, Error> {
    let rows = keys::list_for_user(&state.pool, &user_id).await?;
    Ok(Json(rows))
}

async fn mint_key(
    State(state): State<AppState>,
    AuthenticatedUser(UserId(user_id)): AuthenticatedUser,
    Json(body): Json<MintKeyRequest>,
) -> Result<(StatusCode, Json<MintKeyResponse>), Error> {
    // Validate inputs at the boundary (§8 spirit, applied to API input too).
    if body.label.trim().is_empty() {
        return Err(Error::BadRequest("label must be non-empty"));
    }
    if body.label.len() > 128 {
        return Err(Error::BadRequest("label must be <= 128 chars"));
    }
    if !state
        .config
        .services
        .iter()
        .any(|s| s.id.as_str() == body.service_id)
    {
        return Err(Error::BadRequest("unknown service_id"));
    }

    let minted = keys::mint(
        &state.pool,
        &state.pepper,
        &user_id,
        &body.service_id,
        &body.label,
    )
    .await?;

    let resp = MintKeyResponse {
        key_id: minted.key_id.as_str().to_string(),
        plaintext: minted.plaintext.expose().to_string(),
        prefix: minted.prefix,
        last4: minted.last4,
        service_id: body.service_id,
        label: body.label,
    };
    Ok((StatusCode::CREATED, Json(resp)))
}

async fn revoke_key(
    State(state): State<AppState>,
    AuthenticatedUser(UserId(user_id)): AuthenticatedUser,
    Path(key_id): Path<String>,
) -> Result<StatusCode, Error> {
    let key_id = KeyId::parse(&key_id)?;
    let did_revoke = keys::revoke(&state.pool, &user_id, &key_id).await?;
    if did_revoke {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Error::NotFound)
    }
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
        let pepper = crate::config::decode_key_material(cfg.api_key_pepper.expose()).unwrap();

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
            pepper: Arc::new(pepper),
            proxy_client: reqwest::Client::new(),
            proxy_usage: Arc::new(crate::proxy::UsageBatcher::default()),
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
        .set_token_endpoint(Some(
            TokenUrl::new("http://idp.test/token".to_string()).unwrap(),
        ));
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
            .oneshot(
                Request::builder()
                    .uri("/api/me")
                    .body(Body::empty())
                    .unwrap(),
            )
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
        // Real OIDC providers (e.g. Nextcloud) return the issuer WITHOUT a
        // trailing slash in their discovery document. Our IssuerUrl now strips
        // the trailing slash before comparison, so the mock must match.
        let issuer = base.clone();
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
        let pepper = crate::config::decode_key_material(cfg.api_key_pepper.expose()).unwrap();
        let oidc = OidcState::from_config(&cfg).await.unwrap();
        let state = AppState {
            config: Arc::new(cfg),
            pool,
            cookie_key,
            cookie_secure: false,
            oidc: Arc::new(oidc),
            pepper: Arc::new(pepper),
            proxy_client: reqwest::Client::new(),
            proxy_usage: Arc::new(crate::proxy::UsageBatcher::default()),
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

    // --- M4: keys + services ---------------------------------------------

    /// Seed a user + active session, return (state, app, session cookie
    /// header value). Tests use the cookie to authenticate subsequent
    /// requests through the real `AuthenticatedUser` extractor — no test-
    /// only auth bypass.
    async fn authed_app() -> (AppState, axum::Router, String) {
        let state = dummy_state().await;

        // Seed a user and a session row directly. The extractor only
        // checks the session table; it doesn't care how the row got there.
        sqlx::query("INSERT INTO users (id, email) VALUES ('u-test', 'u@example.com')")
            .execute(&state.pool)
            .await
            .unwrap();
        let session_id = crate::auth::session::new_session_id();
        sqlx::query(
            "INSERT INTO sessions (id, user_id, expires_at) \
             VALUES (?, 'u-test', datetime('now', '+1 hour'))",
        )
        .bind(&session_id)
        .execute(&state.pool)
        .await
        .unwrap();

        // Sign the cookie the same way the production handler would.
        let mut jar = cookie::CookieJar::new();
        jar.signed_mut(&state.cookie_key).add(cookie::Cookie::new(
            crate::auth::session::SESSION_COOKIE,
            session_id,
        ));
        let cookie_header = jar
            .get(crate::auth::session::SESSION_COOKIE)
            .expect("signed jar must hold the cookie")
            .to_string();

        let app = build_router(state.clone());
        (state, app, cookie_header)
    }

    #[tokio::test]
    async fn services_lists_configured_ids_without_secrets() {
        let (_state, app, cookie) = authed_app().await;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/services")
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array(), "expected array, got {v}");
        assert_eq!(v[0]["id"], "openai");
        assert_eq!(v[0]["display_name"], "OpenAI");
        // Hard contract: never expose upstream URLs or credential material.
        assert!(v[0].get("upstream_url").is_none(), "leaked upstream_url");
        assert!(v[0].get("auth").is_none(), "leaked auth block");
        assert!(v[0].get("value").is_none(), "leaked credential value");
    }

    #[tokio::test]
    async fn keys_list_requires_session() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/keys")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mint_returns_plaintext_once_and_409_on_dup() {
        let (state, app, cookie) = authed_app().await;

        let mint = |service: &'static str, label: &'static str| {
            let app = app.clone();
            let cookie = cookie.clone();
            async move {
                let body = serde_json::json!({ "service_id": service, "label": label });
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/keys")
                        .header(axum::http::header::COOKIE, &cookie)
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        let resp = mint("openai", "laptop").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let plaintext = v["plaintext"].as_str().unwrap().to_string();
        assert!(plaintext.starts_with("pi_live_"));
        assert_eq!(v["service_id"], "openai");
        assert_eq!(v["label"], "laptop");
        // last4 must really be the last four chars of the plaintext.
        assert_eq!(
            v["last4"].as_str().unwrap(),
            &plaintext[plaintext.len() - 4..]
        );

        // Second mint for the same service: 409 with the contract code.
        let dup = mint("openai", "laptop-2").await;
        assert_eq!(dup.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(dup.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "conflict");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("key_already_exists"),
            "expected conflict reason in message: {v}"
        );

        // And the list endpoint shows exactly one key.
        let list = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/keys")
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let body = axum::body::to_bytes(list.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        // Listing must NEVER expose plaintext or hashes.
        assert!(v[0].get("plaintext").is_none());
        assert!(v[0].get("key_hash").is_none());
    }

    #[tokio::test]
    async fn mint_rejects_unknown_service_and_empty_label() {
        let (_state, app, cookie) = authed_app().await;

        let make = |json: serde_json::Value| {
            let app = app.clone();
            let cookie = cookie.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/keys")
                        .header(axum::http::header::COOKIE, &cookie)
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(json.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        let resp = make(serde_json::json!({"service_id": "ghost", "label": "x"})).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = make(serde_json::json!({"service_id": "openai", "label": ""})).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn revoke_then_remint_succeeds_and_revoke_404s() {
        let (state, _app, cookie) = authed_app().await;

        // Mint
        let app = build_router(state.clone());
        let body = serde_json::json!({"service_id": "openai", "label": "laptop"});
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys")
                    .header(axum::http::header::COOKIE, &cookie)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let key_id = v["key_id"].as_str().unwrap().to_string();

        // Revoke
        let app = build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/keys/{key_id}"))
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Revoking the same key again → 404 (idempotent at the verb level,
        // but we honestly report that the active row is gone).
        let app = build_router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/keys/{key_id}"))
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Re-mint for the same service: now works (the partial unique index
        // ignores revoked rows). End-to-end proof of the §11.2 contract.
        let app = build_router(state);
        let body = serde_json::json!({"service_id": "openai", "label": "fresh"});
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys")
                    .header(axum::http::header::COOKIE, &cookie)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    // --- M5: proxy ---------------------------------------------------------

    /// Spin up a wiremock server, point a one-service config at it, mint a
    /// real key via `keys::mint`, then drive the full forwarder through the
    /// router. This is the meaningful acceptance test: end-to-end auth →
    /// header injection → upstream call → response stream-back.
    async fn proxy_app_with_upstream(
        upstream_uri: &str,
        auth_block: &str,
    ) -> (AppState, axum::Router, String) {
        let yaml = format!(
            r#"
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
    upstream_url: "{upstream_uri}"
    auth:
{auth_block}
"#,
        );
        let cfg = Config::from_yaml_str(&yaml).unwrap();
        let pool = crate::db::connect(":memory:").await.unwrap();
        let key_bytes = crate::config::decode_key_material(cfg.cookie_key.expose()).unwrap();
        let cookie_key = Key::derive_from(&key_bytes);
        let pepper = crate::config::decode_key_material(cfg.api_key_pepper.expose()).unwrap();
        let oidc = Arc::new(OidcState {
            client: build_offline_oidc_client(),
            http: reqwest::Client::new(),
            scopes: cfg.oidc.scopes.clone(),
            allowed_email_domains: cfg.oidc.allowed_email_domains.clone(),
        });

        // Seed a user and mint a real key for them via the keys module.
        sqlx::query("INSERT INTO users (id, email) VALUES ('u', 'u@example.com')")
            .execute(&pool)
            .await
            .unwrap();
        let minted = crate::keys::mint(&pool, &pepper, "u", "openai", "laptop")
            .await
            .unwrap();
        let plaintext = minted.plaintext.expose().to_string();

        let state = AppState {
            config: Arc::new(cfg),
            pool,
            cookie_key,
            cookie_secure: false,
            oidc,
            pepper: Arc::new(pepper),
            proxy_client: reqwest::Client::new(),
            proxy_usage: Arc::new(crate::proxy::UsageBatcher::default()),
        };
        let app = build_router(state.clone());
        (state, app, plaintext)
    }

    /// Helpers to drive the proxy with `ConnectInfo` populated (axum's
    /// `Router::oneshot` doesn't carry a real peer otherwise).
    fn proxy_request(method: &str, path: &str, bearer: Option<&str>, body: Body) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = bearer {
            b = b.header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
        }
        // axum's `ConnectInfo<SocketAddr>` extractor reads from request
        // extensions; insert one so the handler sees a peer.
        let mut req = b.body(body).unwrap();
        req.extensions_mut()
            .insert(axum::extract::connect_info::ConnectInfo::<
                std::net::SocketAddr,
            >("203.0.113.7:55555".parse().unwrap()));
        req
    }

    #[tokio::test]
    async fn proxy_forwards_with_injected_bearer_and_strips_caller_header() {
        let upstream = wiremock::MockServer::start().await;
        let upstream_uri = upstream.uri();
        // Upstream demands the operator's bearer, not the caller's.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/echo"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer sk-OPERATOR",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .insert_header("x-upstream-marker", "alive")
                    .set_body_string(r#"{"hello":"world"}"#),
            )
            .mount(&upstream)
            .await;

        let auth = "      kind: bearer\n      value: \"sk-OPERATOR\"";
        let (_state, app, bearer) = proxy_app_with_upstream(&upstream_uri, auth).await;
        let resp = app
            .oneshot(proxy_request(
                "GET",
                "/proxy/openai/v1/echo",
                Some(&bearer),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Response headers passed through except hop-by-hop.
        assert_eq!(
            resp.headers()
                .get("x-upstream-marker")
                .unwrap()
                .to_str()
                .unwrap(),
            "alive"
        );
        // Body streamed through verbatim.
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"hello":"world"}"#);
    }

    #[tokio::test]
    async fn proxy_rejects_unknown_service_with_404() {
        let upstream = wiremock::MockServer::start().await;
        let auth = "      kind: bearer\n      value: \"sk-X\"";
        let (_s, app, bearer) = proxy_app_with_upstream(&upstream.uri(), auth).await;
        // /proxy/ghost/... → 404 because no such service is configured.
        let resp = app
            .oneshot(proxy_request(
                "GET",
                "/proxy/ghost/v1/x",
                Some(&bearer),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn proxy_missing_bearer_returns_401() {
        let upstream = wiremock::MockServer::start().await;
        let auth = "      kind: bearer\n      value: \"sk-X\"";
        let (_s, app, _bearer) = proxy_app_with_upstream(&upstream.uri(), auth).await;
        let resp = app
            .oneshot(proxy_request(
                "GET",
                "/proxy/openai/v1/x",
                None,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn proxy_revoked_key_returns_401() {
        let upstream = wiremock::MockServer::start().await;
        let auth = "      kind: bearer\n      value: \"sk-X\"";
        let (state, app, bearer) = proxy_app_with_upstream(&upstream.uri(), auth).await;
        // Revoke the freshly-minted key directly in the DB.
        sqlx::query("UPDATE api_keys SET revoked_at = datetime('now')")
            .execute(&state.pool)
            .await
            .unwrap();
        let resp = app
            .oneshot(proxy_request(
                "GET",
                "/proxy/openai/v1/x",
                Some(&bearer),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn proxy_propagates_upstream_status_and_body() {
        let upstream = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/echo"))
            .respond_with(wiremock::ResponseTemplate::new(418).set_body_string("teapot"))
            .mount(&upstream)
            .await;
        let auth = "      kind: bearer\n      value: \"sk-X\"";
        let (_s, app, bearer) = proxy_app_with_upstream(&upstream.uri(), auth).await;
        let resp = app
            .oneshot(proxy_request(
                "POST",
                "/proxy/openai/echo",
                Some(&bearer),
                Body::from("anything"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"teapot");
    }

    /// Bare-service URLs (no tail) must reach the upstream root. Reported
    /// in the field: callers naturally try `/proxy/<id>` and expect it to
    /// land on the upstream's `/`. The route must match, the auth check
    /// must run, and the upstream must see a request to `/`.
    #[tokio::test]
    async fn proxy_bare_service_url_hits_upstream_root() {
        let upstream = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("root!"))
            .mount(&upstream)
            .await;
        let auth = "      kind: bearer\n      value: \"sk-X\"";
        let (_s, app, bearer) = proxy_app_with_upstream(&upstream.uri(), auth).await;
        // No trailing slash, no tail — the form the user typed.
        let resp = app
            .clone()
            .oneshot(proxy_request(
                "GET",
                "/proxy/openai",
                Some(&bearer),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"root!");

        // Also accept the trailing-slash variant — same destination.
        let resp = app
            .oneshot(proxy_request(
                "GET",
                "/proxy/openai/",
                Some(&bearer),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn proxy_injects_query_param_auth() {
        let upstream = wiremock::MockServer::start().await;
        // Upstream requires `?api_key=OP_K`. The caller did not supply it.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/search"))
            .and(wiremock::matchers::query_param("api_key", "OP_K"))
            .and(wiremock::matchers::query_param("q", "hi"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("found"))
            .mount(&upstream)
            .await;
        let auth = "      kind: query\n      param: \"api_key\"\n      value: \"OP_K\"";
        let (_s, app, bearer) = proxy_app_with_upstream(&upstream.uri(), auth).await;
        let resp = app
            .oneshot(proxy_request(
                "GET",
                "/proxy/openai/search?q=hi",
                Some(&bearer),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"found");
    }

    // --- M7: SPA fallback + JSON-404 carve-out for /api/* and /proxy/* ----

    /// The fallback handler must answer with a 200 + HTML page for any
    /// unknown non-API path so react-router can pick up history-mode
    /// routes on the client. In a debug build (cargo test is always
    /// debug) this is the dev-mode notice; in release it would be the
    /// embedded `index.html`. Either way: 200 + `text/html`.
    #[tokio::test]
    async fn spa_fallback_serves_html_for_unknown_non_api_path() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/some/spa/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .expect("content-type")
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/html"), "expected text/html, got {ct}");
    }

    /// `/api/garbage` must return JSON-404 with the project error envelope —
    /// NOT the SPA. §7 contract: the SPA fallback never swallows `/api/*`.
    #[tokio::test]
    async fn unknown_api_path_returns_json_404() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/garbage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "not_found");
    }

    /// Same contract on the proxy side: `/proxy/anything` that doesn't
    /// match the real `/proxy/{service_id}/{*path}` route must JSON-404,
    /// not return the SPA. Otherwise a caller hitting a malformed proxy
    /// URL would see an HTML page and be very confused.
    #[tokio::test]
    async fn unknown_proxy_path_returns_json_404() {
        let app = build_router(dummy_state().await);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/proxy/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "not_found");
    }
}
