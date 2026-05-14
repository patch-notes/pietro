-- 0001_init.sql — initial schema (§9 of pietro.md).
--
-- Three tables. That's all v1 needs. Migrations are append-only: never edit a
-- shipped file; add 0002, 0003, ... when changes are required.
--
-- SQLite-specific notes:
--   * Datetime columns are TEXT in ISO-8601 with `(datetime('now'))` defaults.
--   * Partial unique indexes are supported since SQLite 3.8.0 — fine for us.
--   * `STRICT` would be nice but `sqlx::migrate!` runs each file as a single
--     batch and STRICT-table constraints interact awkwardly with TEXT
--     defaults that use functions. Keep classic affinity for now.

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
CREATE UNIQUE INDEX api_keys_hash_idx ON api_keys(key_hash);

-- At most ONE active key per (user, service). Revoked rows do not occupy the
-- slot, so revoke-then-mint just works. Enforced by the DB, not the app
-- (§9 "One active key per (user, service)" decision).
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
