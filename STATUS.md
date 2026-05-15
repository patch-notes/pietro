# Pietro — Current State (checkpoint 2026-05-15, M7)

> One-page resumption snapshot. For deep context read `pietro.md` (the design
> plan, locked) and `memory.md` (the agent memory). For day-to-day work, this
> page is enough.

## TL;DR

**Pietro v1 is complete.** All seven milestones shipped. The single binary
serves the React SPA, the API, and the streaming proxy — exactly the §4
mental model. The test suite is **60/60 green** on the Rust side; the
frontend is lint-clean (eslint, tsc) and build-clean (`vite build` → 243 KB
JS / 77 KB gzipped, under the §14.2 200 KB-gz target). Release binary is
**11 MB stripped**, well under the §13 15–25 MB target. A GitHub Actions
workflow builds tagged musl tarballs for x86_64 and aarch64.

```
✅ M1 — Skeleton          (9 tests)
✅ M2 — DB + migrations   (11 tests)
✅ M3 — OIDC login        (24 tests)
✅ M4 — Key lifecycle     (40 tests)
✅ M5 — Proxy             (55 tests)
✅ M6 — React UI          (243 KB JS / 77 KB gz)
✅ M7 — Embed + release   (60 tests / 11 MB stripped musl)
```

Git is clean. `pietro.db*` files are not tracked and can be wiped — a fresh
`cargo run -- migrate --config pietro.yaml` recreates them.

## Verify the project is healthy

```bash
cd /var/home/exe/workz/pietro
cargo fmt --all -- --check    # → clean
cargo clippy --all-targets --all-features -- -D warnings  # → clean
cargo build                   # → clean (debug; SPA served as dev notice)
cargo test                    # → 60 passed; 0 failed
cargo build --release         # → builds the embedded-SPA binary
                              #   (requires frontend/dist/index.html)

cd frontend
npm run lint                  # → clean
npm run build                 # → clean; bundle under 200 KB gz

git status                    # → nothing to commit
git log --oneline             # → ~18 commits, one per milestone + sub-steps
```

## Run the release binary end to end (no real IdP needed)

```bash
# 0. one-time: build the SPA + release binary
cd frontend && npm ci && npm run build && cd ..
cargo build --release

# 1. dev IdP stub (still needed — OIDC discovery happens at startup)
python3 scripts/fake-idp.py 19000 &

# 2. env (the cookie key and pepper are 64 hex chars = 32 bytes each)
export PIETRO_COOKIE_KEY=$(openssl rand -hex 32)
export PIETRO_API_KEY_PEPPER=$(openssl rand -hex 32)
export PIETRO_OIDC_CLIENT_SECRET=dev
export OPENAI_API_KEY=sk-test

# 3. always kill the previous run first; stale binaries lie convincingly
pkill -f 'target/release/pietro' 2>/dev/null
./target/release/pietro serve --config pietro.yaml &

# 4. smoke (single binary serves API + SPA + assets)
curl -s http://127.0.0.1:18080/healthz            # → "ok"
curl -s -o /dev/null -w "%{http_code} %{content_type}\n" \
     http://127.0.0.1:18080/                      # → 200 text/html (real SPA)
curl -s -o /dev/null -w "%{http_code}\n" \
     http://127.0.0.1:18080/some/spa/route        # → 200 (history-mode → index.html)
curl -s http://127.0.0.1:18080/api/garbage        # → 404 JSON envelope
curl -s http://127.0.0.1:18080/api/me             # → 401 JSON envelope
curl -i http://127.0.0.1:18080/api/auth/login     # → 303 to fake-idp
curl -X POST http://127.0.0.1:18080/api/auth/logout  # → 204
curl -s http://127.0.0.1:18080/proxy/openai/v1/x  # → 401 JSON envelope
```

The authenticated paths (mint a real key, then exercise `/proxy/...`) are
covered by the test suite via wiremock upstreams — see `routes::tests::proxy_*`
in `src/routes.rs`. Driving them by hand against the running binary would
require minting a key via the OIDC callback path with a real Keycloak.

### Debug mode

`cargo build` (without `--release`) intentionally does **not** serve the
embedded SPA. Any non-API path returns a "you're in a debug build" notice
page; the React app is meant to be served by Vite (`cd frontend && npm run
dev`, typically `:5173` or `:5174`). This kills the "I'm staring at
four-day-old embedded bytes" trap.

## Source map

| Module | Role | Touched by |
|---|---|---|
| `src/main.rs` | clap CLI, runtime, startup wiring (config → DB → OIDC discovery → pepper → proxy client → usage flusher → axum::serve) | every milestone |
| `src/config.rs` | YAML load + `${VAR}` interpolation + structural validation; `decode_key_material` helper | M1 + M3 (key decode) + M5 (`ServiceId::from_str_for_tests`) |
| `src/secret.rs` | `Secret<T>` newtype: redacts in `Debug`, `.expose()` to read | M1 |
| `src/db.rs` | sqlx pool (WAL, foreign_keys, busy_timeout) + embedded migrator | M2 |
| `src/errors.rs` | One `Error` enum, one `IntoResponse`, one JSON shape: `{ "error": { "code", "message" } }` | M3 (initial), M4/M5 (variants consumed) |
| `src/auth/mod.rs` | aggregator | M3 |
| `src/auth/session.rs` | DB-backed session row + `AuthenticatedUser` extractor + cookie builders | M3 |
| `src/auth/oidc.rs` | OIDC discovery + login + callback + logout; email allowlist; PKCE + state + nonce | M3 |
| `src/keys.rs` | `ApiKey` / `ApiKeyHash` / `KeyId` newtypes + `mint` / `list_for_user` / `revoke` / `verify` / `mark_used` | M4 (initial), M5 (verify + mark_used consumed) |
| `src/proxy.rs` | The whole forwarder + `UsageBatcher` + `run_usage_flusher` | M5 |
| `src/spa.rs` | `rust-embed`-backed SPA handlers (`serve_asset`, `fallback`) + content-type table + dev-mode notice | **M7** |
| `src/routes.rs` | Router assembly + all handlers (`/healthz`, `/api/auth/*`, `/api/me`, `/api/services`, `/api/keys*`, `/proxy/...`, `/assets/*`, fallback) + `AppState` | every milestone |
| `build.rs` | Re-runs on `frontend/dist/index.html` + `frontend/dist/assets/`; fails release builds when index.html is missing, warns in debug | **M7** |
| `migrations/0001_init.sql` | Three tables + the partial unique index that enforces Q5 | M2; **never edit, only append** |
| `scripts/fake-idp.py` | Dev-only IdP stub (discovery + empty JWKS) — not for prod | M3 |
| `frontend/vite.config.ts` | `@tailwindcss/vite` plugin + dev-proxy for `/api` and `/proxy` → `:18080` | M6 |
| `frontend/src/index.css` | One line: `@import "tailwindcss";` — the entire CSS toolchain | M6 |
| `frontend/src/api.ts` | Typed fetch wrapper + `ApiError` envelope parser + endpoint helpers | M6 |
| `frontend/src/App.tsx` | Router + three-state session probe (`loading | out | in`) + `RequireAuth` guard | M6 |
| `frontend/src/pages/Login.tsx` | `<a href="/api/auth/login">` splash | M6 |
| `frontend/src/pages/Dashboard.tsx` | List keys (active + revoked, dim'd), optimistic revoke, sign-out | M6 |
| `frontend/src/pages/NewKey.tsx` | Mint form → plaintext-once reveal with copy + acknowledged exit | M6 |
| `.github/workflows/release.yml` | Tag-triggered musl release: x86_64 (host `musl-tools`) + aarch64 (`cross`) tarballs + `.sha256`, uploaded via `softprops/action-gh-release@v2` | **M7** |

## HTTP surface (what's live today)

| Method | Path | Notes |
|---|---|---|
| GET | `/healthz` | 200 if pool `SELECT 1` passes, 503 otherwise |
| GET | `/api/auth/login` | 303 to the IdP authorize URL; PKCE + state + nonce in `pietro_flow` cookie |
| GET | `/api/auth/callback` | state/code/ID token + email allowlist + user upsert + session row + `pietro_session` cookie; 303 home |
| POST | `/api/auth/logout` | DB delete + `pietro_session` cleared; 204 |
| GET | `/api/me` | session-guarded: `{ user_id, email, display_name }` |
| GET | `/api/services` | session-guarded: `[{ id, display_name, description }]` — never upstream URL or auth |
| GET | `/api/keys` | session-guarded: current user's keys (no plaintext, no hash) |
| POST | `/api/keys` | mint with plaintext-once contract; 409 `key_already_exists` on dup |
| DELETE | `/api/keys/{key_id}` | soft revoke; 204 success / 404 not active or not owned |
| ANY | `/proxy/{service_id}/{*path}` | bearer-authed; streaming; auth injected; hop-by-hop stripped; XFF; status+body pass-through |
| GET | `/assets/{*path}` | **M7** — embedded SPA static files (real-miss → JSON-404; no SPA fallback under `/assets/`) |
| ANY | fallback | **M7** — SPA `index.html` for unknown non-API paths (history-mode); JSON-404 envelope for unknown `/api/*` and `/proxy/*` paths |

Error responses everywhere are `{ "error": { "code": "<machine>", "message": "<human>" } }`.

## Design decisions still in force

Read `pietro.md` for the full statement. Quick reference:

- **One binary, one config file, one DB file.** No watching, no hot reload.
  After M7 the binary literally contains the SPA bundle.
- **YAGNI hard** on multi-tenancy, rate limiting, body transforms,
  WebSockets, plugins, HA, admin-edit-YAML UI, token refresh.
- **One active key per (user, service).** Partial unique index in SQLite is
  the enforcer; the app layer maps the constraint violation to 409 with
  code `key_already_exists`. No silent auto-revoke.
- **Plaintext leak surface: exactly two places.** Mint response (once), and
  the bearer header on the proxy hot path (hashed immediately).
- **Stream everything in the proxy.** No body buffering — a 10 GB upload
  doesn't OOM Pietro.
- **Operator credentials overwrite caller-supplied auth headers** with a
  one-line warn log (`proxy.header_overwritten` + service_id + header_name;
  values never logged; inserted HeaderValue is `set_sensitive(true)`).
- **Email-only allowlist for OIDC.** No groups in v1.
- **Per-cookie logout only.** "Log out everywhere" is v2.
- **Trust the immediate peer for XFF.** If you run behind multiple proxies,
  configure your TLS terminator to set the chain.
- **§7 carve-out (M7):** the SPA fallback never swallows `/api/*` or
  `/proxy/*`. Unknown paths under those prefixes return the project
  JSON-404 envelope.
- **Debug builds never serve embedded bytes.** They return a hostname-
  agnostic notice page so stale bundles can't silently win.

## Crate budget

24 prod + 2 dev = **26** — over the §5 ceiling of 25 by **one** (rust-embed,
M7). The plan reserved exactly this slot at the end of M6, so we're at the
agreed final count for v1. Any further dep needs explicit budget
justification.

## Frontend npm dep budget

Runtime: `react`, `react-dom`, `react-router-dom`. **Three.** Adding a
fourth needs explicit justification — the moment a state-management or
form library shows up, ask first.

## Releases

Tag-triggered:

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions then:

1. Builds the SPA (`npm ci && npm run build`).
2. Builds `target/{x86_64,aarch64}-unknown-linux-musl/release/pietro`
   (x86_64 with host `musl-tools`; aarch64 with `cross`).
3. Packages `pietro-vX.Y.Z-{target}.tar.gz` + `.sha256`.
4. Uploads to the GitHub Release.

Tags containing a hyphen (`v0.1.0-rc1`) get `prerelease: true` automatically.

## Notes for the agent picking this up

1. **Read `memory.md` first.** Top-of-file "How to resume" pointer is
   there. The rest is project knowledge organised as
   Episodic / Semantic / Procedural.
2. **Read `pietro.md` second**, especially §2 (goals / non-goals) and §14
   (UI structure).
3. **Run `cargo test` to verify the baseline.** 60/60 green is the
   contract.
4. **Don't touch shipped migrations.** Add `migrations/0002_*.sql` if you
   need schema changes.
5. **Don't add deps without justification.** Each new entry in
   `Cargo.toml` should have a comment explaining why it's there. The
   crate budget is now at 25 — adding a 26th needs a real reason.
6. **When the running binary's behaviour confuses you, kill stale processes
   first.** `pkill -f target/release/pietro` (or `target/debug/pietro`)
   is in the M4 pinned-learning list for a reason.
7. **Debug build doesn't serve the SPA.** This is deliberate (M7). Use
   `cargo build --release` to test the embedded bundle; use `npm run dev`
   for SPA work.

🙏 *Soli Deo gloria. Pietro v1 stands complete — keeper of the keys.*
