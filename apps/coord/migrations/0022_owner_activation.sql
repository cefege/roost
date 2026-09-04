-- Remove the historical high-volume anonymous SPA/static successes. Retain
-- authenticated rows, failures, API exports, WebSocket admission, and internal
-- lifecycle/security paths.
DELETE FROM audit_log
WHERE caller_fp IS NULL
  AND method IN ('GET', 'HEAD')
  AND status >= 200
  AND status < 400
  AND path NOT LIKE '/api/%'
  AND path NOT LIKE '/internal/%'
  AND path NOT LIKE '/ws/%';

-- Legacy Push rows were not tied to an authorized browser key. Remove rows
-- that cannot acquire the new foreign key before rebuilding the table.
DELETE FROM push_subscriptions
WHERE NOT EXISTS (
  SELECT 1
  FROM authorized_keys
  WHERE authorized_keys.fingerprint = push_subscriptions.viewer_fp
);

CREATE TABLE push_subscriptions_authorized_device (
  dashboard_id TEXT REFERENCES dashboards(id),
  viewer_fp TEXT NOT NULL REFERENCES authorized_keys(fingerprint) ON DELETE CASCADE,
  endpoint TEXT NOT NULL,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, viewer_fp, endpoint)
);
INSERT INTO push_subscriptions_authorized_device
  (dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms)
  SELECT dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms
  FROM push_subscriptions;
DROP TABLE push_subscriptions;
ALTER TABLE push_subscriptions_authorized_device RENAME TO push_subscriptions;

CREATE INDEX push_subscriptions_dashboard_viewer_idx
  ON push_subscriptions(dashboard_id, viewer_fp, endpoint);

-- Legacy messages can no longer be redeemed after this cutover and must never
-- be dispatched with dead links.
DELETE FROM email_outbox WHERE kind = 'invitation';

DROP TABLE invitation_dashboard_grants;
DROP TABLE invitations;

CREATE TABLE owner_activation_tokens (
  coordinator_id TEXT PRIMARY KEY,
  account_id TEXT UNIQUE NOT NULL,
  email_normalized TEXT UNIQUE NOT NULL,
  token_hash TEXT UNIQUE NOT NULL,
  outbox_id TEXT NOT NULL REFERENCES email_outbox(id),
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  accepted_at_ms INTEGER,
  revoked_at_ms INTEGER
);
