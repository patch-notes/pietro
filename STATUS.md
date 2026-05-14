# Pietro — Current State (checkpoint 2026-05-14)

> One-page resumption snapshot. For deep context read `pietro.md` (the design
> plan, locked) and `memory.md` (the agent memory). For day-to-day work, this
> page is enough.

## TL;DR

Five of seven milestones shipped. The backend is feature-complete for v1.
What remains is the UI (M6) and packaging (M7). The test suite is **55/55
green**, no compiler warnings, no half-states, no open design questions.

```
✅ M1 — Skeleton          (9 tests)
✅ M2 — DB + migrations   (11 tests)
✅ M3 — OIDC login        (24 tests)
✅ M4 — Key lifecycle     (40 tests)
✅ M5 — Proxy             (55 tests)
⬜ M6 — React UI
⬜ M7 — Embed + release
```

Git is clean. `pietro.db*` files are not tracked and have been wiped — a
fresh `cargo run -- migrate --config pietro.yaml` recreates them.

## Verify the project is healthy

```bash
cd /var/home/exe/workz/pietro
cargo build           # → clean, no warnings
cargo test            # → 55 passed; 0 failed
git status            # → nothing to commit
git log --oneline     # → 6 commits, one per milestone (+ one doc commit)
```

## Run it end to end (no real IdP needed)

```bash
# 1. dev IdP stub
python3 scripts/fake-idp.py 19000 &

# 2. env (the cookie key and pepper are 64 hex chars = 32 bytes each)
export PIETRO_COOKIE_KEY=$(openssl rand -hex 32)
export PIETRO_API_KEY_PEPPER=$(openssl rand -hex 32)
export PIETRO_OIDC_CLIENT_SECRET=dev
export OPENAI_API_KEY=sk-test

# 3. always kill the previous run first; stale binaries lie convincingly
pkill -f 'target/debug/pietro' 2>/dev/null
cargo run -- serve --config pietro.yaml &

# 4. smoke
curl -s http://127.0.0.1:18080/healthz                         # → "ok"
curl -s http://127.0.0.1:18080/api/me        # → 401 JSON
curl -i http://127.0.0.1:18080/api/auth/login                  # → 303 to fake-idp
curl -X POST http://127.0.0.1:18080/api/auth/logout            # → 204
curl http://127.0.0.1:18080/proxy/openai/v1/x                  # → 401 JSON
```

The authenticated paths (mint a real key, then exercise `/proxy/...`) are
covered by the test suite via wiremock upstreams — see
`routes::tests::proxy_*` in `src/routes.rs`. Driving them by hand against
the running binary would require minting a key via `cargo run`'s OIDC
callback path with a real Keycloak.

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
| `src/routes.rs` | Router assembly + all handlers (`/healthz`, `/api/auth/*`, `/api/me`, `/api/services`, `/api/keys*`, `/proxy/...`) + `AppState` | every milestone |
| `migrations/0001_init.sql` | Three tables + the partial unique index that enforces Q5 | M2; **never edit, only append** |
| `scripts/fake-idp.py` | Dev-only IdP stub (discovery + empty JWKS) — not for prod | M3 |
| `frontend/` | Vite + React + TS scaffold, Tailwind v4 wired into vite.config.ts. Not yet a real UI. | M1 bootstrap |

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

Error responses everywhere are `{ "error": { "code": "<machine>", "message": "<human>" } }`.

## Design decisions still in force

Read `pietro.md` for the full statement. Quick reference:

- **One binary, one config file, one DB file.** No watching, no hot reload.
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

## Crate budget

23 prod + 2 dev = **25**, exactly at the §5 ceiling. Future milestones add:
- **M7** — `rust-embed` (+1, takes us to 24 prod). Anything beyond that
  needs explicit budget justification.

## What's next: M6 — React UI

Three pages (`/`, `/new`, `/login`) per §14, fetch wrapper, login redirect on
401. Builds via Vite into `frontend/dist/`. The backend HTTP surface above is
exactly the shape the UI needs. Until M7 nothing touches Rust.

When you start M6, the entry points are:
- `frontend/src/main.tsx` (Vite template; replace the demo App)
- `frontend/src/App.tsx`
- `frontend/vite.config.ts` (already has Tailwind v4 plugin wired)
- `frontend/src/index.css` (`@import "tailwindcss";`)

Run `cd frontend && npm run dev` for the dev server on :5173. It already
proxies `/api/*` and `/proxy/*` to the Rust backend on :18080 per the
config in `vite.config.ts` — wait, actually that's the *plan*. Check
`vite.config.ts` before relying on it; the proxy may need to be wired up
as part of M6's first task.

## Notes for the agent picking this up

1. **Read `memory.md` first.** Top-of-file "How to resume" pointer is
   there. The rest is project knowledge organised as
   Episodic / Semantic / Procedural.
2. **Read `pietro.md` second**, especially §2 (goals / non-goals) and §14
   (UI structure).
3. **Run `cargo test` to verify the baseline.** 55/55 green is the
   contract.
4. **Don't touch shipped migrations.** Add `migrations/0002_*.sql` if you
   need schema changes.
5. **Don't add deps without justification.** Each new entry in
   `Cargo.toml` should have a comment explaining why it's there.
6. **When the running binary's behaviour confuses you, kill stale processes
   first.** `pkill -f target/debug/pietro` is in the M4 pinned-learning
   list for a reason.

🙏 *Soli Deo gloria.*
