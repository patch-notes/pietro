//! SPA serving (M7, §13).
//!
//! `rust-embed` compiles `frontend/dist/` into the binary. This module wraps
//! it with two handlers that the router mounts:
//!
//!   * [`serve_asset`] — for `GET /assets/{*path}`: serves embedded static
//!     bundle files with a Content-Type guessed by extension. 404 if not
//!     present (no SPA fallback — `/assets/` only ever holds real files).
//!   * [`fallback`] — mounted as `Router::fallback`. Any path that didn't
//!     match a real route lands here:
//!       - `/api/*` and `/proxy/*` → JSON 404 with the project error
//!         envelope, so the SPA never accidentally swallows an unknown
//!         API path (§7).
//!       - anything else → SPA `index.html` (history-mode routing).
//!
//! ## Debug vs release
//!
//! Per §13: "a developer who hits :8080 directly gets a clear note, not a
//! 4-day-old SPA." So in `#[cfg(debug_assertions)]` builds, both handlers
//! return a hostname-agnostic dev-mode notice page instead of any embedded
//! bytes. The expected dev workflow is Vite on a separate port; the embedded
//! bundle is only meaningful in release builds.

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// Compile-time embed of the React build output. Empty in debug — see the
/// module docstring; build.rs only enforces a populated dist/ for release
/// builds, where the embed actually matters.
#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Spa;

/// Body of the dev-mode notice page. Hostname-agnostic on purpose: the dev
/// who hits this can be on any host:port, and Vite occasionally picks a
/// different port (`:5174` when `:5173` is busy — see memory.md).
const DEV_NOTICE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <title>Pietro — debug build</title>
  <style>
    body { font-family: ui-sans-serif, system-ui, sans-serif;
           max-width: 38rem; margin: 4rem auto; padding: 0 1rem;
           color: #222; line-height: 1.5; }
    h1 { font-size: 1.5rem; }
    code { background: #f3f3f3; padding: 0.1rem 0.35rem; border-radius: 0.25rem; }
    .muted { color: #666; font-size: 0.9rem; }
  </style>
</head>
<body>
  <h1>Pietro — debug build</h1>
  <p>You are hitting the Pietro binary directly in a <strong>debug</strong>
     build. The React SPA is intentionally <em>not</em> served from here in
     debug — the embedded bundle would go stale silently.</p>
  <p>The dev workflow is to run Vite separately:</p>
  <pre><code>cd frontend &amp;&amp; npm run dev</code></pre>
  <p>Vite picks the first free port starting at <code>5173</code> and proxies
     <code>/api/*</code> and <code>/proxy/*</code> back to this binary.</p>
  <p class="muted">To test the embedded bundle, build in release:
     <code>cargo build --release</code>.</p>
</body>
</html>
"#;

fn dev_notice_response() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DEV_NOTICE,
    )
        .into_response()
}

/// JSON 404 in the project envelope. Matches `errors::Error::NotFound` so
/// `/api/garbage` and `/proxy/garbage` look the same as a real handler 404.
fn json_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":{"code":"not_found","message":"not found"}}"#,
    )
        .into_response()
}

/// Map a file extension to a sensible Content-Type. This is enough for our
/// bundle (JS, CSS, HTML, SVG, ICO, PNG, JSON, plus a fallback). Adding a
/// `mime_guess` dep for a ten-line table would be wasteful.
fn content_type_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Serve one embedded file by exact path (no fallback). Returns 404 if not
/// found. Used for `/assets/{*path}` — those URLs are content-addressed by
/// Vite, so a miss is a real miss.
fn serve_embedded(path: &str) -> Response {
    let Some(file) = Spa::get(path) else {
        return json_not_found();
    };
    let body = Body::from(file.data.into_owned());
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(path)),
    );
    resp
}

/// `GET /assets/{*path}` handler. In debug builds returns the dev notice
/// (because no assets are embedded). In release, serves the asset or 404s.
pub async fn serve_asset(Path(path): Path<String>) -> Response {
    if cfg!(debug_assertions) {
        return dev_notice_response();
    }
    // Rebuild the full embed key: `Path<String>` strips the route prefix.
    let key = format!("assets/{path}");
    serve_embedded(&key)
}

/// Router fallback. Returns:
///   * a JSON-404 envelope for paths under `/api/` and `/proxy/` (§7);
///   * the dev-mode notice for any other path in debug builds;
///   * the embedded `index.html` for any other path in release builds.
pub async fn fallback(req: axum::http::Request<Body>) -> Response {
    let path = req.uri().path();
    if is_api_or_proxy_path(path) {
        return json_not_found();
    }
    if cfg!(debug_assertions) {
        return dev_notice_response();
    }
    // SPA history-mode: any unknown non-API path returns index.html and lets
    // react-router handle it on the client.
    serve_embedded("index.html")
}

/// Centralised so both the fallback and any future caller agree on the rule.
fn is_api_or_proxy_path(path: &str) -> bool {
    path.starts_with("/api/") || path.starts_with("/proxy/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_table_covers_bundle_shape() {
        assert!(content_type_for("foo.js").starts_with("application/javascript"));
        assert!(content_type_for("foo.css").starts_with("text/css"));
        assert!(content_type_for("foo.html").starts_with("text/html"));
        assert_eq!(content_type_for("favicon.svg"), "image/svg+xml");
        assert_eq!(content_type_for("favicon.ico"), "image/x-icon");
        assert_eq!(content_type_for("blob"), "application/octet-stream");
    }

    #[test]
    fn api_and_proxy_paths_are_classified() {
        assert!(is_api_or_proxy_path("/api/garbage"));
        assert!(is_api_or_proxy_path("/api/keys/abc"));
        assert!(is_api_or_proxy_path("/proxy/openai/anything"));
        assert!(!is_api_or_proxy_path("/"));
        assert!(!is_api_or_proxy_path("/new"));
        assert!(!is_api_or_proxy_path("/assets/index.js"));
        // The literal "/api" without trailing slash is NOT an API path; it
        // would 404 to the SPA. That's fine — it's not a real route.
        assert!(!is_api_or_proxy_path("/api"));
    }
}
