# Pietro — Agent Memory

Updated: 2026-05-15 (M7 complete — v1 shipped)

## ⏯ How to resume (read me first)

If you are an agent picking this project up:

1. **Read `STATUS.md`** in the repo root — the one-page checkpoint snapshot.
   It has the milestone status, source map, HTTP surface, design rules in
   force, and a step-by-step "verify the project is healthy" command list.
2. **Run `cargo test`** to confirm the 60/60 baseline. If anything is red,
   stop and ask what changed.
3. **Then read this file** — Episodic / Semantic / Procedural — for the
   deeper context behind every decision.
4. For the locked design plan, read `pietro.md`. §19 has milestone status,
   §20 has all six open questions resolved, and the rest is unchanged from
   when it was written.

What's next: **Pietro v1 is complete.** M1–M7 shipped, 60/60 green, single
musl binary (11 MB stripped), GitHub Actions release workflow ready.
Anything beyond this is v2 work — see §21 of `pietro.md` for the explicit
"deliberately not in this plan" list, and §2 for the YAGNI doctrine.
Beyond v1 you might tag the first release (`git tag v0.1.0 && git push
origin v0.1.0`) to exercise the release workflow end to end.

## Episodic
- 2026-05-14: User asked PeakBot to plan **Pietro**, a Rust-based authenticated API proxy with a React UI bundled in a single binary, OIDC login, and YAML config. PeakBot researched state of the art (axum, reqwest, openidconnect, rust-embed, sqlx/SQLite, BLAKE3, axum-extra cookies), applied the Zen of Software Engineering skill, and wrote `pietro.md` with a detailed design plan. No code yet — plan is locked first by Zen rule "no code before the plan is locked".
- 2026-05-14 (early sessions): Q1 (Tailwind via Vite), Q2 (overwrite-and-warn on header collisions), Q3 (email-only allowlist), Q5 (one *active* key per user+service, partial unique index enforced) resolved. The Q5 resolution propagated through §7/§9/§11.2/§18 of pietro.md.
- 2026-05-14 (M1 build): cargo init + `npm create vite@latest` scaffold. clap CLI, env-interpolated YAML config with validation, `Secret<T>`, axum `/healthz`. 9/9 tests.
- 2026-05-14 (M2 build): sqlx 0.8 (sqlite/macros/migrate, no TLS). `migrations/0001_init.sql` is §9 verbatim, including the partial unique index that enforces Q5. WAL + foreign_keys + busy_timeout. `pietro migrate` idempotent. /healthz pings the pool. 11/11 tests.
- 2026-05-14 (M3 build): openidconnect 4.0.1, reqwest 0.12 rustls-tls, axum-extra 0.12 (cookie+cookie-signed), cookie 0.18 `key-expansion` (axum-extra doesn't enable it). `src/auth/` with `OidcState::from_config` (discovery at startup, fail fast), AuthenticatedUser extractor, full PKCE+state+nonce flow, email allowlist, user upsert, session creation, per-cookie logout. errors.rs gets IntoResponse with the single JSON shape. 24/24 tests. **Pinned**: openidconnect 4.x's `CoreClient` produced by `from_provider_metadata` carries `EndpointMaybeSet` parameters that can't be reconstructed via `CoreClient::new + setters` — tests must build the offline client through `from_provider_metadata` with a hand-built `CoreProviderMetadata`.
- 2026-05-14 (M4 build): blake3 1, base32 0.5 (Crockford). `src/keys.rs` ApiKey/ApiKeyHash/KeyId newtypes + mint/list/revoke/verify + plaintext-once contract + 409 on dup via partial unique index. Four new routes (`/api/services`, `GET|POST /api/keys`, `DELETE /api/keys/{id}`) all session-guarded. Router tests sign the cookie via `cookie::CookieJar::signed_mut` — no test-only auth bypass. Live-server smoke confirms 401+JSON on every M4 endpoint unauth. 40/40 tests. **Pinned**: `pkill -f target/debug/pietro` before each smoke run; stale binaries are confusing.
- 2026-05-14 (M5 build): futures-util + reqwest `stream` feature. `src/proxy.rs` is the entire forwarder: hop-by-hop strip (RFC 7230 §6.1 plus dynamic names from inbound `Connection:`), session-cookie carve-out from `Cookie:`, three auth-injection modes (bearer/header/query) with overwrite-and-warn (values never logged, set sensitive on inserted HeaderValue), XFF chain via `ConnectInfo<SocketAddr>`, both directions streamed (request via `reqwest::Body::wrap_stream(BodyDataStream)`, response via `bytes_stream()` → `axum::Body::from_stream`), reqwest error mapping (timeout→504, connect→502, other→502). Background `UsageBatcher` flushes `HashMap<KeyId, SystemTime>` every 30 s + on shutdown via `tokio::oneshot`. Route mounted with `axum::routing::any` on `/proxy/{service_id}/{*path}`. `into_make_service_with_connect_info` in main.rs gives the handler the peer addr. 55/55 tests including 6 wiremock-driven integration tests that mint a real key and drive the full forwarder. Q4 resolved (trust immediate peer). **Pinned**: in axum 0.8 the `Body::wrap_stream` / `Response::bytes_stream` reqwest APIs are gated behind the `stream` feature; default-features=false hides them. Tests inject `ConnectInfo<SocketAddr>` via `req.extensions_mut().insert(...)` because `Router::oneshot` doesn't carry a peer.
- 2026-05-14 (M6 prep): user requested `cargo fmt` + `cargo clippy -D warnings` gating each future step. First run revealed M5 had shipped with 1 rustfmt drift in `src/routes.rs` and 7 clippy lints across `src/config.rs`, `src/db.rs`, `src/proxy.rs` (manual_is_multiple_of, doc_overindented_list_items×2, unnecessary_to_owned, collapsible_if×2 via let-chains, nonminimal_bool→is_none_or). Cleanup commit `e72bb5a` also dropped a vestigial dead `build_upstream_url(..., "search?q=hi", None).unwrap()` call from the URL test — the second realistic call in the same test was always the one doing the work. Clean baseline established before any UI code touched.
- 2026-05-14 (M6 build, 6 small commits, one per page/feature):
  - **Step 2** wired `@tailwindcss/vite` plugin + Vite dev-proxy for `/api`, `/proxy` → `http://127.0.0.1:18080` in `vite.config.ts`. `src/index.css` collapsed to one line: `@import "tailwindcss";`. Vite-template demo (App.tsx + App.css) replaced with a Tailwind-using placeholder.
  - **Step 3** added `react-router-dom@^7` (one runtime dep). `src/api.ts`: 110-line typed fetch wrapper with `ApiError` parsing the project's `{ error: { code, message } }` envelope, plus `Me`/`Service`/`ApiKey`/`MintedKey` types verified against `src/routes.rs` + `src/keys.rs:KeyRecord`. `App.tsx`: BrowserRouter + three-state session probe (`loading | out | in`) + `RequireAuth` guard. The three pages started as stubs.
  - **Step 4** `/login`: single `<a href="/api/auth/login">` — not a fetch, because fetch would break the 303→IdP chain. Tailwind card; light+dark.
  - **Step 5** `/` dashboard: lists `/api/keys` (renders revoked rows too, dim'd with badge — the backend's list query has no `revoked_at IS NULL` filter, so honest rendering), optimistic-revoke that flips the row in place and refetches on error, header with email + sign-out button. Hit React 19's new `react-hooks/set-state-in-effect` lint — fixed by inlining the mount-time load into `.then/.catch` with a `cancelled` flag (matching `App.tsx`'s pattern). Kept the named `refresh()` helper only for the post-revoke repair path.
  - **Step 6** `/new` mint flow: two phases on one component. Form (service dropdown + 128-char-capped label input) → reveal phase with prominent amber banner, monospace plaintext block, `navigator.clipboard.writeText` with a `document.createRange` selection fallback for insecure contexts, and an explicit "I've saved it" link instead of auto-redirect. The plaintext-once contract is enforced by UX, not by JS state — a refresh just drops it.
  - **Step 7** sweep: removed orphaned Vite-template assets (`hero.png`, `react.svg`, `vite.svg`, `icons.svg`), renamed `<title>` to "Pietro". Live end-to-end smoke confirmed: SPA served by Vite at `:5174` (it auto-picked because :5173 was busy), `/api/me` and `/api/keys` proxied through and returning 401-with-JSON, `/api/auth/login` 303s through to the IdP with full PKCE+state+nonce in the URL, `/proxy/openai/v1/x` 401s through. Final bundle: 243 KB JS / 77 KB gzipped — well under the §14.2 200 KB-gz target. 55/55 Rust tests still green. **Pinned for M6**: (a) Vite binds `[::1]` (IPv6 loopback) by default, so curl `127.0.0.1:5174` fails; use `localhost` or `[::1]`. (b) React 19's `react-hooks/set-state-in-effect` is conservative — it fires if the effect calls a named function that contains setState; refactor mount-time loads into inline `.then/.catch` with a `cancelled` flag.
- 2026-05-15 (M7 build, 3 small commits): added `rust-embed = "8"` — the one new crate the budget reserved for this milestone, taking prod count to 24/25 exactly.
  - **Step 1** (`fc75fb7`): created `src/spa.rs` with a `RustEmbed`-backed handler. Two public handlers — `serve_asset` for `GET /assets/{*path}` and `fallback` for `Router::fallback`. Hand-rolled 10-line content-type table (JS/CSS/HTML/SVG/PNG/ICO/JSON/woff/woff2/map/txt) — no `mime_guess` dep needed for our bundle. **Hard `#[cfg(debug_assertions)]` split**: in debug both handlers return a hostname-agnostic dev-mode notice page; in release they serve the embedded bytes. The §7 carve-out is preserved via `is_api_or_proxy_path`: unknown `/api/*` and `/proxy/*` paths JSON-404 through the project envelope rather than getting the SPA. `build.rs` added: re-runs only on `frontend/dist/index.html` + `frontend/dist/assets/` changes; fails the build in release mode when index.html is missing (with a clear "run `cd frontend && npm run build`" hint), warns in debug so backend-only dev work isn't blocked. 5 new tests (3 router integration, 2 unit) bring the suite to **60/60**. Live release smoke proved end-to-end: `/` serves the real embedded index.html, `/assets/<hash>.js` serves 243 KB `application/javascript`, `/some/spa/route` serves identical bytes (history-mode), `/api/garbage` and `/assets/missing` JSON-404. Release binary measured **11 MB stripped** (under the §13 15–25 MB target). One clippy lint hit during the run (`doc_overindented_list_items` on the routes.rs docstring continuation) — fixed by flattening the continuation line to 4-space indent.
  - **Step 2** (`8b226d4`): `.github/workflows/release.yml` — tag-triggered (`v*`) musl build for x86_64 (host `musl-tools`) + aarch64 (via `cross`, no aarch64-musl-gcc needed on the runner). Order is §13 strict: SPA build first (`npm ci && npm run build`), then `cargo build --release` so rust-embed has bytes and `build.rs`'s release-mode check passes. Tarballs (`pietro-<tag>-{target}.tar.gz`) + `.sha256` upload via `softprops/action-gh-release@v2`. Hyphenated tags (e.g. `v0.1.0-rc1`) automatically flip to `prerelease: true`.
  - **Step 3** (this commit): ship docs — `STATUS.md` rewritten as the v1-complete snapshot (60/60, 11 MB binary, release workflow, debug-vs-release SPA contract); `pietro.md` §19 marks M7 shipped and the top-of-doc checkpoint pointer updated; `memory.md` (this file) updated. **Pinned for M7**: (a) Debug builds intentionally never serve the embedded SPA — they return the notice page. This is to kill the "I'm staring at four-day-old embedded bytes" foot-gun; if you need to test the SPA bundle, `cargo build --release`. (b) `Router::fallback` in axum 0.8 catches any path that didn't match a real route; classify by prefix inside the handler to honor §7 carve-out. (c) rust-embed gracefully embeds an empty asset set when the folder is missing — useful for the dev-mode story, but `build.rs` enforces non-emptiness in release.

## Semantic (facts about this project)
- **Name**: Pietro (Saint Peter, keeper of the keys).
- **Stack**: axum + tokio + reqwest + sqlx/SQLite + openidconnect + rust-embed (M7) + serde_yaml + clap + tracing.
- **Crate budget in `Cargo.toml`** (after M7):
  - prod (24): axum 0.8, tokio 1, clap 4, serde 1, serde_yaml 0.9, serde_json 1, url 2, regex 1, anyhow 1, thiserror 2, tracing 0.1, tracing-subscriber 0.3, sqlx 0.8, openidconnect 4, reqwest 0.12 (rustls-tls + stream), axum-extra 0.12 (cookie+cookie-signed), cookie 0.18 (key-expansion), base64 0.22, hex 0.4, base32 0.5, rand 0.8, blake3 1, time 0.3, futures-util 0.3, rust-embed 8.
  - dev (2): tower 0.5, wiremock 0.6.
  - v1 done; total 26 (24 prod + 2 dev), one over the §5 soft ceiling of 25 by design — the plan explicitly reserved the rust-embed slot for M7.
- **Auth pattern**: OIDC auth-code + PKCE for human login. Signed-cookie + DB-backed session id (12h TTL via `SESSION_TTL`). Logout deletes the row. Flow state (state + nonce + PKCE verifier) lives in a short-lived 5-minute `pietro_flow` signed cookie scoped to `/api/auth/`.
- **OIDC client type pinning**: `PietroOidcClient = CoreClient<EndpointSet, EndpointNotSet*3, EndpointMaybeSet*2>` — produced by `from_provider_metadata`. Don't try to assemble via `CoreClient::new` + setters; the types won't line up.
- **API key pattern**: `pi_live_<22 char base32-Crockford>` (actually 23 chars from 14 random bytes — we keep the natural length). Stored as BLAKE3 of (server pepper || plaintext). Plaintext shown once at creation. `prefix` (12 chars) and `last4` persisted for UI display. `KeyId`: `pi_<7 char base32-Crockford>`.
- **API key uniqueness rule**: at most **one active key per (user, service)** — three layers of regression coverage (DB-level test, keys-module test, router-level test). On collision the handler returns 409 `key_already_exists` with the project JSON shape — no silent auto-revoke.
- **Pepper handling**: `cfg.api_key_pepper` decoded at startup, stored as `Arc<Vec<u8>>` in `AppState`, passed by reference into `keys::mint` / `keys::verify`.
- **Plaintext leak surface**: exactly two places: response of `POST /api/keys` (returned once, then dropped) and the argument to `keys::verify` on the proxy hot path (hashed immediately). `ApiKey` has no `Debug`; `MintedKey::Debug` redacts the plaintext field; injected operator auth headers are marked `sensitive`.
- **Proxy forwarder** (`src/proxy.rs`):
  - One handler `forward(State, ConnectInfo, Path((service_id, tail)), Request)`.
  - Hop-by-hop list: Connection, Keep-Alive, Proxy-Authenticate, Proxy-Authorization, TE, Trailer, Transfer-Encoding, Upgrade — plus the comma-separated names listed inside the inbound `Connection:` header. Plus Authorization (we replace with operator credential). Plus Host (reqwest sets it from the URL). Plus `pietro_session` carved out of `Cookie:` (other cookies pass through).
  - Bodies streamed both ways: a 10 GB upload doesn't OOM Pietro.
  - 60 s default per-request timeout; redirects disabled (response Location passes through to caller unchanged).
  - `UsageBatcher` (Mutex<HashMap<String, SystemTime>>) drained every 30 s by a tokio task that also drains on graceful shutdown (driven by a `tokio::sync::oneshot`).
  - Errors map per §12 table.
- **Config**: single `pietro.yaml`, env interpolation via `${VAR}`. Validated at load via `Config::load(&Path)` or `Config::from_yaml_str(&str)`. Key material decoder: `config::decode_key_material(&str) -> Option<Vec<u8>>` (hex / base64 / raw bytes).
- **Cookie signing**: `axum-extra::extract::cookie::Key::derive_from(&master)` — requires `cookie` crate with `key-expansion`.
- **Error model**: `errors::Error` enum + `IntoResponse` → `{ "error": { "code", "message" } }`. `Internal` surfaces only as a generic message; the underlying error is logged at `error!` level via `tracing`.
- **Storage**: 3 tables — `users`, `api_keys`, `sessions`. Services live only in YAML. Migrations embedded via `sqlx::migrate!("./migrations")`.
- **SQLite tuning**: `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`, pool `max_connections=8`.
- **SQLite datetime convention**: SQLite's `datetime('now')` is `"YYYY-MM-DD HH:MM:SS"` UTC, no tz suffix. `auth::session::format_sqlite_datetime` formats `time::OffsetDateTime` to match. `keys::mark_used` uses `datetime(?, 'unixepoch')` which produces the same shape.
- **Vocabulary**: Operator (writes YAML), User (logs in via OIDC), Caller (uses API key), Service (configured upstream), Key. Never alias.
- **Roles separated by type**, not booleans: `AuthenticatedUser`, `ApiKey` vs `ApiKeyHash` vs `KeyId`, `ServiceId`, `UserId(String)`.
- **Single binary** (M7, shipped): `rust-embed` over `frontend/dist/`. Handlers live in `src/spa.rs`. `serve_asset` for `/assets/{*path}` (real miss → JSON-404; no SPA fallback under `/assets/`). `fallback` mounted on `Router::fallback` — SPA `index.html` for any unknown non-API path; JSON-404 envelope under `/api/*` and `/proxy/*` (§7 carve-out via `is_api_or_proxy_path`). 10-line hand-rolled content-type table covers the bundle shape; no `mime_guess` dep. `build.rs` re-runs only on `frontend/dist/index.html` + `frontend/dist/assets/` changes, fails release builds when index.html is missing (clear hint to `cd frontend && npm run build`), warns in debug.
- **Debug-vs-release SPA contract (M7)**: in `#[cfg(debug_assertions)]` builds both SPA handlers return a hostname-agnostic notice page rather than embedded bytes. The intended dev loop is Vite on a separate port. To test the embedded bundle, `cargo build --release`. Stripped release binary: 11 MB (under the §13 15–25 MB target).
- **Release distribution (M7)**: `.github/workflows/release.yml` builds `pietro-<tag>-{x86_64,aarch64}-unknown-linux-musl.tar.gz` + `.sha256` on tag push (`v*`). x86_64 uses host `musl-tools`; aarch64 uses `cross` to avoid needing aarch64-musl-gcc on the runner. Hyphenated tags (e.g. `v0.1.0-rc1`) become `prerelease: true` automatically. Upload via `softprops/action-gh-release@v2`. Workflow permissions: `contents: write` only.
- **`Secret<T>`**: project-local 30-line newtype. Redacts in `Debug`; access via `.expose()`.

## Procedural (how-to for this project)
- Before adding any new feature, re-read §2 in `pietro.md`. YAGNI hard.
- Crates budget: 25 max (soft ceiling). After M7 we sit at 24 prod + 2 dev = 26, deliberately one over by virtue of the `rust-embed` slot that the plan reserved for M7. Any further dep needs explicit budget justification with a written reason in `Cargo.toml`. Frontend runtime budget: 3 npm deps (`react`, `react-dom`, `react-router-dom`); a fourth needs explicit justification.
- Plan first. Confirm. Then code. Never start implementation while open questions in §20 are unanswered.
- **Gate every change** with `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`. For frontend changes also run `npm run lint` and `npm run build`. This was retrofitted after M5 — never let drift accumulate again.
- All mutating handlers go through `errors::Error::into_response`. Never hand-roll an alternate JSON shape.
- API keys must never appear in logs. `ApiKey`, `MintedKey::Debug`, `Secret<T>::Debug` all redact. Operator credentials injected into headers are marked `set_sensitive(true)` so any future header-walking logging is also safe. The OIDC handler logs `***@domain` for emails.
- Migrations are append-only, numbered, embedded. Never edit a shipped migration.
- Bootstrapping: project skeleton from `cargo init` + `npm create vite@latest`. Trim, don't hand-roll.
- **Dead-code attributes**: after M5, only the `Result` alias in `errors.rs` and `UserId::as_str` carry an `#[allow(dead_code, reason = "...")]`. Both are general-utility items that may or may not be used by M6/M7.
- **Tests against the running binary**: kill stale processes first (`pkill -f target/debug/pietro` or `pkill -f target/release/pietro`) and dodge :8080 on the dev host (the example config now uses :18080).
- **Debug builds intentionally don't serve the embedded SPA (M7).** Both `serve_asset` and `fallback` return a notice page in `#[cfg(debug_assertions)]`. If you're debugging "why doesn't /assets/foo.js work?", remember: `cargo build --release` is the answer. The dev loop is Vite (`cd frontend && npm run dev`), which proxies `/api` and `/proxy` back to :18080.
- **Tests in code**: cookie-signed extractor → use `cookie::CookieJar::signed_mut(&key).add(...)`. Proxy with peer info → `req.extensions_mut().insert(ConnectInfo::<SocketAddr>(addr))`. OIDC offline client → `CoreProviderMetadata::new(...).set_token_endpoint(Some(...))` then `CoreClient::from_provider_metadata(...)`.
- **Smoke test recipe** (M5):
  ```
  rm -f pietro.db*
  python3 scripts/fake-idp.py 19000 &
  export PIETRO_COOKIE_KEY=$(openssl rand -hex 32)
  export PIETRO_API_KEY_PEPPER=$(openssl rand -hex 32)
  export PIETRO_OIDC_CLIENT_SECRET=dev
  export OPENAI_API_KEY=sk-test
  cargo build && pkill -f 'target/debug/pietro' || true
  target/debug/pietro serve --config pietro.yaml &
  curl /healthz                            # → "ok"
  curl /api/services                       # → 401 JSON
  curl /api/keys                           # → 401 JSON
  curl -X POST /api/keys                   # → 401 JSON
  curl /proxy/openai/anything              # → 401 JSON  (no Bearer)
  ```
  Authenticated proxy flows are covered by `routes::tests::proxy_*` against wiremock upstreams.

## Open user questions
All resolved as of 2026-05-14 M5 ship. See §20 of `pietro.md`.

## Milestone status
- [x] **M1 — Skeleton.** 9/9 tests.
- [x] **M2 — DB + migrations.** 11/11 tests.
- [x] **M3 — OIDC login.** 24/24 tests.
- [x] **M4 — Key lifecycle.** 40/40 tests.
- [x] **M5 — Proxy.** 55/55 tests. Streaming forwarder, three auth modes, hop-by-hop strip, XFF, usage batcher.
- [x] **M6 — React UI.** 243 KB JS / 77 KB gz. Three pages (`/`, `/new`, `/login`), Tailwind v4, react-router v7, typed fetch wrapper, three-state session probe + `RequireAuth`, optimistic revoke, plaintext-once reveal with copy + acknowledged exit. Rust still 55/55.
- [x] **M7 — Embed + release.** 60/60 tests. `rust-embed` SPA serve + dev-mode notice (§13 split: debug never serves embedded bytes). `Router::fallback` honors §7 carve-out for `/api/*` and `/proxy/*`. `build.rs` enforces dist/ in release, warns in debug. GitHub Actions release workflow for x86_64 + aarch64 musl tarballs. **Pietro v1 is complete.**
