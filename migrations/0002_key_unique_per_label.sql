-- 0002_key_unique_per_label.sql — loosen the active-key uniqueness rule.
--
-- v1 enforced "at most one active key per (user, service)". In practice a
-- User wants several concurrently-active keys for the same Service — one per
-- machine/context — distinguished by their human label ("laptop", "ci",
-- "phone"). So the label becomes part of the uniqueness tuple.
--
-- New rule: at most one active key per (user, service, label). Same service
-- with a *different* label is now allowed; an exact (service, label) duplicate
-- still trips 409 `key_already_exists`, guarding against accidental re-mints.
--
-- Migrations are append-only: this file replaces the 0001 index rather than
-- editing it. The new index is strictly looser than the old one, so it always
-- builds cleanly on an existing database (the old index guaranteed at most one
-- active row per (user, service), hence at most one per (user, service, label)).

DROP INDEX IF EXISTS api_keys_active_user_service_idx;

CREATE UNIQUE INDEX api_keys_active_user_service_label_idx
    ON api_keys(user_id, service_id, label)
    WHERE revoked_at IS NULL;
