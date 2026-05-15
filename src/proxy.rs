//! The forwarder (§12).
//!
//! One handler: `forward`. Receives `ANY /proxy/{service_id}/{*path}`,
//! authenticates the caller, picks the upstream from `AppState.config`,
//! builds a streaming reqwest request with hop-by-hop headers stripped and
//! the operator credential injected, and streams the response back.
//!
//! Two auxiliary pieces:
//!   * [`UsageBatcher`] — in-memory `HashMap<KeyId, SystemTime>` of last-used
//!     stamps; the background [`run_usage_flusher`] task drains it to the DB
//!     every 30 s (and on shutdown).
//!   * [`build_client`]   — `reqwest::Client` configured with a 60 s default
//!     timeout. Each forwarded request can use a different timeout if we ever
//!     add per-service timeouts in v2.
//!
//! Streaming everywhere: a 10 GB upload through Pietro does not OOM Pietro
//! (§12 "What we do NOT do").

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use futures_util::TryStreamExt;
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::{Service, ServiceAuth};
use crate::errors::Error;
use crate::keys;
use crate::routes::AppState;

/// Default per-request upstream timeout. Configurable per service in v2 if
/// we ever need to, but a single ceiling is enough for v1.
const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the background task drains the usage batcher to the DB.
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Cookie name we always strip from forwarded requests so the operator's
/// upstream never sees Pietro's session cookie. Kept in sync with
/// `auth::session::SESSION_COOKIE`.
const SESSION_COOKIE: &str = "pietro_session";

// -- usage batcher -----------------------------------------------------------

/// Buffer of "this key was used at this time" intents. Each `verify` writes
/// one entry; the background loop flushes them all in a single `UPDATE` per
/// key. Keeps the proxy hot path off the SQLite write lock.
#[derive(Default)]
pub struct UsageBatcher {
    inner: Mutex<HashMap<String, SystemTime>>,
}

impl UsageBatcher {
    /// Record an intent. Last-writer-wins per key, which is what we want —
    /// only the most recent use matters for the UI.
    pub async fn touch(&self, key_id: &str) {
        let mut g = self.inner.lock().await;
        g.insert(key_id.to_string(), SystemTime::now());
    }

    /// Drain the buffer and stamp every entry into `api_keys.last_used_at`.
    /// Errors are logged at warn level — failing here doesn't fail the
    /// request that already returned.
    pub async fn flush(&self, pool: &SqlitePool) {
        let drained: Vec<(String, SystemTime)> = {
            let mut g = self.inner.lock().await;
            g.drain().collect()
        };
        if drained.is_empty() {
            return;
        }
        for (key_id, when) in drained {
            if let Err(err) = keys::mark_used(pool, &key_id, when).await {
                warn!(error = %err, key_id, "usage flush failed for one key");
            }
        }
    }
}

/// Background loop: flush the batcher every 30 s. Returns when the cancel
/// future resolves, draining once more on the way out.
pub async fn run_usage_flusher<F>(batcher: Arc<UsageBatcher>, pool: SqlitePool, cancel: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::pin!(cancel);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(USAGE_FLUSH_INTERVAL) => {
                batcher.flush(&pool).await;
            }
            _ = &mut cancel => {
                info!("usage flusher draining on shutdown");
                batcher.flush(&pool).await;
                return;
            }
        }
    }
}

// -- reqwest client builder --------------------------------------------------

/// Build the proxy's HTTP client. Distinct from the OIDC client because:
///   * we *do* want to follow redirects here? No — actually we don't; an
///     upstream `Location:` should pass through to the caller unchanged.
///   * we want a per-request timeout, not a per-connection one.
pub fn build_client() -> anyhow::Result<reqwest::Client> {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DEFAULT_UPSTREAM_TIMEOUT)
        // Allow HTTP/2 upgrades but don't *require* them — some upstreams
        // are still HTTP/1.1 only.
        .build()
        .context("building reqwest client for proxy")
}

// -- hop-by-hop header set ---------------------------------------------------

/// Names that must be stripped on both directions (RFC 7230 §6.1).
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Collect any extra names listed in the inbound `Connection:` header. Per
/// RFC 7230 §6.1, every value in the comma-separated list is itself a
/// hop-by-hop header that should not propagate.
fn connection_named(headers: &HeaderMap) -> Vec<String> {
    headers
        .get(axum::http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(',')
                .map(|tok| tok.trim().to_ascii_lowercase())
                .filter(|tok| !tok.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// -- the handler -------------------------------------------------------------

/// Forward a request through `service_id` to the configured upstream.
///
/// Path captures: `service_id` from the segment after `/proxy/`, and `path`
/// from the wildcard tail. We assemble the upstream URL by joining
/// `service.upstream_url`, the captured `path`, and the inbound query
/// string.
pub async fn forward(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((service_id, tail)): Path<(String, String)>,
    req: Request<Body>,
) -> Result<Response, Error> {
    forward_inner(state, peer, service_id, tail, req).await
}

/// Bare-service variant: routed for `/proxy/{service_id}` and
/// `/proxy/{service_id}/` (no wildcard tail). Equivalent to `forward` with
/// an empty path tail — lets callers hit the upstream's root directly.
pub async fn forward_bare(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(service_id): Path<String>,
    req: Request<Body>,
) -> Result<Response, Error> {
    forward_inner(state, peer, service_id, String::new(), req).await
}

async fn forward_inner(
    state: AppState,
    peer: SocketAddr,
    service_id: String,
    tail: String,
    req: Request<Body>,
) -> Result<Response, Error> {
    let (parts, body) = req.into_parts();
    let headers = parts.headers;
    let method = parts.method;
    let uri = parts.uri;

    // 1. Look up the service from the YAML-loaded config.
    let service = state
        .config
        .services
        .iter()
        .find(|s| s.id.as_str() == service_id)
        .ok_or(Error::NotFound)?;

    // 2. Authenticate the caller. Authorization header is required.
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .ok_or(Error::Unauthorized)?;
    let verified = keys::verify(&state.pool, &state.pepper, &bearer)
        .await?
        .ok_or(Error::Unauthorized)?;

    // 3. Service scope check.
    if verified.service_id != service.id.as_str() {
        return Err(Error::Forbidden);
    }

    // 4. Build upstream URL.
    let upstream_url = build_upstream_url(service, &tail, uri.query())?;

    // 5. Strip hop-by-hop + Authorization + session cookie, leaving the
    //    rest. Note: we keep `Content-Type`, `Content-Length`, etc.
    let forwarded_headers = filter_request_headers(&headers);

    // 6. Inject the operator credential (if any — some upstreams are open
    //    and have no `auth:` block in the YAML). Header collisions overwrite
    //    the caller's value and emit a single warn line — §12 "Header
    //    collisions".
    let (mut upstream_headers, mut upstream_url) = (forwarded_headers, upstream_url);
    if let Some(auth) = service.auth.as_ref() {
        inject_auth(
            service.id.as_str(),
            auth,
            &mut upstream_headers,
            &mut upstream_url,
        );
    }

    // 7. Append the immediate peer to X-Forwarded-For. Trust the peer only
    //    (Q4 default; documented in §12).
    add_xff_entry(&mut upstream_headers, peer);

    // 8. Issue with reqwest, streaming the inbound body.
    let upstream_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|_| Error::BadRequest("invalid method"))?;
    let inbound_stream = body.into_data_stream().map_err(std::io::Error::other);
    let reqwest_body = reqwest::Body::wrap_stream(inbound_stream);

    let upstream_resp = state
        .proxy_client
        .request(upstream_method, upstream_url.clone())
        .headers(reqwest_headers_from(&upstream_headers))
        .body(reqwest_body)
        .send()
        .await
        .map_err(map_reqwest_error)?;

    // 9. Stream the response back, again stripping hop-by-hop headers.
    let status = upstream_resp.status();
    let upstream_resp_headers = upstream_resp.headers().clone();
    let resp_stream = upstream_resp.bytes_stream().map_err(std::io::Error::other);
    let body_out = Body::from_stream(resp_stream);

    let mut response_builder = Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    let resp_headers = response_builder
        .headers_mut()
        .expect("builder is fresh, must have headers");
    for (k, v) in upstream_resp_headers.iter() {
        let name = match HeaderName::from_bytes(k.as_str().as_bytes()) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if is_hop_by_hop(&name) {
            continue;
        }
        if let Ok(val) = HeaderValue::from_bytes(v.as_bytes()) {
            resp_headers.append(name, val);
        }
    }
    let response = response_builder
        .body(body_out)
        .map_err(|e| Error::Internal(e.into()))?;

    // 10. Record usage AFTER we've handed the response to the caller. The
    //     batcher flushes asynchronously every 30 s.
    state.proxy_usage.touch(&verified.key_id).await;

    Ok(response)
}

// -- url assembly ------------------------------------------------------------

fn build_upstream_url(
    service: &Service,
    tail: &str,
    query: Option<&str>,
) -> Result<url::Url, Error> {
    // The tail captured by axum's `{*path}` does NOT have a leading slash.
    // We always want exactly one between the upstream's path and the tail.
    let mut upstream = service.upstream_url.clone();
    {
        let base_path = upstream.path().trim_end_matches('/').to_string();
        let joined_path = if tail.is_empty() {
            base_path
        } else {
            format!("{base_path}/{tail}")
        };
        upstream.set_path(&joined_path);
    }
    if let Some(q) = query {
        upstream.set_query(Some(q));
    }
    Ok(upstream)
}

// -- header filtering --------------------------------------------------------

fn filter_request_headers(inbound: &HeaderMap) -> HeaderMap {
    let drop_extra = connection_named(inbound);
    let mut out = HeaderMap::with_capacity(inbound.len());
    for (k, v) in inbound.iter() {
        if is_hop_by_hop(k) {
            continue;
        }
        if drop_extra.iter().any(|d| d == k.as_str()) {
            continue;
        }
        if k == axum::http::header::AUTHORIZATION {
            continue;
        }
        if k == axum::http::header::HOST {
            // `reqwest` will set Host from the upstream URL; don't carry
            // the caller's value.
            continue;
        }
        if k == axum::http::header::COOKIE {
            // Strip just our session cookie. Other cookies pass through —
            // some APIs use cookies for non-session purposes (e.g. AWS).
            if let Some(filtered) = filter_session_cookie(v)
                && !filtered.is_empty()
                && let Ok(val) = HeaderValue::from_str(&filtered)
            {
                out.append(k.clone(), val);
            }
            continue;
        }
        out.append(k.clone(), v.clone());
    }
    out
}

/// Remove just our session cookie from a `Cookie:` header value, preserving
/// the rest. Returns `None` if the value isn't valid UTF-8.
fn filter_session_cookie(v: &HeaderValue) -> Option<String> {
    let s = v.to_str().ok()?;
    let kept: Vec<&str> = s
        .split(';')
        .map(str::trim)
        .filter(|part| {
            part.split_once('=')
                .is_none_or(|(name, _)| name != SESSION_COOKIE)
        })
        .filter(|part| !part.is_empty())
        .collect();
    Some(kept.join("; "))
}

fn reqwest_headers_from(h: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::with_capacity(h.len());
    for (k, v) in h.iter() {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            out.append(name, val);
        }
    }
    out
}

// -- auth injection ----------------------------------------------------------

fn inject_auth(service_id: &str, auth: &ServiceAuth, headers: &mut HeaderMap, url: &mut url::Url) {
    match auth {
        ServiceAuth::Bearer { value } => {
            inject_header_overwriting(
                service_id,
                headers,
                axum::http::header::AUTHORIZATION,
                &format!("Bearer {}", value.expose()),
            );
        }
        ServiceAuth::Header { header, value } => {
            let name = match HeaderName::from_bytes(header.as_bytes()) {
                Ok(n) => n,
                Err(err) => {
                    warn!(error = %err, service_id, "invalid auth.header name; skipping injection");
                    return;
                }
            };
            inject_header_overwriting(service_id, headers, name, value.expose());
        }
        ServiceAuth::Query { param, value } => {
            // Query-string injection is straight set; if the caller passed
            // the same param we overwrite. Caller-supplied values would
            // already have been carried in via `set_query`, so we have to
            // rebuild the query.
            let mut new_pairs: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(k, _)| k != param.as_str())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            new_pairs.push((param.clone(), value.expose().clone()));
            url.query_pairs_mut().clear().extend_pairs(new_pairs);
        }
    }
}

fn inject_header_overwriting(
    service_id: &str,
    headers: &mut HeaderMap,
    name: HeaderName,
    value: &str,
) {
    if headers.contains_key(&name) {
        warn!(
            event = "proxy.header_overwritten",
            service_id = service_id,
            header_name = %name,
            "operator credential overrode caller-supplied header"
        );
    }
    match HeaderValue::from_str(value) {
        Ok(v) => {
            // Mark the header as secret so any future logging that walks
            // the HeaderMap won't dump it.
            let mut v = v;
            v.set_sensitive(true);
            headers.insert(name, v);
        }
        Err(err) => warn!(error = %err, service_id, "invalid auth value; injection skipped"),
    }
}

// -- X-Forwarded-For ---------------------------------------------------------

fn add_xff_entry(headers: &mut HeaderMap, peer: SocketAddr) {
    let entry = peer.ip().to_string();
    let xff = HeaderName::from_static("x-forwarded-for");
    match headers.get(&xff).and_then(|v| v.to_str().ok()) {
        Some(existing) => {
            let chained = format!("{existing}, {entry}");
            if let Ok(v) = HeaderValue::from_str(&chained) {
                headers.insert(xff, v);
            }
        }
        None => {
            if let Ok(v) = HeaderValue::from_str(&entry) {
                headers.insert(xff, v);
            }
        }
    }
}

// -- error mapping -----------------------------------------------------------

fn map_reqwest_error(err: reqwest::Error) -> Error {
    if err.is_timeout() {
        return Error::UpstreamTimeout;
    }
    if err.is_connect() {
        return Error::UpstreamUnreachable;
    }
    // Anything else — TLS, decode, etc. — is upstream unreachability from
    // the caller's perspective.
    warn!(error = %err, "proxy: reqwest error");
    Error::UpstreamUnreachable
}

// -- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Service as ConfigService;
    use crate::config::ServiceAuth;
    use crate::config::ServiceId;
    use crate::secret::Secret;

    fn svc_bearer() -> ConfigService {
        ConfigService {
            id: ServiceId::from_str_for_tests("openai"),
            display_name: "x".into(),
            description: None,
            upstream_url: url::Url::parse("https://api.upstream.test").unwrap(),
            auth: Some(ServiceAuth::Bearer {
                value: Secret::new("sk-OPERATOR".into()),
            }),
        }
    }

    fn svc_header(name: &str) -> ConfigService {
        ConfigService {
            id: ServiceId::from_str_for_tests("inner"),
            display_name: "x".into(),
            description: None,
            upstream_url: url::Url::parse("https://api.upstream.test/base").unwrap(),
            auth: Some(ServiceAuth::Header {
                header: name.into(),
                value: Secret::new("OP-SECRET".into()),
            }),
        }
    }

    fn svc_query() -> ConfigService {
        ConfigService {
            id: ServiceId::from_str_for_tests("q"),
            display_name: "x".into(),
            description: None,
            upstream_url: url::Url::parse("https://api.upstream.test").unwrap(),
            auth: Some(ServiceAuth::Query {
                param: "api_key".into(),
                value: Secret::new("OP".into()),
            }),
        }
    }

    #[test]
    fn build_upstream_url_joins_path_and_query() {
        let svc = svc_bearer();
        let u = build_upstream_url(&svc, "v1/chat/completions", Some("debug=1")).unwrap();
        assert_eq!(
            u.as_str(),
            "https://api.upstream.test/v1/chat/completions?debug=1"
        );
    }

    #[test]
    fn build_upstream_url_preserves_base_path() {
        // axum captures the tail without the query string, so the realistic
        // shape is `tail = "search"`, `query = Some("q=hi")`. The base path
        // `/base` from the service config must survive the join.
        let svc = svc_header("X-Api");
        let u = build_upstream_url(&svc, "search", Some("q=hi")).unwrap();
        assert_eq!(u.as_str(), "https://api.upstream.test/base/search?q=hi");
    }

    #[test]
    fn build_upstream_url_empty_tail_hits_base() {
        let svc = svc_header("X-Api");
        let u = build_upstream_url(&svc, "", None).unwrap();
        assert_eq!(u.as_str(), "https://api.upstream.test/base");
    }

    #[test]
    fn inject_bearer_overrides_caller_authorization() {
        let svc = svc_bearer();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer pi_live_calling"),
        );
        let mut url = svc.upstream_url.clone();
        inject_auth("openai", svc.auth.as_ref().unwrap(), &mut headers, &mut url);
        let got = headers
            .get(axum::http::header::AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(got, "Bearer sk-OPERATOR");
    }

    #[test]
    fn inject_custom_header_works() {
        let svc = svc_header("X-Internal");
        let mut headers = HeaderMap::new();
        let mut url = svc.upstream_url.clone();
        inject_auth("inner", svc.auth.as_ref().unwrap(), &mut headers, &mut url);
        assert_eq!(
            headers.get("x-internal").unwrap().to_str().unwrap(),
            "OP-SECRET"
        );
    }

    #[test]
    fn inject_query_param_overrides_caller_value() {
        let svc = svc_query();
        let mut headers = HeaderMap::new();
        let mut url = svc.upstream_url.clone();
        url.set_query(Some("api_key=caller&other=keep"));
        inject_auth("q", svc.auth.as_ref().unwrap(), &mut headers, &mut url);
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q.get("api_key").map(|s| s.as_str()), Some("OP"));
        assert_eq!(q.get("other").map(|s| s.as_str()), Some("keep"));
    }

    #[test]
    fn hop_by_hop_headers_get_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("close, x-custom"));
        headers.insert("x-custom", HeaderValue::from_static("drop me"));
        headers.insert("upgrade", HeaderValue::from_static("websocket"));
        headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        headers.insert("authorization", HeaderValue::from_static("Bearer pi_x"));
        headers.insert("host", HeaderValue::from_static("pietro.example"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let filtered = filter_request_headers(&headers);
        for stripped in [
            "connection",
            "upgrade",
            "keep-alive",
            "authorization",
            "host",
            "x-custom", // listed in Connection header
        ] {
            assert!(
                filtered.get(stripped).is_none(),
                "expected {stripped} to be stripped"
            );
        }
        assert!(filtered.get("content-type").is_some());
    }

    #[test]
    fn session_cookie_is_stripped_but_others_pass() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_static("pietro_session=secret; user_pref=dark; csrf=z"),
        );
        let filtered = filter_request_headers(&headers);
        let cookie = filtered
            .get(axum::http::header::COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(!cookie.contains("pietro_session"));
        assert!(cookie.contains("user_pref=dark"));
        assert!(cookie.contains("csrf=z"));
    }

    #[test]
    fn xff_appends_when_present_and_sets_when_absent() {
        let peer: SocketAddr = "203.0.113.7:55555".parse().unwrap();
        let mut headers = HeaderMap::new();
        add_xff_entry(&mut headers, peer);
        assert_eq!(
            headers.get("x-forwarded-for").unwrap().to_str().unwrap(),
            "203.0.113.7"
        );
        // Run a second time as if the caller had already set XFF.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        add_xff_entry(&mut headers, peer);
        assert_eq!(
            headers.get("x-forwarded-for").unwrap().to_str().unwrap(),
            "10.0.0.1, 203.0.113.7"
        );
    }
}
