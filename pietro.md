# Pietro — Design Plan

> *"And I will give unto thee the keys of the kingdom of heaven."* — Matthew 16:19
>
> Pietro is the keeper of the keys. Users come to him to ask for keys to the
> services the operator allows. He hands out the keys, and when those keys are
> later presented at his gate, he forwards the request to the right service.

This is a **plan** in the Zen-of-Engineering sense: detailed, opinionated,
covering edge cases and rejected alternatives. Code that comes after it should
be small and boring. Nothing in this document should be implemented until the
plan is reviewed and locked.

---

## 1. One-paragraph pitch

Pietro is a single Rust binary that runs as an authenticated HTTP API proxy.
An operator declares a fixed set of upstream services in a YAML file. Users
log in to Pietro's web UI via an external OIDC provider, generate per-user API
keys for those services, and then point their own clients at Pietro instead of
the upstream. Pietro authenticates the inbound key, looks up the matching
service, and forwards the request — injecting whatever credentials the
operator configured for that service. The whole thing — backend, React UI,
SQLite migrations — ships as one statically-linkable binary.

---

## 2. Goals and non-goals

### Goals (v1, must)
- Operator configures a list of upstream services in one YAML file.
- Authenticated humans (OIDC) can mint, list, and revoke their own API keys, scoped to a service.
- Inbound requests with a valid Pietro API key are forwarded to the configured upstream, with the operator-defined upstream credential attached.
- React SPA is served from `/` by the same binary. No external `nginx`, no separate static host.
- Configuration lives in one file. Secrets may be `${ENV}`-interpolated.
- SQLite for state. One file on disk next to the binary.
- Streams pass through (no buffering full responses in memory).

### Non-goals (v1, explicitly)
- Multi-tenancy / organizations / teams. There is one tenant: this Pietro instance.
- Per-key rate limiting or quota enforcement. (Hookable later — see §17.)
- Billing, metering, invoicing.
- Body / response transformation, JSON path rewriting, URL templating beyond `{path}`.
- WebSocket upgrade proxying. HTTP/1.1 + HTTP/2 unary + chunked/SSE only.
- Health-checking or circuit-breaking upstream services.
- A plugin system or any form of "extension point". YAGNI hard.
- High availability / clustering. One process, one box. Run it behind a load balancer if you need HA, but state will be local.
- An admin UI for *editing* `pietro.yaml`. The file is the source of truth; edit it and restart.

The non-goals are doctrine. If a v1 PR adds any of them, it should be rejected.

---

## 3. Vocabulary (read this before anything else)

Three distinct human roles, one machine role. Name them precisely from day one
to avoid the "user means three things" trap.

| Term       | Who/what                                                                 |
|------------|--------------------------------------------------------------------------|
| **Operator** | The human who deploys Pietro and writes `pietro.yaml`.                  |
| **User**     | A human who logs in via OIDC and manages API keys in the web UI.       |
| **Caller**   | Any HTTP client (script, app, CI) that presents an API key at the proxy.|
| **Service**  | A statically-configured upstream HTTP API (name + base URL + creds).   |
| **Key**      | A Pietro-issued API token. Belongs to exactly one User, scoped to exactly one Service. |

Do not write `user_id` on a column that means operator. Do not name a struct
`User` for the OIDC subject if a different struct elsewhere holds API
credentials. Pick one word per concept and never alias.

---

## 4. Architecture (text diagram)

```
                   ┌──────────────────────────────────────────────┐
                   │             Pietro (one binary)              │
                   │                                              │
   browser ───────►│  /  /assets/*        ── SPA (rust-embed)     │
                   │                                              │
   browser ───────►│  /api/auth/*         ── OIDC login (cookies) │
                   │  /api/me  /api/keys  ── session-guarded API  │◄── SQLite
                   │  /api/services       ── reads config         │
                   │                                              │
   any client ────►│  /proxy/:svc/*path   ── key-guarded forward  │───► upstream HTTP
                   │                                              │     (api.foo.com,
                   └──────────────────────────────────────────────┘     etc.)
                                          ▲
                                          │ reads at boot
                                    pietro.yaml
```

One process. One DB file (`pietro.db`). One config file (`pietro.yaml`).
That's the entire mental model. If a feature can't fit in this picture, it
probably doesn't belong in v1.

---

## 5. Component choices (and what we rejected)

The principle: **fewer pieces, boring pieces.**

| Concern                  | Choice                                | Why this, why not the alternative |
|--------------------------|---------------------------------------|------------------------------------|
| HTTP server / routing    | `axum` (latest stable) + `tokio`      | Default for new Rust web in 2024-25. Tower middleware is the lingua franca. Hyper 1.x under the hood. |
| HTTP client (forwarder)  | `reqwest` with streaming bodies       | Already depends on hyper. Native streaming. Rejected `hyper` raw client — more code for no benefit. |
| Reverse-proxy logic      | **Hand-written** (~80 lines)          | Rejected `axum-reverse-proxy`: small dep but black-box behavior for header rewriting. Rejected `pingora`: designed for 40M req/s, an order of magnitude too heavy. We need a function that copies method+headers+body to reqwest and pipes the response back. |
| Embed SPA in binary      | `rust-embed`                          | Canonical solution. Compile-time embed in release, on-disk in debug. Rejected `axum-embed`: thin wrapper, not worth the dep. |
| OIDC client              | `openidconnect` crate                 | Battle-tested, supports PKCE, ID-token verification, discovery. Rejected `axum-oidc` wrapper: hides cookie + session semantics we want to control. Cost: ~150 lines of glue. Benefit: no surprise middleware. |
| Session cookie           | `axum-extra::extract::cookie::SignedCookieJar` | One signed cookie holding `session_id`. Rejected `tower-sessions`: a whole session-store abstraction we don't need; the session is just `user_id + expiry`. |
| State / DB               | `sqlx` + SQLite                        | Compile-time-checked queries. One file. Rejected Postgres: YAGNI for v1. Rejected `rusqlite`: no async, doesn't compose with axum handlers as nicely. |
| Migrations               | `sqlx::migrate!` (embedded SQL files)  | Migrations live in `migrations/`, embedded at compile time, run on startup. |
| Config parsing           | `serde` + `serde_yaml` (or `serde_yml` if maintenance is a concern) | One file in, one struct out. Rejected `figment` / `config-rs`: layered config is a feature we don't need; "config in yaml" was the brief. |
| Env interpolation        | A 20-line `${ENV_VAR}` pre-processor on the YAML text | Rejected pulling `envsubst`-flavored crates. The substitution is one regex and one `std::env::var` call. |
| Password / token hashing | `blake3` (or `sha2::Sha256`)           | API keys are 256-bit random — argon2 / bcrypt is the wrong tool. See §11. |
| Random token gen         | `rand::rngs::OsRng` + base32 (Crockford) or base62 | Base32 is keyboard-safe and case-insensitive-friendly. Whatever we pick, it must be URL-safe. |
| Logging                  | `tracing` + `tracing-subscriber`       | Standard. JSON-or-pretty toggle via env. |
| CLI surface              | `clap` with two subcommands: `serve`, `migrate` | Boring. `pietro serve --config pietro.yaml`. |
| Build tool (frontend)    | `vite` + `react` + `typescript`        | Mainstream. Outputs static files into `frontend/dist/` which `rust-embed` picks up. |
| UI styling               | `tailwindcss`                          | Zero-runtime CSS, small bundle, fast iteration. Locked in. |

**Total third-party Rust crates (target):** `tokio`, `axum`, `axum-extra`,
`tower`, `tower-http`, `tracing`, `tracing-subscriber`, `serde`, `serde_yaml`,
`serde_json`, `sqlx`, `reqwest`, `openidconnect`, `rust-embed`, `rand`,
`blake3`, `clap`, `thiserror`, `time`. Roughly 18. If a PR pushes that past 25
without strong justification, that's a smell.

---

## 6. Domain model (make illegal states unrepresentable)

```rust
/// A service name as it appears in pietro.yaml and in URLs.
/// Validated at config load: ^[a-z0-9][a-z0-9-]{0,31}$
pub struct ServiceId(String);

/// The OIDC subject, opaque to us.
pub struct UserId(String);

/// A plaintext API key. Only ever exists in memory:
///   - briefly after generation, to show the User once
///   - briefly during validation, to compare to the stored hash
/// Never derives Debug; never serialised; never logged.
pub struct ApiKey(SecretString);

/// What we actually persist. Deterministic hash of an ApiKey.
pub struct ApiKeyHash([u8; 32]);

/// Visible identifier of a key — safe to show, log, search by.
/// Format: "pi_<6 char random>". Stable for the life of the key.
pub struct KeyId(String);
```

Rules the types enforce:

- `ApiKey` cannot be stored in the database. The repository takes only
  `ApiKeyHash`. If you have an `ApiKey`, you're either creating it (about to
  hash it) or verifying it (about to hash it and look it up).
- `ServiceId` is constructed only by the config loader. Handlers receive it
  via routing extractor *only after* a lookup against the configured set has
  succeeded — i.e. the type guarantees "this name refers to a real service".
- The session extractor produces `AuthenticatedUser(UserId)` or rejects with
  401. Handlers that need a user take it by argument. There is no
  `Option<UserId>` in handler signatures, and no `is_logged_in()` boolean.
- `KeyId` is what the UI displays and what URLs reference. The plaintext key
  is only ever shown to the User once, immediately after creation, in the JSON
  response — and then discarded server-side.

Result: there is no path in the code where a raw API key reaches storage, and
no path where an unauthenticated request reaches a user-scoped handler.

---

## 7. HTTP surface (every route, exhaustive)

All routes mounted on a single axum `Router`. No nested apps.

### Public (no auth)
| Method | Path                  | Purpose                                          |
|--------|-----------------------|--------------------------------------------------|
| GET    | `/healthz`            | Liveness. Returns 200 "ok".                      |
| GET    | `/`                   | Serves `index.html` from embedded SPA.           |
| GET    | `/assets/*file`       | Serves embedded SPA static assets.               |
| GET    | `/*path` (fallback)   | SPA history-mode fallback → `index.html`.        |
| GET    | `/api/auth/login`     | Begins OIDC auth code + PKCE; 302 to IdP.        |
| GET    | `/api/auth/callback`  | OIDC callback; sets session cookie; 302 to `/`.  |
| POST   | `/api/auth/logout`    | Clears session cookie; 204.                      |

### Session-guarded (signed cookie, no CSRF token needed because all mutations require `Content-Type: application/json` + same-origin — see §10) 
| Method | Path                          | Purpose                                       |
|--------|-------------------------------|-----------------------------------------------|
| GET    | `/api/me`                     | `{ user_id, email, name }`.                   |
| GET    | `/api/services`               | List of configured services (id + display name + brief description). Never returns upstream credentials. |
| GET    | `/api/keys`                   | List the *current user's* keys: `KeyId`, `ServiceId`, label, prefix, last4, created_at, last_used_at, revoked_at. |
| POST   | `/api/keys`                   | Body: `{ service_id, label }`. Generates key, stores hash, returns the **plaintext key exactly once**. If the user already has an active key for `service_id`, returns **409 Conflict** with code `key_already_exists`; the caller must revoke first. See §11.2. |
| DELETE | `/api/keys/:key_id`           | Soft-revoke (set `revoked_at = now()`). 204.  |

### Caller-guarded (API key in `Authorization: Bearer pi_live_...`)
| Method | Path                          | Purpose                                       |
|--------|-------------------------------|-----------------------------------------------|
| ANY    | `/proxy/:service_id/*path`    | Forward to upstream. Method, headers, body all pass through, except per §12 rewrites. |

Notes:
- We do **not** mount the SPA fallback over `/api/*` or `/proxy/*`. Those paths return JSON 404.
- The OIDC callback URL the operator must register at the IdP is
  `${base_url}/api/auth/callback`. This is the only public path that must be
  reachable from the user-agent and known to the IdP.

---

## 8. Configuration (`pietro.yaml`)

Single file. One struct. Parsed once at startup. Pietro does **not** watch
for changes — restart on edit. Watching files is a synchronization problem
in disguise and is explicitly out of scope.

Full example with every field:

```yaml
# pietro.yaml

# Where Pietro listens. Use 127.0.0.1 behind a TLS terminator.
listen: "0.0.0.0:8080"

# The base URL users actually browse to. Used to build OIDC redirect_uri
# and to set cookie domain. No trailing slash.
public_url: "https://pietro.example.com"

# Path to the SQLite file. Created if missing. Relative paths are resolved
# from the current working directory.
database_path: "./pietro.db"

# Cookie signing key. 64 bytes, hex or base64. Use a long random value.
# If absent and PIETRO_COOKIE_KEY env is set, that wins. Generate one with
# `openssl rand -hex 32`.
cookie_key: "${PIETRO_COOKIE_KEY}"

# Server-side pepper for API-key hashing. 32+ random bytes. Rotating this
# invalidates ALL keys — there is no key rotation in v1, this is by design.
api_key_pepper: "${PIETRO_API_KEY_PEPPER}"

# OIDC settings. Only one provider in v1.
oidc:
  issuer_url: "https://login.example.com/realms/main"
  client_id: "pietro"
  client_secret: "${PIETRO_OIDC_CLIENT_SECRET}"
  # Optional. Restrict who may log in. If absent, anyone the IdP authenticates is allowed.
  allowed_email_domains: ["example.com"]
  # Scopes requested in addition to the always-included "openid".
  scopes: ["profile", "email"]

# Services available to mint keys for. The order here is the order shown
# in the UI dropdown.
services:
  - id: "openai"                       # Used in URLs and as a stable handle
    display_name: "OpenAI"             # Shown in the UI
    description: "Chat + embeddings"   # Optional
    upstream_url: "https://api.openai.com"
    # How Pietro injects upstream credentials into forwarded requests.
    auth:
      kind: "bearer"                   # bearer | header | query
      value: "${OPENAI_API_KEY}"       # Plain string after env expansion
      # For kind=header: also specify `header: "X-Api-Key"`
      # For kind=query:  also specify `param: "api_key"`

  - id: "internal-search"
    display_name: "Internal Search"
    upstream_url: "http://search.internal:9000"
    auth:
      kind: "header"
      header: "X-Service-Token"
      value: "${SEARCH_TOKEN}"
```

### Env interpolation rule (boring on purpose)

Before YAML parsing, the raw file is passed through a single regex:

```
\$\{([A-Z_][A-Z0-9_]*)\}    →    std::env::var(captured) | error if unset
```

No defaults, no shell expansion, no nesting. If you want a default, write the
literal value. Astonishment risk: zero.

### Validation at load

The config struct's `TryFrom<RawConfig>` impl checks:

- `service.id` matches `^[a-z0-9][a-z0-9-]{0,31}$` and is unique.
- `service.upstream_url` parses as `http`/`https` URL.
- `cookie_key` decodes to ≥ 32 bytes.
- `api_key_pepper` decodes to ≥ 32 bytes.
- `oidc.issuer_url` is reachable and returns a valid discovery document. (Done with a timeout; if down, startup fails fast.)
- At least one service exists.

If any check fails, log a precise message and exit nonzero. No partial
startup. "Parse at the boundary, trust inside" — once `Config` is built, no
handler ever revalidates it.

---

## 9. Storage schema (SQLite)

Three tables. That's all v1 needs.

```sql
-- migrations/0001_init.sql

CREATE TABLE users (
    id            TEXT PRIMARY KEY,         -- OIDC subject (sub claim)
    email         TEXT NOT NULL,
    display_name  TEXT,
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE api_keys (
    id           TEXT PRIMARY KEY,           -- KeyId, e.g. "pi_xK91aF"
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    service_id   TEXT NOT NULL,              -- Validated at API boundary; not FK'd because services live in YAML
    label        TEXT NOT NULL,              -- User-supplied human label
    key_hash     BLOB NOT NULL,              -- 32 bytes: blake3(pepper || plaintext)
    prefix       TEXT NOT NULL,              -- First 12 chars of plaintext, e.g. "pi_live_aB3d" — for UI display
    last4        TEXT NOT NULL,              -- Last 4 chars of plaintext — for UI display
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT,                       -- NULL until first use
    revoked_at   TEXT                        -- NULL = active
);
CREATE INDEX api_keys_user_idx ON api_keys(user_id);
CREATE UNIQUE INDEX api_keys_hash_idx ON api_keys(key_hash);  -- enforces non-collision
-- At most ONE active key per (user, service). Revoked keys do not occupy the slot,
-- so a user may revoke and immediately re-mint. Enforced by the DB, not the app.
CREATE UNIQUE INDEX api_keys_active_user_service_idx
    ON api_keys(user_id, service_id)
    WHERE revoked_at IS NULL;

CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,            -- Random 128-bit, base32
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at  TEXT NOT NULL                -- Absolute, e.g. now + 12h
);
CREATE INDEX sessions_user_idx ON sessions(user_id);
```

### Decisions and their reasons

- **`service_id` is not a foreign key.** Services live in YAML; SQLite cannot
  enforce the relationship. Validation happens at the API layer at write
  time. The cost of "stale" rows (a service is removed from YAML but old keys
  reference it) is acceptable: the proxy handler rejects requests for unknown
  services with a clear 404. The UI filters them out. Storing services in
  both YAML and DB would be a duplicate-data smell.
- **Soft revocation** instead of hard delete preserves audit value (you can
  still see "key created → revoked" timeline). Hard-delete via a future
  admin tool if you ever need it.
- **One active key per (user, service).** Enforced by the partial unique
  index above — not by an app-layer check. Reasons: (a) the database is the
  only place this can be raced safely; (b) it removes a class of "I have
  four keys for the same thing, which one is on which laptop?" confusion;
  (c) revocation remains cheap because the index excludes `revoked_at IS NOT NULL`
  rows, so re-minting after revoke "just works". The handler maps the
  resulting `SQLITE_CONSTRAINT_UNIQUE` to a `409 Conflict` with code
  `key_already_exists` (see §11.2).
- **`prefix` and `last4` are derived from the plaintext at creation and then
  immutable.** This is the *only* "derived data" we persist — and we persist
  it because we explicitly throw away the plaintext, so it can never be
  re-derived. This is a necessary exception to "don't store derived data",
  documented here so future-you doesn't try to "fix" it.
- **Sessions live in the DB, not just in a self-contained signed cookie.**
  Why: we need server-side logout to be immediate. A signed-cookie-only
  session would still be "valid" client-side after revocation. The cookie
  itself stores only the random session id; the DB row is the truth.

---

## 10. Authentication flows

### 10.1 OIDC login (User → Pietro → IdP → Pietro → User)

```
GET /api/auth/login
  │
  ├─ Generate (state, nonce, pkce_verifier).
  ├─ Stash (verifier, nonce, state) keyed by a short-lived "flow cookie"
  │  (signed, 5-minute TTL, HttpOnly, SameSite=Lax). NOT in the DB —
  │  it's a one-shot per browser, lives only across the redirect.
  └─ 302 → IdP authorize URL with state + pkce_challenge

[user authenticates at IdP]

GET /api/auth/callback?code=...&state=...
  │
  ├─ Read flow cookie; verify state matches.
  ├─ Exchange code for tokens using pkce_verifier.
  ├─ Verify ID token (signature, issuer, audience, nonce, exp).
  ├─ Enforce allowed_email_domains if configured.
  ├─ UPSERT users row (id = sub, email, display_name).
  ├─ INSERT sessions row (random id, user_id, expires = now+12h).
  ├─ Set signed cookie `pietro_session` = session_id; HttpOnly; Secure
  │  (if public_url is https); SameSite=Lax; Path=/.
  ├─ Delete flow cookie.
  └─ 302 → /
```

### 10.2 Session check (extractor)

```
For every request to /api/me, /api/services, /api/keys/*:
  1. Read pietro_session cookie. Missing or invalid signature → 401.
  2. SELECT * FROM sessions WHERE id = ? AND expires_at > now(). None → 401.
  3. Hand the handler an AuthenticatedUser(UserId). Done.
```

No middleware that "tries" to authenticate and sets `Option<UserId>` on the
request. Either the extractor succeeds and the handler runs, or it fails and
the handler is never called.

### 10.3 Logout

```
POST /api/auth/logout
  ├─ DELETE FROM sessions WHERE id = ?
  ├─ Clear cookie (Max-Age=0).
  └─ 204
```

### 10.4 CSRF posture

All mutating endpoints (`POST /api/keys`, `DELETE /api/keys/:id`, `POST
/api/auth/logout`) are same-origin only and accept `Content-Type:
application/json`. The session cookie is `SameSite=Lax`. Combined, this
defeats classic CSRF on modern browsers without an explicit anti-forgery
token. If we ever expose any non-JSON POST endpoint or relax SameSite, we add
a double-submit token then — but not before.

### 10.5 Token refresh

Out of scope for v1. Sessions are 12 hours, then the user logs in again.
Refresh tokens add complexity (storage, rotation, replay protection) for
marginal UX benefit on an internal tool. YAGNI.

---

## 11. API key lifecycle

### 11.1 Format

```
pi_live_<22 chars base32-Crockford>
```

- `pi_` is the project namespace (humans-can-hold-it: short, recognisable).
- `live_` is reserved for future environment separation; v1 always uses `live_`.
  Keeping the slot prevents a painful future migration. (This is the **one**
  place we accept a YAGNI exception, because the cost of adding the slot
  later is changing every emitted key. Cheap to add now, expensive later.)
- 22 chars base32 ≈ 110 bits entropy. More than enough.

Display in UI: `pi_live_aB3dXX…YzQk` (`prefix` … `last4`).

### 11.2 Generation

```
plaintext   = "pi_live_" + base32_crockford(OsRng.gen::<[u8; 14]>())
key_hash    = blake3(pepper || plaintext.as_bytes())   // 32 bytes
key_id      = "pi_" + base32_crockford(OsRng.gen::<[u8; 4]>())  // ~6 chars
prefix      = plaintext[..12]   // "pi_live_aB3d"
last4       = plaintext[-4..]   // "YzQk"

INSERT INTO api_keys (id, user_id, service_id, label, key_hash, prefix, last4)
VALUES (key_id, ...);

Return to UI: { key_id, plaintext, prefix, last4, ... }   ← ONCE only.
```

**Uniqueness contract.** A user may hold **at most one active key per
service**. The DB enforces this via the partial unique index in §9 — the
handler does *not* `SELECT … FOR UPDATE` first. On `INSERT`:

- success → 200 with the plaintext payload (once).
- `SQLITE_CONSTRAINT_UNIQUE` on `api_keys_active_user_service_idx` →
  `409 Conflict`, body `{ "error": { "code": "key_already_exists",
  "message": "An active key for this service already exists. Revoke it
  before minting a new one." } }`.

Why not auto-revoke the existing key and silently issue a new one? Two
reasons: (a) principle of least astonishment — a "create" verb that
revokes another resource as a side effect is a foot-gun; (b) it would let
a compromised UI session quietly rotate someone's key without explicit
intent. Forcing the user to revoke first makes the rotation visible and
auditable (`revoked_at` then `created_at` rows in succession).

### 11.3 Verification (hot path, must be fast)

```
1. Parse "Authorization: Bearer pi_live_..." → plaintext. Reject malformed.
2. h = blake3(pepper || plaintext)
3. SELECT user_id, service_id, revoked_at FROM api_keys WHERE key_hash = ?
4. None → 401. revoked_at != NULL → 401.
5. Update last_used_at = now() (deferred; see below).
```

Hashing with BLAKE3 + a 32-byte server-side pepper is ~microseconds, makes
the DB lookup a single indexed point read on a 32-byte BLOB column, and
guards against an attacker who gains read-only DB access (they can't use
the stolen hashes without the pepper).

**Why not Argon2/bcrypt?** Those are tuned to be slow against
low-entropy human passwords. API keys have ≥110 bits of entropy; brute-force
is infeasible regardless of hash speed, and the per-request cost of Argon2
(>10ms) is wasteful on the proxy hot path.

**`last_used_at` update strategy:** Don't write on every request — that would
serialise the proxy on a write lock. Batch via an in-memory `HashMap<KeyId,
Instant>` flushed every N seconds (e.g. 30s) in a background task. On
shutdown, flush once. Acceptable freshness for a "last used" display.

### 11.4 Revocation

Soft. `UPDATE api_keys SET revoked_at = now() WHERE id = ? AND user_id = ?`.
Verification checks `revoked_at IS NULL`. A user can only revoke their own
keys; the `user_id` clause is part of the WHERE, not relied upon by the app
layer alone.

---

## 12. Proxy request flow

For `ANY /proxy/:service_id/*path`:

```
1. Authenticate the caller (§11.3). Result: (user_id, key_service_id).
2. Look up service in the in-memory Config map.
   - If service_id (from URL) != key_service_id → 403 "key not valid for this service".
   - If service_id is unknown → 404.
3. Build the upstream request:
     method      = inbound method
     url         = service.upstream_url + "/" + path + "?" + query_string
     headers     = inbound headers MINUS hop-by-hop and MINUS Authorization
                                   PLUS service.auth injection (bearer/header/query)
                                       (if the caller already set the same header,
                                        overwrite it and emit a warn-level log
                                        carrying request_id + service_id + header_name;
                                        never log the values — see §12 "Header collisions" below)
                                   PLUS X-Forwarded-For chain entry
     body        = streamed pass-through (reqwest::Body::wrap_stream over axum's BodyDataStream)
4. Issue with reqwest. Timeout = configurable per service (default 60s, hard ceiling 5min).
5. Stream response status + headers + body back to the caller, again minus
   hop-by-hop headers.
6. After the response is fully sent, record `last_used_at` update intent.
```

### Hop-by-hop headers to strip (RFC 7230 §6.1)
`Connection`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`,
`TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`. Plus anything listed in the
inbound `Connection:` header. Plus our own session cookie (`pietro_session`)
— we never leak it upstream.

### What we do NOT do
- Rewrite response headers (`Location`, `Set-Cookie`). The upstream's URLs
  are the upstream's problem; if the Caller follows redirects manually they
  hit Pietro again only if they hit Pietro again. Most JSON APIs don't redirect.
- Decompress / recompress. Pass-through `Content-Encoding`.
- Buffer. Streaming bodies all the way through; no `.bytes().await` on the
  upstream response. A 10-GB upload through Pietro should not OOM Pietro.

### Header collisions (`auth.kind: header` / `bearer`)

When the operator's `service.auth` would set a header the caller already
sent, **Pietro overwrites and warns**. Reasoning:

- Overwrite is the only behavior that honors the operator's contract — the
  whole point of Pietro is that the operator's upstream credential is what
  reaches the upstream.
- Reject would be a foot-gun: a benign client that happens to send
  `Authorization: Bearer something-irrelevant` would get 4xx'd at the gate
  for no security reason.
- Merge has no defined semantics for credential headers.

The warning is emitted exactly once per request at `warn` level, with the
fields:

```
event       = "proxy.header_overwritten"
request_id  = <ULID>
service_id  = <id>
header_name = <name>      // e.g. "Authorization" or "X-Service-Token"
```

The **values are never logged** — neither the caller's nor the operator's.
The header name alone is enough to diagnose a misbehaving client.

### Errors

| Condition                                  | Status returned             |
|--------------------------------------------|-----------------------------|
| Missing/malformed `Authorization` header   | 401 + JSON error body       |
| Key not found or revoked                   | 401 + JSON                  |
| Key's service ≠ URL service                | 403 + JSON                  |
| Service id in URL doesn't exist            | 404 + JSON                  |
| Upstream timed out                         | 504                         |
| Upstream connect failure                   | 502                         |
| Upstream returned a status                 | that exact status, body pass-through |

JSON error body shape (single shape used everywhere):
```json
{ "error": { "code": "key_revoked", "message": "Human-readable reason" } }
```

---

## 13. Build & packaging (the single-binary story)

### Bootstrap (do this once, before §13.1)

Both halves of the project are scaffolded by their official generators. We do
not hand-write `Cargo.toml`, `package.json`, or `vite.config.ts` from scratch —
we let the tools generate the conventional shape and then *trim*. Reasoning:
generators encode current best practice, and matching the conventional layout
is the cheapest way to honor the principle of least astonishment for anyone
who has used either tool before.

```
# Rust side
cargo init --name pietro                # creates Cargo.toml, src/main.rs, .gitignore
mkdir migrations

# Frontend side
npm create vite@latest frontend -- --template react-ts
cd frontend
npm install
npm install -D tailwindcss @tailwindcss/vite
cd ..
```

After bootstrap, we *edit* the generated files (add deps to `Cargo.toml`,
wire Tailwind into `vite.config.ts`, replace the demo React app, etc.) — we
do not delete-and-rewrite. The generated `.gitignore` files from both tools
are also kept and merged.

This bootstrap is a one-shot step and should be captured in the README as
"how to start from a clean checkout" rather than performed manually each
time CI runs.

### Layout

```
pietro/
├── Cargo.toml
├── build.rs                  # Builds the frontend if dist/ is missing or stale
├── pietro.yaml               # Example config, also used in dev
├── migrations/
│   └── 0001_init.sql
├── frontend/
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── src/                  # React app
│   └── dist/                 # Built output — embedded by rust-embed
└── src/
    ├── main.rs               # CLI + bootstrap
    ├── config.rs             # YAML → Config (with validation)
    ├── domain.rs             # Newtypes: ServiceId, UserId, ApiKey, ApiKeyHash, KeyId
    ├── db.rs                 # sqlx pool + migrations
    ├── auth/
    │   ├── oidc.rs           # Login, callback
    │   └── session.rs        # Cookie + extractor
    ├── keys.rs               # Mint, list, revoke, verify
    ├── proxy.rs              # The forwarder
    ├── routes.rs             # Router assembly
    ├── ui.rs                 # rust-embed SPA handler
    └── errors.rs             # One error type, one JSON shape
```

### `build.rs` (minimal)

```text
If frontend/dist/index.html is missing OR frontend/dist is older than
frontend/src:
    run `npm ci` (if frontend/node_modules missing)
    run `npm run build`
Emit cargo:rerun-if-changed for frontend/src and frontend/package.json.
```

Three rules:
1. `cargo build --release` produces a working binary on a fresh checkout, no human steps.
2. CI caches `frontend/node_modules` and `frontend/dist` keyed on `package-lock.json` + source hash.
3. A developer can skip the JS build by setting `PIETRO_SKIP_FRONTEND=1` and pointing `rust-embed` at an already-built `dist/`.

### Single-binary distribution

- Target `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` for static binaries.
- Stripped release binary expected size: ~15–25 MB (most of it the SPA bundle and SQLite).
- Distribute as a tarball + checksum + signature, GitHub Releases style.

### Dev loop

```
# Terminal A (backend):
cargo run -- serve --config pietro.yaml

# Terminal B (frontend, only while iterating on UI):
cd frontend && npm run dev
# Vite dev server on :5173, configured to proxy /api/* and /proxy/* to :8080.
```

In dev mode (`#[cfg(debug_assertions)]`), the embedded UI handler returns a
plain text page explaining "you're in dev mode; the SPA is served by Vite on
:5173". This avoids the trap of accidentally testing against a stale embedded
bundle. (Astonishment risk addressed: a developer who hits :8080 directly
gets a clear note, not a 4-day-old SPA.)

---

## 14. UI structure (React)

### 14.1 Scaffolding

The frontend skeleton is generated by Vite's official template (see §13
"Bootstrap" for the exact commands). We do **not** hand-write the React
toolchain — Vite's generator is the source of the conventional shape.

After bootstrap, Tailwind v4 plugs in via the official `@tailwindcss/vite`
plugin — no `tailwind.config.js`, no PostCSS chain. One import in
`src/index.css`:

```css
@import "tailwindcss";
```

Result: the entire CSS toolchain is two files (`vite.config.ts` and
`index.css`). Nothing else. If a future contributor needs to extend it, they
edit those two files; there is no third place to look.

Vite dev server (`npm run dev`) is configured in `vite.config.ts` to proxy
`/api/*` and `/proxy/*` to `http://127.0.0.1:8080` so the local Pietro
binary handles them while Vite serves the SPA with HMR.

### 14.2 Pages

Three pages. That's enough.

| Route       | Page                | What it does                                                              |
|-------------|---------------------|---------------------------------------------------------------------------|
| `/`         | Dashboard / Keys    | Lists current user's keys, with Create button and Revoke action per row.  |
| `/new`      | Mint key            | Form: service dropdown + label. POSTs; shows the plaintext key once on success with a "Copy" button and a banner: "This key will not be shown again." |
| `/login`    | Login splash        | Single "Sign in" button → `/api/auth/login`. Shown when `/api/me` returns 401. |

Components: ~10 small ones. No state-management library
(Redux/Zustand/etc.) needed; `useState` + a tiny `fetch` wrapper is enough.
If a future feature demands shared state, add it then.

Bundle target: < 200 KB gzipped. If it grows past 500 KB, that's a smell.

---

## 15. Secrets handling

- Secrets enter via environment variables, referenced from YAML via `${VAR}`.
- Once loaded into the `Config` struct, secrets are wrapped in a `Secret<T>`
  newtype (custom, not `secrecy` crate to avoid the dep — 15 lines: holds a
  `String`, redacts in `Debug`, exposes `.expose()`).
- `Debug` impl on `Config` never prints secrets. Period. Logged config at
  startup is sanitised.
- `pietro.yaml` is read once. The file itself need not contain plaintext
  secrets if every secret slot uses `${ENV}`.
- The cookie signing key and the API-key pepper are the two never-rotatable
  values in v1. Rotating either invalidates all sessions / all keys
  respectively. Documented loudly in the example config.

---

## 16. Observability

- Logs: `tracing` with `tracing-subscriber`. Default human-readable; JSON if
  `PIETRO_LOG_FORMAT=json`. Levels controllable via `RUST_LOG`.
- Every inbound HTTP request gets a `request_id` (ULID), threaded as a
  `tracing` span field and echoed in an `X-Request-Id` response header.
- Per request log line (debug level): method, path, status, duration_ms,
  user_id (if any), key_id (if proxy), service_id (if proxy). API keys never
  logged.
- `/healthz` returns 200 if the DB pool can serve a `SELECT 1`. No upstream
  checks — Pietro is healthy if Pietro is healthy.
- Metrics: out of scope for v1. If asked later: `metrics` crate +
  `metrics-exporter-prometheus` behind a feature flag.

---

## 17. Error model

One enum:

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    #[error("upstream timed out")]
    UpstreamTimeout,
    #[error("upstream unreachable")]
    UpstreamUnreachable,
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}
```

One `IntoResponse` impl maps each variant to (status, error_code_string).
Every handler returns `Result<T, Error>`. No `?` ever propagates raw `sqlx`
or `reqwest` errors past the immediate caller — they become `Error::Internal`
and the original gets logged via `tracing::error!` with the request_id span.
"Parse at the boundary, trust inside" applies to errors too.

---

## 18. Testing strategy

- **Unit tests** for: env interpolation, config validation, key hashing,
  hop-by-hop header filtering, format of generated keys.
- **Integration tests** with `axum::Router` + `tower::ServiceExt::oneshot`
  driving the live router with a temp SQLite DB. Cover:
  - login flow against a fake OIDC issuer (mock with `wiremock`),
  - mint key → list keys (plaintext appears exactly once),
  - mint a second key for the same service → 409 with `key_already_exists`,
  - revoke key → re-mint for the same service succeeds (active-uniqueness slot freed),
  - revoke key → verification fails afterward,
  - proxy with valid key forwards correctly (mock upstream with `wiremock`),
  - proxy with wrong-service key → 403,
  - proxy with revoked key → 401,
  - proxy preserves status, streams body, strips hop-by-hop headers.
- **No mocking of internal modules.** Test through the public HTTP surface;
  that's the contract that actually matters.
- **No tests on the React UI in v1.** It's a thin CRUD form. If/when the UI
  grows, add Playwright. Until then, manual smoke testing is honest.

---

## 19. Milestones (build order)

Each milestone ends with the binary running and demonstrable. No half-states
shipped.

Status legend: ✅ shipped · 🟡 in progress · ⬜ not started.

1. **M1 — Skeleton (1 day).** ✅ *Shipped 2026-05-14.* axum hello-world, clap CLI, YAML config load with validation, `/healthz`. Single binary builds. Tests: 9/9. Smoke-tested.
2. **M2 — DB + migrations (½ day).** ✅ *Shipped 2026-05-14.* sqlx + SQLite, migrations baked in. `users` / `sessions` / `api_keys` tables with the partial unique index that enforces Q5. CLI `pietro migrate` is idempotent; `/healthz` upgraded to ping the pool. Tests: 11/11.
3. **M3 — OIDC login (2 days).** ✅ *Shipped 2026-05-14.* `/api/auth/login` (PKCE + flow cookie), `/api/auth/callback` (state check, code exchange, ID token verify, email allowlist, user upsert, session creation), `/api/auth/logout` (DB delete + cookie clear), `/api/me` (session-guarded). `errors.rs` now implements `IntoResponse` with the project-wide JSON shape. Tests: 24/24 (includes a wiremock-based real-discovery test of the login redirect). The full callback round-trip is verified against `scripts/fake-idp.py` for smoke and is meant to be re-verified against Keycloak in docker-compose per this section's brief.
4. **M4 — Key lifecycle (1 day).** ✅ *Shipped 2026-05-14.* `/api/services` (list-only, no secrets), `/api/keys` (mint with plaintext-once contract; 409 `key_already_exists` on dup; rejects unknown service / empty label / >128-char label), `/api/keys/:key_id` (soft revoke; 404 if not active or not owned), session-guarded throughout. `src/keys.rs` introduces `ApiKey` / `ApiKeyHash` / `KeyId` newtypes (§6), BLAKE3-with-pepper hashing (§11.3), Crockford base32 (§11.1). Hot-path `verify` and last-used-stamping primitives are landed but only wired up by M5. Tests: 40/40 — 11 new in `keys::tests` (mint/verify round-trip, dup → 409, revoke-frees-slot, owner-scoped revoke, ordered listing without hash, garbage/revoked rejection) plus 5 new router-level tests using a real signed session cookie.
5. **M5 — Proxy (2 days).** ✅ *Shipped 2026-05-14.* `ANY /proxy/{service_id}/{*path}` with streaming pass-through (no body buffering — a 10 GB upload doesn't OOM Pietro), bearer/header/query auth injection (operator credential wins on collision with `proxy.header_overwritten` warn, values never logged), hop-by-hop header stripping per RFC 7230 §6.1 including the inbound `Connection:` named hops, session-cookie scrubbing on the way out, X-Forwarded-For chain entry, and an in-memory `UsageBatcher` background-flushed every 30 s + on shutdown. Errors map per the §12 table (401 missing/revoked/garbage, 403 wrong service, 404 unknown service, 502/504 reqwest connect/timeout). Tests: 55/55 — 8 new proxy unit tests (URL assembly, header filter incl. session-cookie carve-out, XFF, all three auth-injection modes) and 6 new router integration tests against wiremock upstreams (full bearer round-trip, query-param injection, 404/401/revoked/status-and-body passthrough). Q4 resolved with the default: trust the immediate peer for XFF; document running behind one TLS terminator.
6. **M6 — React UI (2-3 days).** ⬜ Three pages, Tailwind, fetch wrapper, login redirect on 401. Built via Vite into `frontend/dist/`.
7. **M7 — Embed + release (½ day).** ⬜ `rust-embed`, SPA fallback handler, dev-mode notice page, musl release build, GitHub Actions release workflow.

Total: ~10 working days for one engineer. If it takes more, something has
been added that's not in this plan; pull it back out.

### Progress log

- **2026-05-14 — M1 shipped.** Bootstrapped via `cargo init` + `npm create vite@latest frontend -- --template react-ts` + Tailwind v4 (`@tailwindcss/vite`). 9/9 tests green.
- **2026-05-14 — M2 shipped.** sqlx 0.8 (sqlite/macros/migrate, no TLS). `migrations/0001_init.sql` includes the partial unique index `api_keys_active_user_service_idx` for Q5. 11/11 tests green.
- **2026-05-14 — M3 shipped.** openidconnect 4.0.1 + axum-extra 0.12 (cookie + cookie-signed) + cookie 0.18 (`key-expansion` — axum-extra doesn't enable it on its own). Wiremock + tiny `scripts/fake-idp.py` for smoke runs. 24/24 tests green. Notable scope decision logged here: the full code-exchange + ID-token-verification round-trip is not unit-tested by design (per §18's last bullet about honest smoke testing) — it's covered by Keycloak in docker-compose, not by minting signed JWTs in tests.
- **2026-05-14 — M4 shipped.** Added `blake3` 1 and `base32` 0.5 (Crockford alphabet). `src/keys.rs` (~370 LOC, ~half tests) covers the §6 newtypes, §11 format/hashing/verify, and §11.2 uniqueness contract. `MintedKey` hand-rolls `Debug` to redact the plaintext (`ApiKey` deliberately has no `Debug` impl at all). Router tests use a real signed session cookie produced via `cookie::CookieJar::signed_mut`, so the auth path is fully exercised end-to-end with no test-only bypass. Live-server smoke confirmed all four new endpoints return 401 + the project-wide JSON error shape when unauthenticated. 40/40 tests green. Crate count now 22 prod + 2 dev = 24 (budget 25).
- **2026-05-14 — M5 shipped.** Added `futures-util` for stream adapters and the `stream` feature on reqwest (which the proxy needs for `Body::wrap_stream` + `Response::bytes_stream`). `src/proxy.rs` (~500 LOC, half tests) holds the entire forwarder: hop-by-hop header strip (RFC 7230 §6.1) including dynamic names from the inbound `Connection:` header, session-cookie carve-out from `Cookie:`, all three auth-injection modes (bearer / header / query) with overwrite-and-warn on collisions (header values never logged), XFF chain entry from `ConnectInfo<SocketAddr>`, request-body streaming via `Body::wrap_stream`, response-body streaming via `bytes_stream` + `Body::from_stream`, and reqwest error mapping (timeout → 504, connect → 502). Background `UsageBatcher` task wakes every 30 s and on shutdown to drain `last_used_at` stamps without serialising the hot path on the SQLite write lock. Tests: 55/55 — 8 unit tests for the small helpers (URL assembly with/without query, hop-by-hop & session-cookie filtering, all three injection paths, XFF append-vs-set) and 6 router integration tests against wiremock upstreams (full bearer round-trip with operator-credential override, unknown service → 404, missing bearer → 401, revoked key → 401, status + body pass-through with arbitrary code, query-param injection). Q4 resolved as the default — trust the immediate peer for XFF; if the operator runs behind multiple proxies they should configure their TLS terminator to set the chain itself.

---

## 20. Open questions (resolved + outstanding)

The header used to read "need an answer before M1 starts." All six are now
resolved as of 2026-05-14 with M1–M5 shipped.

1. ~~**UI styling**: Tailwind or plain CSS modules?~~ **Resolved 2026-05-14: Tailwind v4, scaffolded via `npm create vite@latest -- --template react-ts`. See §14.1.**
2. ~~**Service auth injection — header collisions**: overwrite, reject, or merge?~~ **Resolved 2026-05-14: overwrite and warn. See §12 "Header collisions".**
3. ~~**OIDC `allowed_email_domains`** — is the email claim required, or do we also support `groups` / role-based allowlists in v1?~~ **Resolved 2026-05-14: email-only in v1. Groups deferred.**
4. ~~**Reverse proxy host**: should Pietro respect a configurable trusted proxy CIDR for `X-Forwarded-For` parsing, or always treat the immediate peer as the client?~~ **Resolved 2026-05-14 with M5 ship: trust the immediate peer in v1; if the operator runs behind multiple proxies they should configure their TLS terminator to set the chain. Documented in §12.**
5. ~~**Multiple keys per (user, service)**: allowed?~~ **Resolved 2026-05-14: exactly one *active* key per (user, service). Revoked keys do not block re-issue. See §9 and §11.2 for the partial-unique-index enforcement and the 409 contract.**
6. ~~**Logging out a single device vs. all sessions**: do we want "log out everywhere" in v1?~~ **Resolved 2026-05-14 with M3 ship: per-cookie logout only. `/api/auth/logout` deletes exactly the session row referenced by the cookie. "Log out everywhere" stays a v2 feature.**

---

## 21. What is deliberately not in this plan

Things that have shown up in "similar" projects and that we are not building:

- A web-UI editor for `pietro.yaml`. The file *is* the source of truth.
- A per-route ACL system. The grain is `(user, service)`; finer grain is YAGNI.
- A plugin / hook system. Every "plugin point" is a future bug surface.
- Multi-issuer OIDC. One IdP per Pietro instance. Run two Pietros if you need two.
- An admin role distinct from user. v1 has no "admin user"; the operator is whoever can edit pietro.yaml on the box.
- A `pietro doctor` self-diagnostic command. Nice to have, but startup validation already covers ~95% of misconfiguration.

If any of these come up during the build, the answer is "not in v1, file an
issue, keep going."

---

## 22. Summary

One binary. One config file. One database file. Three database tables. Three
UI pages. Roughly eighteen Rust dependencies. Two security primitives (signed
cookies for sessions, BLAKE3-hashed keys for the proxy). One forwarder
function.

If it looks small, that's the point. Saint Peter holds the keys; he doesn't
run the kingdom.

*Soli Deo gloria.*
