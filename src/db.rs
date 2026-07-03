//! SQLite pool + embedded migrations (§9, §13).
//!
//! Two entry points:
//!   * [`connect`]   — open / create the pool and apply pending migrations.
//!   * [`migrate`]   — apply pending migrations only (used by the
//!     `pietro migrate` subcommand for ops who want to run
//!     migrations as a separate step before `serve`).
//!
//! `sqlx::migrate!()` walks `migrations/` at compile time and embeds the SQL
//! into the binary, so a deployed Pietro never reads loose .sql files.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// Compile-time-embedded list of migrations under `./migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Open the SQLite database at `path`, create it (and its immediate parent
/// directory) if missing, run migrations, and hand back a ready-to-use pool.
/// This is the `serve` startup path: migrations always run on start.
///
/// The pool is small on purpose. SQLite serialises writers anyway; oversizing
/// the pool only buys lock contention.
pub async fn connect(path: &str) -> Result<SqlitePool> {
    let pool = open_pool(path).await?;
    MIGRATOR
        .run(&pool)
        .await
        .context("running embedded migrations")?;
    Ok(pool)
}

/// Open the pool without running migrations. Internal helper kept separate so
/// the `migrate` subcommand can decide what to log.
///
/// Ensures the DB file's immediate parent directory exists first, so a fresh
/// deploy pointing at e.g. `/var/lib/pietro/pietro.db` works on the very first
/// `serve` without a preceding `migrate`. We only create the immediate parent
/// (not arbitrary ancestors) — the operator owns the layout above that.
async fn open_pool(path: &str) -> Result<SqlitePool> {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir for {path:?}"))?;
    }

    let opts = SqliteConnectOptions::from_str(path)
        .with_context(|| format!("invalid sqlite path: {path:?}"))?
        .create_if_missing(true)
        // WAL is the right default for a single-writer app: readers don't
        // block the writer, and the writer doesn't block readers.
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        // NORMAL is safe under WAL and avoids a fsync per transaction; matches
        // most production SQLite usage.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(5));

    SqlitePoolOptions::new()
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_with(opts)
        .await
        .with_context(|| format!("opening sqlite pool at {path:?}"))
}

/// Run the embedded migrations against the configured DB and return how many
/// migrations the migrator reports as currently applied.
///
/// Used by `pietro migrate`. Idempotent: re-running it after all migrations
/// are applied is a no-op. The parent-directory ensure lives in `open_pool`,
/// so both this and `serve`'s `connect` get it.
pub async fn migrate(path: &str) -> Result<usize> {
    let pool = open_pool(path).await?;
    MIGRATOR
        .run(&pool)
        .await
        .context("running embedded migrations")?;
    let applied = MIGRATOR.iter().count();
    pool.close().await;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: a fresh in-memory database accepts the embedded
    /// migrations and exposes the expected tables + the partial unique
    /// index (Q5 resolution).
    #[tokio::test]
    async fn migrations_apply_cleanly() {
        // `:memory:` is per-connection in SQLite; sqlx maps that path to a
        // shared in-memory DB tied to the pool's lifetime.
        let pool = connect(":memory:").await.unwrap();

        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        let names: Vec<String> = tables.into_iter().map(|(n,)| n).collect();

        for expected in ["api_keys", "sessions", "users"] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing table {expected}; got {names:?}"
            );
        }

        // The partial unique index is the storage-level guarantee for the
        // "one active key per (user, service, label)" rule (§11.2, migration
        // 0002).
        let idx: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master \
             WHERE type = 'index' AND name = 'api_keys_active_user_service_label_idx'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(idx.is_some(), "partial unique index was not created");
    }

    /// `connect` (the `serve` startup path) must create the DB file *and* its
    /// missing parent directory, then run migrations — so a fresh deploy works
    /// on the first `serve` without a preceding `pietro migrate`.
    #[tokio::test]
    async fn connect_creates_missing_parent_dir_and_migrates() {
        let base = std::env::temp_dir().join(format!("pietro-db-test-{}", std::process::id()));
        // A nested dir that does NOT exist yet.
        let db_path = base.join("nested").join("pietro.db");
        let db_str = db_path.to_str().unwrap().to_string();
        // Ensure a clean slate.
        let _ = std::fs::remove_dir_all(&base);

        let pool = connect(&db_str)
            .await
            .expect("connect must create dir + db");

        // The file exists and a migrated table is queryable.
        assert!(db_path.exists(), "db file was not created at {db_path:?}");
        let n: (i64,) = sqlx::query_as("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .expect("users table must exist after migrations");
        assert_eq!(n.0, 0);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Two active rows for the same (user, service, label) must trip
    /// SQLITE_CONSTRAINT_UNIQUE. Revoking the first frees the slot. Rows that
    /// share (user, service) but differ by label are NOT a conflict.
    #[tokio::test]
    async fn active_user_service_uniqueness_is_enforced() {
        let pool = connect(":memory:").await.unwrap();

        sqlx::query("INSERT INTO users (id, email) VALUES (?, ?)")
            .bind("user-1")
            .bind("u@example.com")
            .execute(&pool)
            .await
            .unwrap();

        let insert_key = |id: &'static str, hash: &'static [u8]| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO api_keys \
                     (id, user_id, service_id, label, key_hash, prefix, last4) \
                     VALUES (?, 'user-1', 'svc', 'lbl', ?, 'pi_live_aaaa', 'zzzz')",
                )
                .bind(id)
                .bind(hash)
                .execute(&pool)
                .await
            }
        };

        insert_key("pi_aaaaaa", b"hash-1--------------------------")
            .await
            .expect("first key inserts");

        let err = insert_key("pi_bbbbbb", b"hash-2--------------------------")
            .await
            .expect_err("second active key must fail");
        let msg = format!("{err}");
        assert!(
            msg.to_ascii_lowercase().contains("unique"),
            "expected UNIQUE constraint error, got: {msg}"
        );

        // Revoke the first → the slot is free → the second insert succeeds.
        sqlx::query("UPDATE api_keys SET revoked_at = datetime('now') WHERE id = 'pi_aaaaaa'")
            .execute(&pool)
            .await
            .unwrap();
        insert_key("pi_cccccc", b"hash-3--------------------------")
            .await
            .expect("after revoke, re-mint works");

        // Same (user, service) but a DIFFERENT label is not a conflict: insert
        // a second active row that differs only by label.
        sqlx::query(
            "INSERT INTO api_keys \
             (id, user_id, service_id, label, key_hash, prefix, last4) \
             VALUES ('pi_dddddd', 'user-1', 'svc', 'other-label', ?, 'pi_live_aaaa', 'zzzz')",
        )
        .bind(b"hash-4--------------------------".as_slice())
        .execute(&pool)
        .await
        .expect("a different label for the same (user, service) must be allowed");
    }
}
