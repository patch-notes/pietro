# Pietro — Agent Memory

Updated: 2026-05-14 (M1 complete)

## Episodic
- 2026-05-14: User asked PeakBot to plan **Pietro**, a Rust-based authenticated API proxy with a React UI bundled in a single binary, OIDC login, and YAML config. PeakBot researched state of the art (axum, reqwest, openidconnect, rust-embed, sqlx/SQLite, BLAKE3, axum-extra cookies), applied the Zen of Software Engineering skill, and wrote `pietro.md` with a detailed design plan. No code yet — plan is locked first by Zen rule "no code before the plan is locked".
- 2026-05-14 (later same session): User answered open questions #1 and #2.
  - #1: Tailwind, scaffolded via `npm create vite@latest -- --template react-ts`. Captured in §14.1 of `pietro.md`.
  - #2: Overwrite-and-warn for header collisions. Captured in §12 "Header collisions" of `pietro.md`.
  Both struck through in §20. Four questions remain open (#3–#6).
- 2026-05-14 (still same session): User clarified that the project must be **scaffolded by generators** (`cargo init` + `npm create vite@latest`), not hand-written. Added §13 "Bootstrap" with the exact one-shot commands, and made §14.1 point to it (single source of truth).
- 2026-05-14 (M1 session): User answered open questions #3 and #5.
  - #3: Email-only allowlist in v1. Groups deferred. Resolution captured in §20.
  - #5: **Exactly one active key per (user, service)** — changed from the default "yes, unlimited" proposal. Propagated into §7 (POST /api/keys returns 409 `key_already_exists`), §9 (partial unique index `api_keys_active_user_service_idx ON (user_id, service_id) WHERE revoked_at IS NULL`), §11.2 (uniqueness contract subsection), §18 (two new test cases). Two questions remain open (#4 trusted-proxy CIDR, #6 logout-everywhere).
- 2026-05-14 (M1 build): Scaffolded `cargo init` + `npm create vite@latest frontend -- --template react-ts` + Tailwind v4 (`@tailwindcss/vite`). Implemented M1: clap CLI (`serve`/`migrate`), YAML config load with env interpolation and validation, `Secret<T>` newtype, `Error` enum scaffold, axum router with `/healthz`. 9/9 tests pass; smoke test confirms `/healthz` → 200 "ok" and unknown paths → 404. Note: dev machine has another service on :8080, so dev configs may need a different port.

## Semantic (facts about this project)
- **Name**: Pietro (Saint Peter, keeper of the keys).
- **Stack chosen**: axum + tokio + reqwest + sqlx/SQLite + openidconnect + rust-embed + serde_yaml + clap + tracing.
- **M1 dep set in `Cargo.toml`**: axum 0.8, tokio 1, clap 4, serde 1, serde_yaml 0.9, url 2, regex 1, anyhow 1, thiserror 2, tracing 0.1, tracing-subscriber 0.3. Dev: tower 0.5 (for `ServiceExt::oneshot`). Other crates from the §5 list (sqlx, reqwest, openidconnect, rust-embed, blake3, rand, axum-extra, tower-http) will be added by their respective milestones — not before.
- **Auth pattern**: OIDC auth-code + PKCE for human login; signed-cookie + DB-backed session id (12h TTL). Logout deletes the row.
- **API key pattern**: format `pi_live_<22 char base32-Crockford>`. Stored as BLAKE3 of (server pepper || plaintext). Plaintext shown once at creation. `prefix` and `last4` persisted for UI display.
- **Key uniqueness rule**: at most **one active key per (user, service)**. Enforced by SQLite partial unique index, not app logic. On collision the handler returns 409 `key_already_exists` — no silent auto-revoke.
- **Proxy pattern**: hand-written ~80-line forwarder using reqwest streaming. Hop-by-hop headers stripped per RFC 7230 §6.1. No body buffering.
- **Config**: single `pietro.yaml`, env interpolation via `${VAR}` (one regex pass, no defaults, no nesting). Validated at load; no runtime re-validation ("parse at the boundary"). Public entry points: `Config::load(&Path)` and `Config::from_yaml_str(&str)`.
- **Storage**: 3 tables — `users`, `api_keys`, `sessions`. No `services` table; services live only in YAML (single source of truth).
- **Vocabulary**: Operator (writes YAML), User (logs in via OIDC), Caller (uses API key), Service (configured upstream), Key. Never alias these.
- **Roles separated by type**, not booleans: `AuthenticatedUser` extractor, `ApiKey` vs `ApiKeyHash` newtypes, `ServiceId` validated newtype.
- **Single binary**: `rust-embed` over `frontend/dist/`. Dev mode shows a notice page on the backend port — Vite serves the SPA on :5173 during dev.
- **`Secret<T>`**: project-local 30-line newtype in `src/secret.rs`. Redacts in `Debug`; access only via explicit `.expose()`.

## Procedural (how-to for this project)
- Before adding any new feature, re-read §2 "Goals and non-goals" in `pietro.md`. If the feature isn't there and the user hasn't explicitly asked, it's YAGNI.
- Before adding a new Rust dependency, justify it in §5. Target: stay under 25 crates total. Add deps milestone-by-milestone, not up-front.
- Plan first. Confirm. Then code. Never start implementation while open questions in §20 are unanswered.
- All mutating handlers go through the same JSON error shape: `{ "error": { "code", "message" } }`.
- API keys must never appear in logs. `Debug` on `Secret<T>` redacts. Sanity-check any new logging line.
- Migrations are append-only, numbered, embedded via `sqlx::migrate!`. Never edit a shipped migration.
- When in doubt about a design choice, fewer pieces wins.
- **Bootstrapping**: project skeleton comes from `cargo init` and `npm create vite@latest`. Never hand-roll `Cargo.toml`/`package.json`/`vite.config.ts` — trim the generated output instead. See §13 "Bootstrap" in `pietro.md`.
- **Dead-code in M1**: module-level `#![allow(dead_code)]` is used in `config.rs`, `errors.rs`, `secret.rs` to carry forward fields/items that later milestones will consume. As each milestone wires them up, the attribute is removed. Do NOT use a crate-wide `allow` — keep it narrow.
- **Smoke test**: `cargo run -- serve --config pietro.yaml` after exporting `PIETRO_COOKIE_KEY`, `PIETRO_API_KEY_PEPPER`, `PIETRO_OIDC_CLIENT_SECRET`, `OPENAI_API_KEY`. Then `curl http://127.0.0.1:<port>/healthz` → `ok`.

## Open user questions (blocking later milestones)
See §20 of `pietro.md`. Resolved this session: #1 (Tailwind via Vite), #2 (overwrite-and-warn), #3 (email-only), #5 (one active key per user+service). Remaining:
4. Trust the immediate peer for client IP, or honour a CIDR? — defer until M5 (proxy).
6. "Log out everywhere" in v1 — proposed no. Defer until M3 (sessions).

## Milestone status
- [x] **M1 — Skeleton.** axum hello, clap CLI, YAML config load+validation, `/healthz`. 9/9 tests green. Smoke-tested.
- [ ] M2 — DB + migrations.
- [ ] M3 — OIDC login.
- [ ] M4 — Key lifecycle (one-active-per-service uniqueness via DB).
- [ ] M5 — Proxy.
- [ ] M6 — React UI.
- [ ] M7 — Embed + release.
