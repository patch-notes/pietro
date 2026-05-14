# Pietro — Agent Memory

Updated: 2026-05-14 (M4 complete)

## Episodic
- 2026-05-14: User asked PeakBot to plan **Pietro**, a Rust-based authenticated API proxy with a React UI bundled in a single binary, OIDC login, and YAML config. PeakBot researched state of the art (axum, reqwest, openidconnect, rust-embed, sqlx/SQLite, BLAKE3, axum-extra cookies), applied the Zen of Software Engineering skill, and wrote `pietro.md` with a detailed design plan. No code yet — plan is locked first by Zen rule "no code before the plan is locked".
- 2026-05-14 (later same session): User answered open questions #1 and #2.
  - #1: Tailwind, scaffolded via `npm create vite@latest -- --template react-ts`. Captured in §14.1 of `pietro.md`.
  - #2: Overwrite-and-warn for header collisions. Captured in §12 "Header collisions" of `pietro.md`.
- 2026-05-14 (still same session): User clarified that the project must be **scaffolded by generators** (`cargo init` + `npm create vite@latest`), not hand-written. Added §13 "Bootstrap" with the exact one-shot commands, and made §14.1 point to it (single source of truth).
- 2026-05-14 (M1 session): User answered open questions #3 and #5.
  - #3: Email-only allowlist in v1. Groups deferred. Resolution captured in §20.
  - #5: **Exactly one active key per (user, service)** — changed from the default "yes, unlimited" proposal. Propagated into §7 (POST /api/keys returns 409 `key_already_exists`), §9 (partial unique index `api_keys_active_user_service_idx ON (user_id, service_id) WHERE revoked_at IS NULL`), §11.2 (uniqueness contract subsection), §18 (two new test cases). Two questions remain open (#4 trusted-proxy CIDR, #6 logout-everywhere).
- 2026-05-14 (M1 build): Scaffolded `cargo init` + `npm create vite@latest frontend -- --template react-ts` + Tailwind v4 (`@tailwindcss/vite`). Implemented M1: clap CLI (`serve`/`migrate`), YAML config load with env interpolation and validation, `Secret<T>` newtype, `Error` enum scaffold, axum router with `/healthz`. 9/9 tests pass; smoke test confirms `/healthz` → 200 "ok" and unknown paths → 404. Note: dev machine has another service on :8080, so dev configs may need a different port.
- 2026-05-14 (M2 build): Added sqlx (sqlite/macros/migrate, runtime-tokio, no TLS, default-features off). Authored `migrations/0001_init.sql` with the §9 schema verbatim — three tables and the partial unique index that enforces the Q5 rule. Implemented `src/db.rs` (WAL + foreign_keys + busy_timeout, max_connections=8). Wired `pietro migrate` to call `db::migrate` (idempotent) and `pietro serve` to call `db::connect` (open + migrate at startup). Upgraded `/healthz` from a static `"ok"` to a `SELECT 1` against the pool (503 on failure). Tests now 11/11 — including `db::tests::active_user_service_uniqueness_is_enforced`, the regression test for Q5. Dev config moved to `:18080` to dodge another service squatting on `:8080`.
- 2026-05-14 (M3 build): Added openidconnect 4.0.1, reqwest 0.12 (rustls-tls), axum-extra 0.12 (cookie + cookie-signed), cookie 0.18 (with `key-expansion` for `Key::derive_from`), base64, hex, rand, time, serde_json. Wrote `src/auth/{mod,session,oidc}.rs`. Implemented `AuthenticatedUser` extractor (FromRequestParts), `OidcState::from_config` (discover at startup; fail fast if unreachable), `/api/auth/login` (PKCE + flow cookie), `/api/auth/callback` (state check, code exchange, ID token verification, email allowlist, user upsert, session creation), `/api/auth/logout` (DB delete + cookie clear), and `/api/me` (session-guarded). `errors.rs` now has `IntoResponse` mapping every variant to the single JSON shape `{ "error": { "code", "message" } }`. Tests now 24/24 — including a wiremock-based real-discovery test of the login redirect. Smoke-tested with a tiny Python `scripts/fake-idp.py` and `curl`: `/healthz`, `/api/me` (401 JSON), `/api/auth/login` (303 to IdP with PKCE/state/nonce/scope), `/api/auth/logout` (204). Pinned learning: the openidconnect `CoreClient` type produced by `from_provider_metadata` has six `Endpoint*` type parameters; `EndpointMaybeSet` for userinfo/token come specifically from `from_provider_metadata` and can't be reconstructed via `CoreClient::new` + setters in 4.x. Test scaffolding has to go through `from_provider_metadata` with hand-built `CoreProviderMetadata` to match the production type.
- 2026-05-14 (M4 build): Added `blake3` 1 and `base32` 0.5 (Crockford). Wrote `src/keys.rs` — `ApiKey` (Secret-wrapped, no Debug), `ApiKeyHash` ([u8; 32], opaque), `KeyId` (parsed at the boundary), `MintedKey` (hand-rolled Debug that redacts plaintext), plus `mint`/`list_for_user`/`revoke`/`verify`/`mark_used`. Mapped SQLite UNIQUE-constraint violations on `api_keys_active_user_service_idx` to `Error::Conflict("key_already_exists")` — handler turns that into 409 per §11.2. `AppState` grew a `pepper: Arc<Vec<u8>>` field decoded from `cfg.api_key_pepper` once at startup. Four new routes: `GET /api/services` (id + display_name + description only — no upstream URL, no creds), `GET /api/keys`, `POST /api/keys` (plaintext returned once with 201), `DELETE /api/keys/{key_id}` (soft-revoke; 404 if not active or not owned). 16 new tests (11 in `keys::tests`, 5 router-level using a real signed session cookie produced via `cookie::CookieJar::signed_mut` — no test-only auth bypass). 40/40 green. Live-server smoke against the running M4 binary confirmed every M4 endpoint returns 401 + project JSON shape when unauthenticated. **Pinned learning**: `axum::extract::Path<String>` requires the `{name}` syntax in axum 0.8 route patterns (not `:name`); I caught a stale running pietro the hard way when 404s came back from a route I'd just added — `pkill -f target/debug/pietro` before each smoke or you'll be confused.

## Semantic (facts about this project)
- **Name**: Pietro (Saint Peter, keeper of the keys).
- **Stack chosen**: axum + tokio + reqwest + sqlx/SQLite + openidconnect + rust-embed + serde_yaml + clap + tracing.
- **Crate budget in `Cargo.toml`** (after M4):
  - prod (22): axum 0.8, tokio 1, clap 4, serde 1, serde_yaml 0.9, serde_json 1, url 2, regex 1, anyhow 1, thiserror 2, tracing 0.1, tracing-subscriber 0.3, sqlx 0.8, openidconnect 4, reqwest 0.12, axum-extra 0.12, cookie 0.18, base64 0.22, hex 0.4, base32 0.5, rand 0.8, blake3 1, time 0.3.
  - dev (2): tower 0.5, wiremock 0.6.
  - Remaining for later milestones: rust-embed (M7), tower-http (TBD).
- **Auth pattern**: OIDC auth-code + PKCE for human login. Signed-cookie + DB-backed session id (12h TTL via `SESSION_TTL`). Logout deletes the row. Flow state (state + nonce + PKCE verifier) lives in a short-lived 5-minute `pietro_flow` signed cookie scoped to `/api/auth/`.
- **OIDC client type pinning**: `PietroOidcClient = CoreClient<EndpointSet, EndpointNotSet*3, EndpointMaybeSet*2>` — produced by `from_provider_metadata`. Don't try to assemble via `CoreClient::new` + setters; the types won't line up.
- **API key pattern**: format `pi_live_<22 char base32-Crockford>` (actually 23 chars from 14 random bytes — we keep the natural length rather than truncating). Stored as BLAKE3 of (server pepper || plaintext). Plaintext shown once at creation. `prefix` (12 chars) and `last4` persisted for UI display.
- **`KeyId` pattern**: `pi_<7 char base32-Crockford>` — used in URLs and as the public reference. Parsed at the boundary via `KeyId::parse` (rejects garbage, non-`pi_` prefixes, non-alphanumeric characters).
- **API key uniqueness rule**: at most **one active key per (user, service)**. Enforced by SQLite partial unique index `api_keys_active_user_service_idx`, not app logic. On collision the handler returns 409 `key_already_exists` with the project JSON shape — no silent auto-revoke. Verified by `db::tests::active_user_service_uniqueness_is_enforced`, `keys::tests::second_active_key_for_same_service_yields_409`, and `routes::tests::mint_returns_plaintext_once_and_409_on_dup` (three layers of regression coverage).
- **Pepper handling**: `cfg.api_key_pepper` is decoded with `config::decode_key_material` at startup, stored as `Arc<Vec<u8>>` in `AppState`, and only passed by reference to `keys::mint` / `keys::verify`. Rotating the pepper invalidates every existing key — by design, documented in the example config.
- **Plaintext leak surface**: exactly two places. (1) The response body of `POST /api/keys` (returned once, then dropped). (2) The argument to `keys::verify` on the proxy hot path (hashed immediately, never stored). `ApiKey` has no `Debug` impl; `MintedKey::fmt::Debug` redacts the plaintext field.
- **Proxy pattern** (M5, not yet built): hand-written ~80-line forwarder using reqwest streaming. Hop-by-hop headers stripped per RFC 7230 §6.1. No body buffering.
- **Config**: single `pietro.yaml`, env interpolation via `${VAR}` (one regex pass, no defaults, no nesting). Validated at load; no runtime re-validation ("parse at the boundary"). Public entry points: `Config::load(&Path)` and `Config::from_yaml_str(&str)`. Key material decoder: `config::decode_key_material(&str) -> Option<Vec<u8>>` tries hex, then base64, then raw bytes.
- **Cookie signing**: `axum-extra::extract::cookie::Key::derive_from(&master)` — requires `cookie` crate with the `key-expansion` feature (added as a direct dep alongside axum-extra; axum-extra doesn't enable it on its own).
- **Error model**: `errors::Error` enum + `IntoResponse` produces `{ "error": { "code": "<machine>", "message": "<human>" } }`. `Error::Internal` is logged at `error!` level and surfaces only as a generic "internal server error" externally — never leak the underlying message.
- **Storage**: 3 tables — `users`, `api_keys`, `sessions`. No `services` table; services live only in YAML (single source of truth). Migrations embedded at compile time via `sqlx::migrate!("./migrations")`.
- **SQLite tuning**: `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`, pool `max_connections=8`. Documented in `src/db.rs::open_pool`.
- **SQLite datetime convention**: SQLite's `datetime('now')` returns `"YYYY-MM-DD HH:MM:SS"` (UTC, no tz suffix). `auth::session::format_sqlite_datetime` formats `time::OffsetDateTime` to match so WHERE clauses comparing `expires_at > now()` behave.
- **Vocabulary**: Operator (writes YAML), User (logs in via OIDC), Caller (uses API key), Service (configured upstream), Key. Never alias these.
- **Roles separated by type**, not booleans: `AuthenticatedUser` extractor, `ApiKey` vs `ApiKeyHash` newtypes, `ServiceId` validated newtype, `UserId(String)` from OIDC subject, `KeyId(String)` validated at parse time.
- **Single binary**: `rust-embed` over `frontend/dist/`. Dev mode shows a notice page on the backend port — Vite serves the SPA on :5173 during dev.
- **`Secret<T>`**: project-local 30-line newtype in `src/secret.rs`. Redacts in `Debug`; access only via explicit `.expose()`.

## Procedural (how-to for this project)
- Before adding any new feature, re-read §2 "Goals and non-goals" in `pietro.md`. If the feature isn't there and the user hasn't explicitly asked, it's YAGNI.
- Before adding a new Rust dependency, justify it in §5 / Cargo.toml comments. Target: stay under 25 crates total. Add deps milestone-by-milestone, not up-front. (After M4 we sit at 22 prod + 2 dev = 24.)
- Plan first. Confirm. Then code. Never start implementation while open questions in §20 are unanswered.
- All mutating handlers go through the same JSON error shape: `{ "error": { "code", "message" } }`. Constructed by `errors::Error::into_response`. Never hand-roll an alternate shape.
- API keys must never appear in logs. `ApiKey` has no Debug; `MintedKey::Debug` redacts. `Secret<T>::Debug` redacts. The OIDC handler logs *only* `***@domain` for emails. Sanity-check any new logging line.
- **Migrations are append-only**, numbered, embedded via `sqlx::migrate!`. Never edit a shipped migration — add a new file with the next number.
- When in doubt about a design choice, fewer pieces wins.
- **Bootstrapping**: project skeleton comes from `cargo init` and `npm create vite@latest`. Never hand-roll `Cargo.toml`/`package.json`/`vite.config.ts` — trim the generated output instead. See §13 "Bootstrap" in `pietro.md`.
- **Dead-code attributes**: field- or variant-level `#[allow(dead_code, reason = "...")]` with the milestone number that consumes it. After M4 only `UpstreamTimeout`/`UpstreamUnreachable` (M5), `UserId::as_str` (M5), `Result` alias (future), `keys::verify`, and `keys::mark_used` (M5) still carry an allow. Remove the attribute when the milestone consumes it.
- **OIDC test harness**: full callback (state → ID token verification) is *not* unit-tested. The plan (§19 M3) calls for testing against a real Keycloak in docker-compose, and we keep it that way. The wiremock test in `routes::tests::login_redirects_to_idp_with_pkce_and_state` covers everything else: discovery, redirect target, PKCE/state/nonce/scope, flow cookie. For dev smoke-testing without Keycloak, use `scripts/fake-idp.py` (Python http.server stub serving discovery + an empty JWKS).
- **Session cookie in tests**: the M4 router tests use `cookie::CookieJar::signed_mut(&key).add(Cookie::new(SESSION_COOKIE, sid))` then `jar.get(SESSION_COOKIE).to_string()` to sign the cookie the same way production does. No test-only auth shortcut.
- **Smoke test recipe** (M4):
  ```
  rm -f pietro.db*
  python3 scripts/fake-idp.py 19000 &      # tiny dev IdP
  export PIETRO_COOKIE_KEY=$(openssl rand -hex 32)
  export PIETRO_API_KEY_PEPPER=$(openssl rand -hex 32)
  export PIETRO_OIDC_CLIENT_SECRET=dev
  export OPENAI_API_KEY=sk-test
  cargo build && pkill -f 'target/debug/pietro' || true   # stop any stale binary
  target/debug/pietro serve --config pietro.yaml &        # bind :18080
  curl /healthz                            # → "ok"
  curl /api/services                       # → 401 JSON (session-gated)
  curl /api/keys                           # → 401 JSON
  curl -X POST /api/keys                   # → 401 JSON
  curl -X DELETE /api/keys/pi_xxxxxx       # → 401 JSON
  ```
  All four M4 routes return the project JSON error shape when unauthenticated. To smoke the authenticated paths, see the unit tests in `routes::tests` (they exercise the real signed-cookie + session-extractor + DB path).
- **DB inspection**: `sqlite3 pietro.db ".schema"` confirms the partial unique index `api_keys_active_user_service_idx`. If you ever wonder whether the Q5 rule is wired, that's the cheapest check.

## Open user questions
See §20 of `pietro.md`. Resolved: #1 (Tailwind), #2 (overwrite-and-warn), #3 (email-only), #5 (one active key per user+service), #6 (per-cookie logout). Remaining:
4. Trust the immediate peer for client IP, or honour a CIDR? — defer until M5 (proxy).

## Milestone status
- [x] **M1 — Skeleton.** axum hello, clap CLI, YAML config load+validation, `/healthz`. 9/9 tests green. Smoke-tested.
- [x] **M2 — DB + migrations.** sqlx pool, embedded migrations, `pietro migrate` idempotent, `/healthz` pings the pool. 11/11 tests green. Smoke-tested.
- [x] **M3 — OIDC login.** `/api/auth/{login,callback,logout}` + `/api/me`, signed-cookie sessions, DB-backed, email allowlist, PKCE, flow-cookie stash. 24/24 tests green. Smoke-tested against `scripts/fake-idp.py`.
- [x] **M4 — Key lifecycle.** `/api/services` + `/api/keys` (mint/list/revoke) with the §11.2 uniqueness contract enforced at the DB layer. 40/40 tests green. Smoke-tested (unauthenticated 401 path).
- [ ] M5 — Proxy.
- [ ] M6 — React UI.
- [ ] M7 — Embed + release.
