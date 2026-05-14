# Pietro — Agent Memory

Updated: 2026-05-14 (M5 complete — checkpointed)

## ⏯ How to resume (read me first)

If you are an agent picking this project up:

1. **Read `STATUS.md`** in the repo root — the one-page checkpoint snapshot.
   It has the milestone status, source map, HTTP surface, design rules in
   force, and a step-by-step "verify the project is healthy" command list.
2. **Run `cargo test`** to confirm the 55/55 baseline. If anything is red,
   stop and ask what changed.
3. **Then read this file** — Episodic / Semantic / Procedural — for the
   deeper context behind every decision.
4. For the locked design plan, read `pietro.md`. §19 has milestone status,
   §20 has all six open questions resolved, and the rest is unchanged from
   when it was written.

What's next: **M6 — React UI**. The backend is feature-complete; the UI is
the next chunk. See §14 of `pietro.md` and the "What's next" section of
`STATUS.md`.

## Episodic
- 2026-05-14: User asked PeakBot to plan **Pietro**, a Rust-based authenticated API proxy with a React UI bundled in a single binary, OIDC login, and YAML config. PeakBot researched state of the art (axum, reqwest, openidconnect, rust-embed, sqlx/SQLite, BLAKE3, axum-extra cookies), applied the Zen of Software Engineering skill, and wrote `pietro.md` with a detailed design plan. No code yet — plan is locked first by Zen rule "no code before the plan is locked".
- 2026-05-14 (early sessions): Q1 (Tailwind via Vite), Q2 (overwrite-and-warn on header collisions), Q3 (email-only allowlist), Q5 (one *active* key per user+service, partial unique index enforced) resolved. The Q5 resolution propagated through §7/§9/§11.2/§18 of pietro.md.
- 2026-05-14 (M1 build): cargo init + `npm create vite@latest` scaffold. clap CLI, env-interpolated YAML config with validation, `Secret<T>`, axum `/healthz`. 9/9 tests.
- 2026-05-14 (M2 build): sqlx 0.8 (sqlite/macros/migrate, no TLS). `migrations/0001_init.sql` is §9 verbatim, including the partial unique index that enforces Q5. WAL + foreign_keys + busy_timeout. `pietro migrate` idempotent. /healthz pings the pool. 11/11 tests.
- 2026-05-14 (M3 build): openidconnect 4.0.1, reqwest 0.12 rustls-tls, axum-extra 0.12 (cookie+cookie-signed), cookie 0.18 `key-expansion` (axum-extra doesn't enable it). `src/auth/` with `OidcState::from_config` (discovery at startup, fail fast), AuthenticatedUser extractor, full PKCE+state+nonce flow, email allowlist, user upsert, session creation, per-cookie logout. errors.rs gets IntoResponse with the single JSON shape. 24/24 tests. **Pinned**: openidconnect 4.x's `CoreClient` produced by `from_provider_metadata` carries `EndpointMaybeSet` parameters that can't be reconstructed via `CoreClient::new + setters` — tests must build the offline client through `from_provider_metadata` with a hand-built `CoreProviderMetadata`.
- 2026-05-14 (M4 build): blake3 1, base32 0.5 (Crockford). `src/keys.rs` ApiKey/ApiKeyHash/KeyId newtypes + mint/list/revoke/verify + plaintext-once contract + 409 on dup via partial unique index. Four new routes (`/api/services`, `GET|POST /api/keys`, `DELETE /api/keys/{id}`) all session-guarded. Router tests sign the cookie via `cookie::CookieJar::signed_mut` — no test-only auth bypass. Live-server smoke confirms 401+JSON on every M4 endpoint unauth. 40/40 tests. **Pinned**: `pkill -f target/debug/pietro` before each smoke run; stale binaries are confusing.
- 2026-05-14 (M5 build): futures-util + reqwest `stream` feature. `src/proxy.rs` is the entire forwarder: hop-by-hop strip (RFC 7230 §6.1 plus dynamic names from inbound `Connection:`), session-cookie carve-out from `Cookie:`, three auth-injection modes (bearer/header/query) with overwrite-and-warn (values never logged, set sensitive on inserted HeaderValue), XFF chain via `ConnectInfo<SocketAddr>`, both directions streamed (request via `reqwest::Body::wrap_stream(BodyDataStream)`, response via `bytes_stream()` → `axum::Body::from_stream`), reqwest error mapping (timeout→504, connect→502, other→502). Background `UsageBatcher` flushes `HashMap<KeyId, SystemTime>` every 30 s + on shutdown via `tokio::oneshot`. Route mounted with `axum::routing::any` on `/proxy/{service_id}/{*path}`. `into_make_service_with_connect_info` in main.rs gives the handler the peer addr. 55/55 tests including 6 wiremock-driven integration tests that mint a real key and drive the full forwarder. Q4 resolved (trust immediate peer). **Pinned**: in axum 0.8 the `Body::wrap_stream` / `Response::bytes_stream` reqwest APIs are gated behind the `stream` feature; default-features=false hides them. Tests inject `ConnectInfo<SocketAddr>` via `req.extensions_mut().insert(...)` because `Router::oneshot` doesn't carry a peer.

## Semantic (facts about this project)
- **Name**: Pietro (Saint Peter, keeper of the keys).
- **Stack**: axum + tokio + reqwest + sqlx/SQLite + openidconnect + rust-embed (M7) + serde_yaml + clap + tracing.
- **Crate budget in `Cargo.toml`** (after M5):
  - prod (23): axum 0.8, tokio 1, clap 4, serde 1, serde_yaml 0.9, serde_json 1, url 2, regex 1, anyhow 1, thiserror 2, tracing 0.1, tracing-subscriber 0.3, sqlx 0.8, openidconnect 4, reqwest 0.12 (rustls-tls + stream), axum-extra 0.12 (cookie+cookie-signed), cookie 0.18 (key-expansion), base64 0.22, hex 0.4, base32 0.5, rand 0.8, blake3 1, time 0.3, futures-util 0.3.
  - dev (2): tower 0.5, wiremock 0.6.
  - Remaining for later milestones: rust-embed (M7).
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
- **Single binary** (M7-target): `rust-embed` over `frontend/dist/`. Not built yet.
- **`Secret<T>`**: project-local 30-line newtype. Redacts in `Debug`; access via `.expose()`.

## Procedural (how-to for this project)
- Before adding any new feature, re-read §2 in `pietro.md`. YAGNI hard.
- Crates budget: 25 max. After M5 we sit at 23 prod + 2 dev = 25 (at the budget — be deliberate about any future addition).
- Plan first. Confirm. Then code. Never start implementation while open questions in §20 are unanswered.
- All mutating handlers go through `errors::Error::into_response`. Never hand-roll an alternate JSON shape.
- API keys must never appear in logs. `ApiKey`, `MintedKey::Debug`, `Secret<T>::Debug` all redact. Operator credentials injected into headers are marked `set_sensitive(true)` so any future header-walking logging is also safe. The OIDC handler logs `***@domain` for emails.
- Migrations are append-only, numbered, embedded. Never edit a shipped migration.
- Bootstrapping: project skeleton from `cargo init` + `npm create vite@latest`. Trim, don't hand-roll.
- **Dead-code attributes**: after M5, only the `Result` alias in `errors.rs` and `UserId::as_str` carry an `#[allow(dead_code, reason = "...")]`. Both are general-utility items that may or may not be used by M6/M7.
- **Tests against the running binary**: kill stale processes first (`pkill -f target/debug/pietro`) and dodge :8080 on the dev host (the example config now uses :18080).
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
- [ ] M6 — React UI.
- [ ] M7 — Embed + release.
