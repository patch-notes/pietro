# Pietro — Architecture

Reference for the shape of the system: what each module does, the HTTP surface,
the design decisions in force, and how to run and release it. For the full
locked design rationale read `pietro.md`; for the chronological build log read
`memory.md`; for day-to-day contributor conventions read `AGENTS.md`.

## Mental model

Pietro is one Rust (axum) binary that is three things at once:

1. It **serves the React SPA** (embedded via `rust-embed` from `frontend/dist/`).
2. It **exposes the API** (`/api/*`) for OIDC login, session, and key lifecycle.
3. It **proxies** (`/proxy/{service_id}/...`) — authenticating an inbound API
   key, looking up the matching upstream from YAML, injecting the operator's
   credential, and streaming the request/response through.

One binary, one `pietro.yaml`, one SQLite file. No watching, no hot reload.
YAGNI is enforced hard (see `pietro.md` §2 and §21 for the explicit non-goals:
multi-tenancy, rate limiting, body transforms, WebSockets, plugins, HA,
per-route ACLs, token refresh, "log out everywhere").

## Source map

| Module | Role |
|---|---|
| `src/main.rs` | clap CLI, runtime, startup wiring (config → DB → OIDC discovery → pepper → proxy client → usage flusher → `axum::serve`). |
| `src/config.rs` | YAML load + `${VAR}` interpolation + structural validation; `decode_key_material` helper. Enforces `timeout_secs > 0`. |
| `src/secret.rs` | `Secret<T>` newtype: redacts in `Debug`, `.expose()` to read. |
| `src/db.rs` | sqlx pool (WAL, `foreign_keys`, `busy_timeout`) + embedded migrator. |
| `src/errors.rs` | One `Error` enum, one `IntoResponse`, one JSON shape: `{ "error": { "code", "message" } }`. |
| `src/auth/mod.rs` | aggregator. |
| `src/auth/session.rs` | DB-backed session row + `AuthenticatedUser` extractor + cookie builders. |
| `src/auth/oidc.rs` | OIDC discovery + login + callback + logout; email allowlist; PKCE + state + nonce. |
| `src/keys.rs` | `ApiKey` / `ApiKeyHash` / `KeyId` newtypes + `mint` / `list_for_user` / `revoke` / `verify` / `mark_used`. |
| `src/proxy.rs` | The whole forwarder (`forward`, `forward_bare`, `forward_inner`) + per-service timeout + `UsageBatcher` + `run_usage_flusher`. |
| `src/spa.rs` | `rust-embed`-backed SPA handlers (`serve_asset`, `fallback`) + content-type table + dev-mode notice. |
| `src/routes.rs` | Router assembly + all handlers + `AppState`. |
| `build.rs` | Re-runs on `frontend/dist/index.html` + `frontend/dist/assets/`; fails release builds when `index.html` is missing, warns in debug. |
| `migrations/0001_init.sql` | Three tables + the partial unique index that enforces one-active-key-per-(user,service). **Append-only** — never edit; add `000N_*.sql`. |
| `scripts/fake-idp.py` | Dev-only IdP stub (discovery + empty JWKS). |
| `frontend/vite.config.ts` | `@tailwindcss/vite` plugin + dev-proxy for `/api` and `/proxy` → `:18080`. |
| `frontend/src/api.ts` | Typed fetch wrapper + `ApiError` envelope parser + endpoint helpers. |
| `frontend/src/App.tsx` | Router + three-state session probe (`loading \| out \| in`) + `RequireAuth` guard. |
| `frontend/src/pages/Login.tsx` | `<a href="/api/auth/login">` splash. |
| `frontend/src/pages/Dashboard.tsx` | List keys (active + revoked, dim'd), optimistic revoke, per-key proxy endpoint URL, sign-out. |
| `frontend/src/pages/NewKey.tsx` | Mint form → plaintext-once reveal with copy + acknowledged exit. |

## HTTP surface

| Method | Path | Notes |
|---|---|---|
| GET | `/healthz` | 200 if pool `SELECT 1` passes, 503 otherwise. |
| GET | `/api/auth/login` | 303 to IdP authorize URL; PKCE + state + nonce in `pietro_flow` cookie. |
| GET | `/api/auth/callback` | state/code/ID-token + email allowlist + user upsert + session row + `pietro_session` cookie; 303 home. |
| POST | `/api/auth/logout` | DB delete + `pietro_session` cleared; 204 (per-cookie only). |
| GET | `/api/me` | session-guarded: `{ user_id, email, display_name }`. |
| GET | `/api/services` | session-guarded: `[{ id, display_name, description }]` — never upstream URL or auth. |
| GET | `/api/keys` | session-guarded: current user's keys (no plaintext, no hash). |
| POST | `/api/keys` | mint with plaintext-once contract; 409 `key_already_exists` on an active same-service same-label duplicate. |
| DELETE | `/api/keys/{key_id}` | soft revoke; 204 / 404 if not active or not owned. |
| ANY | `/proxy/{service_id}` and `/proxy/{service_id}/` | bare/trailing-slash → upstream root (`forward_bare`). |
| ANY | `/proxy/{service_id}/{*path}` | bearer-authed, streaming forwarder (`forward`); auth injected; hop-by-hop stripped; XFF; status+body pass-through. |
| GET | `/assets/{*path}` | embedded SPA static files (real-miss → JSON-404; no SPA fallback under `/assets/`). |
| ANY | fallback | SPA `index.html` for unknown non-API paths (history-mode); JSON-404 envelope for unknown `/api/*` and `/proxy/*` (§7 carve-out). |

All error responses: `{ "error": { "code": "<machine>", "message": "<human>" } }`.

## Design decisions in force

- **One binary, one config file, one DB file.** After M7 the binary contains the SPA bundle.
- **One active key per (user, service, label).** A partial unique index in SQLite is the enforcer; the app maps the violation to 409 `key_already_exists`. Several concurrent keys for the same service are allowed as long as their labels differ — an exact (service, label) re-mint is the only conflict. No silent auto-revoke. (Loosened from the v1 "one per (user, service)" rule in migration `0002`.)
- **Plaintext leak surface: exactly two places.** Mint response (once), and the bearer header on the proxy hot path (hashed immediately).
- **`service.auth` is optional.** Open upstreams get no injected credential; the caller's own `Authorization` is still stripped (we send operator creds or nothing).
- **`service.timeout_secs` is optional** (per-service upstream timeout, default 60; `0` rejected at config load).
- **Operator credentials overwrite caller-supplied auth headers** with a one-line warn (`proxy.header_overwritten`; values never logged; inserted `HeaderValue` is `set_sensitive(true)`).
- **Stream everything in the proxy.** No body buffering — a 10 GB upload doesn't OOM Pietro.
- **Email-only allowlist for OIDC.** No groups in v1. Per-cookie logout only.
- **Trust the immediate peer for XFF.** Configure your TLS terminator for multi-proxy chains.
- **Debug builds never serve embedded bytes.** They return a hostname-agnostic notice page so stale bundles can't silently win — the dev loop is Vite. Use `cargo build --release` to test the embedded SPA.

## Dependency budgets

- **Rust crates:** 24 prod + 2 dev = **26** (one over the §5 soft ceiling of 25 by design — the plan reserved the `rust-embed` slot for M7). Any further dep needs a justifying comment in `Cargo.toml`.
- **Frontend runtime npm deps: three** — `react`, `react-dom`, `react-router-dom`. A fourth needs explicit justification.

## Milestone status (v1 complete)

All seven milestones shipped (2026-05-15); v1 is complete. Baseline is
**64/64** Rust tests green, frontend lint-/build-clean (bundle ≈ 244 KB JS /
77 KB gz, under the 200 KB-gz target), release binary ≈ 11 MB stripped musl.

```
M1 Skeleton · M2 DB + migrations · M3 OIDC login · M4 Key lifecycle
M5 Proxy · M6 React UI · M7 Embed + release
```

Post-v1 additions (all shipped): optional `service.auth`, per-service
`timeout_secs`, per-key proxy-endpoint URL in the dashboard, bare
`/proxy/{service_id}` routing fix, containerization, and CI publishing.

## Running end to end (no real IdP needed)

```bash
# 0. one-time: build the SPA + release binary
cd frontend && npm ci && npm run build && cd ..
cargo build --release

# 1. dev IdP stub (OIDC discovery happens at startup)
python3 scripts/fake-idp.py 19000 &

# 2. env (cookie key + pepper are 64 hex chars = 32 bytes each)
export PIETRO_COOKIE_KEY=$(openssl rand -hex 32)
export PIETRO_API_KEY_PEPPER=$(openssl rand -hex 32)
export PIETRO_OIDC_CLIENT_SECRET=dev

# 3. kill any previous run first — stale binaries lie convincingly
pkill -f 'target/release/pietro' 2>/dev/null
./target/release/pietro serve --config pietro.yaml &

# 4. smoke
curl -s  http://127.0.0.1:18080/healthz            # → "ok"
curl -so /dev/null -w "%{http_code} %{content_type}\n" http://127.0.0.1:18080/   # → 200 text/html
curl -s  http://127.0.0.1:18080/api/me             # → 401 JSON envelope
```

Authenticated proxy paths are covered by `routes::tests::proxy_*` against
wiremock upstreams. `cargo build` (debug) intentionally serves a notice page
instead of the SPA — run Vite (`cd frontend && npm run dev`) for UI work.

## Containerization & releases

- **Container:** three-stage `Dockerfile` (node → rust-alpine musl → `scratch`), driven by `Makefile` for multi-arch podman builds (`make docker`, `make help`). Default push registry is `ghcr.io/patch-notes`.
- **`.github/workflows/release.yml`:** release-triggered (`v*`) musl tarballs + `.sha256` for x86_64 and aarch64, each built natively on its own GitHub-hosted runner (amd64 on `ubuntu-latest`, arm64 on `ubuntu-24.04-arm`) with `musl-tools`; hyphenated tags become prereleases. Upload via `softprops/action-gh-release@v2`.
- **`.github/workflows/docker-publish.yml`:** multi-arch (amd64+arm64) image, each arch built natively (arm64 on `ubuntu-24.04-arm`) and pushed by digest, then merged into a tagged manifest list. Pushed to `ghcr.io/patch-notes/pietro` on `master` push, `v*` tags, and manual dispatch. Auth via repo-scoped `GITHUB_TOKEN` only.
- **Remote:** `origin` → github.com/patch-notes/pietro.

## Storage

Three tables — `users`, `api_keys`, `sessions`; services live only in YAML.
Migrations are embedded via `sqlx::migrate!("./migrations")`. SQLite tuning:
`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5s`,
pool `max_connections=8`. `pietro.db*` are git- and docker-ignored; a fresh
`cargo run -- migrate --config pietro.yaml` recreates them.
