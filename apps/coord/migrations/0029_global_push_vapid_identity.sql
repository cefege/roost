-- The Web Push VAPID keypair is coordinator-global: src/vapid.ts reads and
-- writes it only in the explicit NULL dashboard scope, so a dashboard-scoped
-- push.vapid row is unreachable, and both tenancy guards refuse to boot on one
-- (src/self-hosted-tenant.ts, src/managed-container-invariant.ts). Drop the
-- unreachable copies. When no global row exists the newest scoped copy is
-- promoted instead of deleted, so the surviving identity keeps signing the push
-- subscriptions minted under it. Every other key stays dashboard-scoped and
-- untouched; no user-visible setting is removed.

DELETE FROM app_settings
WHERE key = 'push.vapid'
  AND dashboard_id IS NOT NULL
  AND EXISTS (
    SELECT 1 FROM app_settings AS global_identity
    WHERE global_identity.key = 'push.vapid'
      AND global_identity.dashboard_id IS NULL
  );

-- updated_at_ms then rowid keeps the surviving copy deterministic when several
-- dashboards each retained one.
DELETE FROM app_settings
WHERE key = 'push.vapid'
  AND dashboard_id IS NOT NULL
  AND rowid <> (
    SELECT rowid FROM app_settings
    WHERE key = 'push.vapid'
      AND dashboard_id IS NOT NULL
    ORDER BY updated_at_ms DESC, rowid DESC
    LIMIT 1
  );

UPDATE app_settings
SET dashboard_id = NULL
WHERE key = 'push.vapid'
  AND dashboard_id IS NOT NULL;
