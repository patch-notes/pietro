# Pietro

> *"And I will give unto thee the keys of the kingdom of heaven."* — Matthew 16:19

**Pietro is the keeper of the keys.** It's a small, self-contained authenticated
API proxy. You declare a fixed set of upstream services in one YAML file; your
users log in through your existing OIDC provider, mint per-user API keys, and
point their clients at Pietro instead of the upstream. Pietro authenticates each
request, finds the right service, injects the operator's credentials, and streams
the response back.

It ships as **one binary** — backend, web UI, and streaming proxy all in a single
executable. One config file, one SQLite database. No sidecars, no runtime deps.

## Why Pietro?

- **Centralized credentials.** The real upstream API keys live only in Pietro's
  config. Users never see them — they get a Pietro key that Pietro swaps for the
  real credential on the way out.
- **Per-user keys you can revoke.** Every key is tied to a user and a service.
  Revoke one without disturbing anyone else.
- **Login you already have.** Authentication is delegated to your OIDC provider
  (Keycloak, Nextcloud, Auth0, Google, …) with an email-domain allowlist.
- **Streams everything.** Large uploads and downloads pass straight through — no
  buffering, no memory blowups.
- **Boring on purpose.** No plugins, no multi-tenancy, no hot reload. It does one
  job and gets out of the way.

## How it works

```
                       ┌─────────────────────────────────────────┐
   Browser  ──login──▶ │  Pietro  (single binary)                │
   (user)             │   • React UI   (mint / list / revoke)    │
                       │   • OIDC login (your IdP)                │
   API client ──key──▶ │   • Proxy      /proxy/<service>/...      │ ──▶ upstream
   (caller)            └─────────────────────────────────────────┘     (real creds
                                                                          injected)
```

1. A **user** signs in via OIDC and mints an API key for a service.
2. A **caller** sends requests to `https://your-pietro/proxy/<service>/<path>`
   with `Authorization: Bearer <pietro-key>`.
3. Pietro validates the key, strips the caller's auth, injects the operator's
   configured credential, and forwards the request upstream — streaming both ways.

## Quick start

You need [Rust](https://rustup.rs/) and [Node](https://nodejs.org/) (to build the
embedded UI), plus an OIDC provider. For local experiments, a tiny fake IdP stub
is included.

```bash
# 1. Build the web UI, then the release binary (which embeds it)
cd frontend && npm ci && npm run build && cd ..
cargo build --release

# 2. Generate the two secrets (32 bytes of hex each)
export PIETRO_COOKIE_KEY=$(openssl rand -hex 32)
export PIETRO_API_KEY_PEPPER=$(openssl rand -hex 32)
export PIETRO_OIDC_CLIENT_ID="your-client-id"
export PIETRO_OIDC_CLIENT_SECRET="your-client-secret"

# 3. Create the database
./target/release/pietro migrate --config pietro.yaml

# 4. Run it
./target/release/pietro serve --config pietro.yaml
```

Then open <http://localhost:18080>, sign in, and mint your first key.

> **Note:** a plain `cargo build` (debug) intentionally does **not** serve the web
> UI — it shows a notice page instead. For UI development run Vite:
> `cd frontend && npm run dev`. To test the real embedded UI, use
> `cargo build --release`.

### Try it with no real IdP

```bash
python3 scripts/fake-idp.py 19000 &     # dev-only OIDC stub
export PIETRO_OIDC_CLIENT_SECRET=dev
./target/release/pietro serve --config pietro.yaml
curl -s http://127.0.0.1:18080/healthz  # → ok
```

## Configuration

Everything lives in one `pietro.yaml`. Secrets are pulled from the environment
with `${VAR}` interpolation, so no credential ever has to be committed.

```yaml
listen: "127.0.0.1:18080"           # address the server binds to
public_url: "http://localhost:18080" # externally-visible base URL
database_path: "./pietro.db"

cookie_key: "${PIETRO_COOKIE_KEY}"       # 32 bytes (hex/base64) — signs session cookies
api_key_pepper: "${PIETRO_API_KEY_PEPPER}" # 32 bytes — peppers hashed API keys

oidc:
  issuer_url: "https://your-idp.example.com"
  client_id: "${PIETRO_OIDC_CLIENT_ID}"
  client_secret: "${PIETRO_OIDC_CLIENT_SECRET}"
  allowed_email_domains: ["example.com"]   # who may log in
  scopes: ["profile", "email"]

services:
  - id: "search"                 # ^[a-z0-9][a-z0-9-]{0,31}$
    display_name: "SearXNG"
    description: "Web search"
    upstream_url: "http://searxng.lan"
    auth:                        # optional — omit for open upstreams
      kind: bearer               # bearer | header | query
      value: "${SEARXNG_TOKEN}"
    # timeout_secs: 60           # optional per-service upstream timeout
```

**Service auth kinds** (the credential Pietro injects toward the upstream):

| `kind`   | Extra fields      | Injects |
|----------|-------------------|---------|
| `bearer` | `value`           | `Authorization: Bearer <value>` |
| `header` | `header`, `value` | a custom request header |
| `query`  | `param`, `value`  | a query-string parameter |
| *(omitted)* | —              | nothing — forwards to open upstreams as-is |

## Using a key

Once a user mints a key (shown **once** at creation — copy it then), callers use
it against the proxy. The proxy path mirrors the upstream:

```bash
# maps to  http://searxng.lan/search?q=pietro
curl -H "Authorization: Bearer pi_live_XXXXXXXXXXXXXXXXXXXXXX" \
     "https://your-pietro/proxy/search/search?q=pietro"
```

The dashboard shows each key's ready-to-use proxy endpoint URL next to it.

## Running in a container

A multi-stage `Dockerfile` produces a tiny static image (`FROM scratch`). The
`Makefile` drives multi-arch (amd64 + arm64) builds via podman:

```bash
make docker        # multi-arch build → pietro:latest
make docker-run    # run the native-arch image on :18080
make help          # list all targets
```

Prebuilt images are published to `ghcr.io/patch-notes/pietro`.

## CLI

```
pietro serve   --config pietro.yaml   # run the HTTP server
pietro migrate --config pietro.yaml   # apply pending DB migrations
```

## Documentation

- **`docs/architecture.md`** — how the system is built (source map, HTTP surface,
  design decisions, budgets, run/release recipe).
- **`pietro.md`** — the full, locked design plan and rationale.
- **`AGENTS.md`** — contributor conventions and quality gates.

## License

[MIT](LICENSE).
