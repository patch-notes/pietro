# Repository Guidelines

Pietro is an authenticated, streaming API proxy: a single Rust (axum) binary that
embeds a React SPA, handles OIDC login, and forwards requests to configured
upstreams. One binary, one `pietro.yaml`, one SQLite file. See
`docs/architecture.md` for the system reference and `memory.md` for the
living build log.

## Project Structure & Module Organization
- `src/` — Rust backend. Key modules: `main.rs` (CLI + startup), `routes.rs`
  (router + handlers + `AppState`), `proxy.rs` (streaming forwarder), `auth/`
  (OIDC + sessions), `keys.rs`, `db.rs`, `config.rs`, `errors.rs`, `spa.rs`.
- `migrations/` — append-only, numbered SQL. **Never edit a shipped migration**;
  add `migrations/000N_*.sql`.
- `frontend/` — React 19 + TypeScript + Vite + Tailwind v4. Pages in
  `src/pages/` (`Dashboard`, `Login`, `NewKey`); typed API client in `src/api.ts`.
- `build.rs` embeds `frontend/dist/`; `scripts/fake-idp.py` is a dev-only IdP stub.

## Build, Test, and Development Commands
- `cargo build` — debug build (does **not** serve the embedded SPA by design).
- `cargo build --release` — production binary (requires `frontend/dist/`).
- `cargo test` — full suite; the green baseline (currently 62/62) is the contract.
- `cargo run -- migrate --config pietro.yaml` — create/upgrade the DB.
- Frontend: `cd frontend && npm ci && npm run dev` (Vite, proxies `/api`+`/proxy`
  to `:18080`), `npm run build`, `npm run lint`.
- Container: `make docker` (multi-arch podman build); `make help` lists targets.

## Coding Style & Naming Conventions
- Rust 2024 edition, 4-space indent, `snake_case`. Model roles as distinct types
  (`ApiKey`/`ApiKeyHash`/`KeyId`, `AuthenticatedUser`), never booleans.
- Vocabulary is fixed: Operator, User, Caller, Service, Key — never alias.
- All error responses use `errors::Error::into_response`
  (`{ "error": { "code", "message" } }`). Never hand-roll another shape.
- Secrets never hit logs; new deps need a justifying comment in `Cargo.toml`.

## Testing Guidelines
Gate **every** change: `cargo fmt --all -- --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`. For
frontend changes also run `npm run lint` and `npm run build`. Kill stale binaries
(`pkill -f target/release/pietro`) before manual smoke tests.

## Commit & Pull Request Guidelines
Small, focused commits (one concern each); imperative subjects. Remotes: `gitea`
(private origin of record) and `origin` (public GitHub mirror). PRs should note
which gates were run and link relevant `docs/architecture.md`/`memory.md` context.

## Architecture Overview
`docs/architecture.md` is the single system reference: the source map, HTTP
surface, design decisions in force, dependency budgets, the run-end-to-end
smoke recipe, and the containerization/release story. Read it before making
structural changes. Two remotes: `gitea` (private origin of record) and
`origin` (public GitHub mirror).
