//! Router assembly (§7).
//!
//! For M1 only `/healthz` is mounted. As later milestones land they will add
//! their routes here. Per §7 the entire app is one flat `axum::Router` — no
//! nested apps.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use crate::config::Config;

/// Application state shared across handlers. Cheap to clone (`Arc`).
#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code)] // wired into handlers from M2 onward
    pub config: Arc<Config>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
}

/// Liveness probe (§7, §16). For M1 we just return "ok"; once the DB pool
/// exists (M2) this will also `SELECT 1` against it.
async fn healthz() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn dummy_state() -> AppState {
        // We don't actually exercise the config in /healthz today, but the
        // router builder takes it — give it something plausibly shaped.
        // Building a real `Config` here would re-implement test fixtures from
        // the config module, so we cheat with a deserialised one.
        let yaml = r#"
listen: "0.0.0.0:8080"
public_url: "http://localhost:8080"
database_path: "./pietro.db"
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
        AppState {
            config: Arc::new(cfg),
        }
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = build_router(dummy_state());
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"ok");
    }
}
