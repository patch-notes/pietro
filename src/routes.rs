//! Router assembly (§7).
//!
//! Routes mounted today:
//!   * `GET /healthz`  — liveness probe; also pings the DB pool (§16).
//!
//! As later milestones land they add their routes here. Per §7 the entire
//! app is one flat `axum::Router` — no nested apps.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use sqlx::SqlitePool;
use tracing::warn;

use crate::config::Config;

/// Application state shared across handlers. Cheap to clone (`Arc` + a sqlx
/// pool which is itself reference-counted internally).
#[derive(Clone)]
pub struct AppState {
    #[allow(
        dead_code,
        reason = "consumed by M3+ handlers (OIDC, services, proxy)"
    )]
    pub config: Arc<Config>,
    pub pool: SqlitePool,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn dummy_state() -> AppState {
        let yaml = r#"
listen: "0.0.0.0:8080"
public_url: "http://localhost:8080"
database_path: ":memory:"
cookie_key: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
api_key_pepper: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
oidc:
  issuer_url: "http://localhost:9000"
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
        AppState {
            config: Arc::new(cfg),
            pool,
        }
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
}
