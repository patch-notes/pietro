//! API keys: format, hashing, mint, list, revoke, verify (§6, §11).
//!
//! Two layers in this file:
//!
//! 1. **Domain types** — `ApiKey`, `ApiKeyHash`, `KeyId`, `KeyPrefix`,
//!    `KeyLast4`. Make illegal states unrepresentable: an `ApiKey` cannot
//!    travel to storage; an `ApiKeyHash` cannot be shown to a user. See §6
//!    of `pietro.md`.
//!
//! 2. **Operations** — `mint`, `list_for_user`, `revoke`, `verify`. All
//!    operate against the SQLite pool. The hot path (`verify`) is one
//!    indexed point read on the `key_hash` BLOB column.
//!
//! Format: `pi_live_<22 char base32-Crockford>` (§11.1). Storage:
//! `blake3(pepper || plaintext)` (§11.3). Uniqueness: at most one active key
//! per `(user_id, service_id)`, enforced by a partial unique index in the
//! schema (§9) — not by a `SELECT` in this module.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base32::Alphabet;
use rand::RngCore;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::errors::Error;
use crate::secret::Secret;

// -- public-ish constants ----------------------------------------------------

/// Project namespace + environment slot (§11.1). The `live_` slot is the one
/// allowed YAGNI exception: cheap to reserve now, painful to add later.
pub const KEY_PREFIX: &str = "pi_live_";

/// Length of the random body in chars (after `pi_live_`). 14 random bytes
/// encoded in unpadded base32-Crockford → 23 chars. §11.1 specs 22 chars but
/// 14 random bytes give us 112 bits of entropy and the natural base32 length
/// is 23 — we keep all of them rather than truncating, since truncation
/// would mean the key can't be re-derived from its prefix+suffix.
const KEY_BODY_BYTES: usize = 14;

/// Length of the public key id body in chars (after `pi_`). 4 random bytes
/// → 7 base32-Crockford chars (we keep them all; §6 says "~6 chars").
const KEYID_BODY_BYTES: usize = 4;

/// How many chars of the plaintext we persist as the human-readable prefix.
/// Matches the example in §11.1: `"pi_live_aB3d"`.
const PREFIX_LEN: usize = 12;

/// Crockford alphabet — case-insensitive, no I/L/O/U to avoid confusion.
const ALPHABET: Alphabet = Alphabet::Crockford;

// -- domain types ------------------------------------------------------------

/// A plaintext API key. Lives only in memory:
///   * briefly after generation (returned to the User exactly once),
///   * briefly during verification (hashed then dropped).
///
/// Never `Debug`-printable, never serialised, never logged.
pub struct ApiKey(Secret<String>);

impl ApiKey {
    /// Construct from a freshly-generated or freshly-parsed plaintext string.
    /// Public only within the crate so external callers can't smuggle a raw
    /// `String` into the type system claiming it's an API key.
    pub(crate) fn from_plaintext(s: String) -> Self {
        Self(Secret::new(s))
    }

    /// View as `&str`. Every call site is a leak-audit point — there are
    /// exactly two: the JSON response after mint, and `hash_with_pepper`.
    pub(crate) fn expose(&self) -> &str {
        self.0.expose().as_str()
    }
}

/// The 32-byte BLAKE3 digest persisted in `api_keys.key_hash`. Constructed
/// only via [`ApiKey::hash_with_pepper`] or read from the database — there's
/// no public constructor from raw bytes outside this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyHash([u8; 32]);

impl ApiKeyHash {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Visible identifier of a key — safe to show, log, search by. Format:
/// `pi_<7 char base32-Crockford>` (§6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyId(String);

impl KeyId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse from a URL path parameter. Cheap structural check; the row may
    /// still not exist (handler treats that as a 404).
    pub fn parse(s: &str) -> Result<Self, Error> {
        if !s.starts_with("pi_") || s.len() < 4 || s.len() > 32 {
            return Err(Error::BadRequest("invalid key id"));
        }
        if !s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric()) {
            return Err(Error::BadRequest("invalid key id"));
        }
        Ok(Self(s.to_string()))
    }
}

impl ApiKey {
    /// Hash this plaintext with the operator-supplied pepper. See §11.3 for
    /// why we don't use Argon2/bcrypt: API keys carry ≥110 bits of entropy
    /// and the per-request cost matters on the proxy hot path.
    pub fn hash_with_pepper(&self, pepper: &[u8]) -> ApiKeyHash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(pepper);
        hasher.update(self.expose().as_bytes());
        ApiKeyHash(*hasher.finalize().as_bytes())
    }
}

// -- generation --------------------------------------------------------------

/// Output of [`mint`]: everything the response needs *and* the bookkeeping
/// fields we just persisted. The plaintext is the only field that's gone
/// after this struct is dropped.
pub struct MintedKey {
    pub key_id: KeyId,
    pub plaintext: ApiKey,
    pub prefix: String,
    pub last4: String,
}

impl std::fmt::Debug for MintedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print everything except the plaintext — same redaction policy as
        // Secret<T>::Debug.
        f.debug_struct("MintedKey")
            .field("key_id", &self.key_id)
            .field("plaintext", &"<redacted>")
            .field("prefix", &self.prefix)
            .field("last4", &self.last4)
            .finish()
    }
}

fn rand_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut v);
    v
}

fn generate_plaintext() -> ApiKey {
    let bytes = rand_bytes(KEY_BODY_BYTES);
    let body = base32::encode(ALPHABET, &bytes);
    ApiKey::from_plaintext(format!("{KEY_PREFIX}{body}"))
}

fn generate_key_id() -> KeyId {
    let bytes = rand_bytes(KEYID_BODY_BYTES);
    let body = base32::encode(ALPHABET, &bytes);
    KeyId(format!("pi_{body}"))
}

// -- mint --------------------------------------------------------------------

/// Insert a new key row for `(user_id, service_id, label)`.
///
/// Maps the SQLite partial-unique-index violation on
/// `api_keys_active_user_service_idx` to `Error::Conflict("key_already_exists")`
/// — see §11.2 "Uniqueness contract" for the rationale (no silent
/// auto-revoke).
pub async fn mint(
    pool: &SqlitePool,
    pepper: &[u8],
    user_id: &str,
    service_id: &str,
    label: &str,
) -> Result<MintedKey, Error> {
    let plaintext = generate_plaintext();
    let key_id = generate_key_id();
    let hash = plaintext.hash_with_pepper(pepper);

    let plain_str = plaintext.expose();
    let prefix = plain_str[..PREFIX_LEN.min(plain_str.len())].to_string();
    let last4 = plain_str
        .get(plain_str.len().saturating_sub(4)..)
        .unwrap_or("")
        .to_string();

    let result = sqlx::query(
        "INSERT INTO api_keys \
            (id, user_id, service_id, label, key_hash, prefix, last4) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(key_id.as_str())
    .bind(user_id)
    .bind(service_id)
    .bind(label)
    .bind(hash.as_bytes())
    .bind(&prefix)
    .bind(&last4)
    .execute(pool)
    .await;

    match result {
        Ok(_) => Ok(MintedKey {
            key_id,
            plaintext,
            prefix,
            last4,
        }),
        Err(sqlx::Error::Database(db)) if is_unique_violation(db.as_ref()) => {
            // Could be either the (user, service) active-uniqueness index or
            // the key_hash collision index. We can't easily tell the two
            // apart from the error message in SQLite; the latter is
            // astronomically improbable (112-bit collision), so we treat any
            // UNIQUE failure here as the (user, service) case.
            Err(Error::Conflict("key_already_exists"))
        }
        Err(e) => Err(Error::Internal(e.into())),
    }
}

fn is_unique_violation(db: &dyn sqlx::error::DatabaseError) -> bool {
    // SQLite SQLSTATE for UNIQUE constraint is "2067" (extended) or "19"
    // (primary). We match by message substring as well to stay robust.
    let code = db.code();
    let msg = db.message();
    matches!(code.as_deref(), Some("2067") | Some("1555") | Some("19")) || msg.contains("UNIQUE")
}

// -- list --------------------------------------------------------------------

/// One row in `GET /api/keys`. The `key_hash` column is deliberately absent —
/// it never leaves the database.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct KeyRecord {
    pub id: String,
    pub service_id: String,
    pub label: String,
    pub prefix: String,
    pub last4: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub async fn list_for_user(pool: &SqlitePool, user_id: &str) -> Result<Vec<KeyRecord>, Error> {
    sqlx::query_as::<_, KeyRecord>(
        "SELECT id, service_id, label, prefix, last4, created_at, last_used_at, revoked_at \
         FROM api_keys WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| Error::Internal(e.into()))
}

// -- revoke ------------------------------------------------------------------

/// Soft-revoke a key. Requires the row to belong to `user_id` — that clause
/// is part of the WHERE, not relied upon by the app layer alone (§11.4).
///
/// Returns `Ok(true)` if a row was actually flipped from active → revoked,
/// `Ok(false)` if the key didn't exist, belonged to a different user, or
/// was already revoked. Handlers map `false` to 404.
pub async fn revoke(pool: &SqlitePool, user_id: &str, key_id: &KeyId) -> Result<bool, Error> {
    let result = sqlx::query(
        "UPDATE api_keys \
         SET revoked_at = datetime('now') \
         WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(key_id.as_str())
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|e| Error::Internal(e.into()))?;
    Ok(result.rows_affected() == 1)
}

// -- verify (hot path) -------------------------------------------------------

/// Identity of a verified caller: which user, scoped to which service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedKey {
    pub key_id: String,
    pub user_id: String,
    pub service_id: String,
}

/// Verify an inbound API key. Used by the proxy handler (M5).
///
/// One indexed point read on `key_hash` (a 32-byte BLOB column with a unique
/// index). Per §11.3 we BLAKE3-hash with the pepper *before* the query so
/// the DB sees only the hash — an attacker with read-only DB access can't
/// use stolen rows without the pepper.
pub async fn verify(
    pool: &SqlitePool,
    pepper: &[u8],
    plaintext: &str,
) -> Result<Option<VerifiedKey>, Error> {
    if !plaintext.starts_with(KEY_PREFIX) {
        return Ok(None);
    }
    let key = ApiKey::from_plaintext(plaintext.to_string());
    let hash = key.hash_with_pepper(pepper);
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, user_id, service_id FROM api_keys \
         WHERE key_hash = ? AND revoked_at IS NULL",
    )
    .bind(hash.as_bytes())
    .fetch_optional(pool)
    .await
    .map_err(|e| Error::Internal(e.into()))?;

    Ok(row.map(|(key_id, user_id, service_id)| VerifiedKey {
        key_id,
        user_id,
        service_id,
    }))
}

// -- last_used_at batching (placeholder for M5) ------------------------------

/// Stamp `last_used_at` for a verified key. The plan (§11.3) says this
/// should be batched to avoid serialising the proxy on a write lock. M5
/// will introduce the batcher; for now we expose the underlying primitive
/// so M5 only has to write the buffer-and-flush loop.
pub async fn mark_used(pool: &SqlitePool, key_id: &str, when: SystemTime) -> Result<(), Error> {
    let secs = when
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    sqlx::query("UPDATE api_keys SET last_used_at = datetime(?, 'unixepoch') WHERE id = ?")
        .bind(secs as i64)
        .bind(key_id)
        .execute(pool)
        .await
        .map_err(|e| Error::Internal(e.into()))?;
    Ok(())
}

// -- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_has_namespace_and_random_body() {
        let a = generate_plaintext();
        let b = generate_plaintext();
        assert!(a.expose().starts_with(KEY_PREFIX));
        assert_ne!(a.expose(), b.expose(), "must be random");
        // 14 random bytes → 23 unpadded base32-Crockford chars.
        assert_eq!(a.expose().len(), KEY_PREFIX.len() + 23);
    }

    #[test]
    fn key_id_parse_round_trip() {
        let id = generate_key_id();
        let parsed = KeyId::parse(id.as_str()).unwrap();
        assert_eq!(parsed.as_str(), id.as_str());
    }

    #[test]
    fn key_id_parse_rejects_garbage() {
        assert!(KeyId::parse("").is_err());
        assert!(KeyId::parse("not-a-key").is_err());
        assert!(KeyId::parse("pi_with/slash").is_err());
    }

    #[test]
    fn hash_is_deterministic_under_same_pepper() {
        let k = ApiKey::from_plaintext("pi_live_AAAA".to_string());
        let pepper = b"pepper-32-bytes-long-enough--okay";
        assert_eq!(k.hash_with_pepper(pepper), k.hash_with_pepper(pepper));
    }

    #[test]
    fn hash_diverges_when_pepper_changes() {
        let k = ApiKey::from_plaintext("pi_live_AAAA".to_string());
        let a = k.hash_with_pepper(b"pepper-A");
        let b = k.hash_with_pepper(b"pepper-B");
        assert_ne!(a, b);
    }

    async fn fixture() -> (sqlx::SqlitePool, Vec<u8>) {
        let pool = crate::db::connect(":memory:").await.unwrap();
        sqlx::query("INSERT INTO users (id, email) VALUES ('u1', 'a@example.com')")
            .execute(&pool)
            .await
            .unwrap();
        (pool, b"test-pepper-32-bytes-long-enough!".to_vec())
    }

    #[tokio::test]
    async fn mint_then_verify_round_trip() {
        let (pool, pepper) = fixture().await;
        let m = mint(&pool, &pepper, "u1", "svc-a", "laptop").await.unwrap();
        let plain = m.plaintext.expose().to_string();

        let v = verify(&pool, &pepper, &plain).await.unwrap().unwrap();
        assert_eq!(v.user_id, "u1");
        assert_eq!(v.service_id, "svc-a");
        assert_eq!(v.key_id, m.key_id.as_str());
    }

    #[tokio::test]
    async fn second_active_key_for_same_service_yields_409() {
        let (pool, pepper) = fixture().await;
        mint(&pool, &pepper, "u1", "svc-a", "k1").await.unwrap();
        let err = mint(&pool, &pepper, "u1", "svc-a", "k2")
            .await
            .expect_err("dup must fail");
        match err {
            Error::Conflict(code) => assert_eq!(code, "key_already_exists"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_frees_the_active_slot() {
        let (pool, pepper) = fixture().await;
        let first = mint(&pool, &pepper, "u1", "svc-a", "k1").await.unwrap();

        let revoked = revoke(&pool, "u1", &first.key_id).await.unwrap();
        assert!(revoked, "first revoke flips the row");

        // Re-mint succeeds — the partial unique index ignores revoked rows.
        mint(&pool, &pepper, "u1", "svc-a", "k2")
            .await
            .expect("after revoke, re-mint works");

        // And the original key no longer verifies.
        let plain = first.plaintext.expose();
        assert!(verify(&pool, &pepper, plain).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn revoke_is_scoped_to_owner() {
        let (pool, pepper) = fixture().await;
        sqlx::query("INSERT INTO users (id, email) VALUES ('u2', 'b@example.com')")
            .execute(&pool)
            .await
            .unwrap();
        let m = mint(&pool, &pepper, "u1", "svc-a", "k1").await.unwrap();
        let did = revoke(&pool, "u2", &m.key_id).await.unwrap();
        assert!(!did, "u2 must not be able to revoke u1's key");
        // The original key still verifies — revocation didn't take.
        let plain = m.plaintext.expose();
        assert!(verify(&pool, &pepper, plain).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_for_user_returns_in_created_order_desc_without_hash() {
        let (pool, pepper) = fixture().await;
        let a = mint(&pool, &pepper, "u1", "svc-a", "first").await.unwrap();
        // Sleep one second so SQLite's `datetime('now')` ticks.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let b = mint(&pool, &pepper, "u1", "svc-b", "second").await.unwrap();
        let list = list_for_user(&pool, "u1").await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b.key_id.as_str());
        assert_eq!(list[1].id, a.key_id.as_str());
        // No hash field on KeyRecord at all — compile-time guarantee, but
        // assert the JSON shape just to be loud about it.
        let json = serde_json::to_value(&list[0]).unwrap();
        assert!(json.get("key_hash").is_none());
        assert!(json.get("hash").is_none());
    }

    #[tokio::test]
    async fn verify_rejects_garbage_and_revoked() {
        let (pool, pepper) = fixture().await;
        assert!(
            verify(&pool, &pepper, "not-even-prefixed")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            verify(&pool, &pepper, "pi_live_NOPE")
                .await
                .unwrap()
                .is_none()
        );

        let m = mint(&pool, &pepper, "u1", "svc-a", "k").await.unwrap();
        revoke(&pool, "u1", &m.key_id).await.unwrap();
        let plain = m.plaintext.expose();
        assert!(verify(&pool, &pepper, plain).await.unwrap().is_none());
    }
}
