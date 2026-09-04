-- runMigrations supplies the connection-local digest map and frozen
-- _roost_0024_context instant inside this same transaction.
-- Its preflight requires every legacy runtime row to have tenant authority.

CREATE TABLE _roost_0024_assertions (
  name TEXT PRIMARY KEY,
  ok INTEGER NOT NULL CHECK (ok = 1)
);

CREATE TABLE coordinator_relocation_redemptions (
  jti TEXT PRIMARY KEY NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  redeemed_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  used_by_fp TEXT NOT NULL,
  delegated_by_fp TEXT NOT NULL
);

-- Preserve the plaintext source until both independently counted copies have
-- succeeded. Dropping it also drops the old dashboard index and minter trigger.
ALTER TABLE bootstrap_tokens RENAME TO bootstrap_tokens_plaintext_0024;

CREATE TABLE bootstrap_tokens (
  token_hash TEXT PRIMARY KEY NOT NULL CHECK (
    length(token_hash) = 64
    AND token_hash = lower(token_hash)
    AND token_hash NOT GLOB '*[^0-9a-f]*'
  ),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  dashboard_id TEXT NOT NULL REFERENCES dashboards(id),
  kind TEXT NOT NULL CHECK (kind IN ('worker', 'browser')),
  label TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  used_at_ms INTEGER,
  used_by_fp TEXT,
  minted_by_fp TEXT
);

-- Ordinary legacy grants identify a dashboard but not the minting account.
-- Exactly one dashboard membership is therefore required before any copy.
INSERT INTO _roost_0024_assertions (name, ok)
VALUES (
  'ordinary bootstrap token account authority',
  CASE WHEN NOT EXISTS (
    SELECT 1
    FROM bootstrap_tokens_plaintext_0024 AS source
    WHERE NOT (
      typeof(source.token) = 'text'
      AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
    )
      AND (
        SELECT count(*)
        FROM dashboard_memberships AS membership
        WHERE membership.dashboard_id = source.dashboard_id
      ) <> 1
  ) THEN 1 ELSE 0 END
);

-- Ordinary one-shot grants retain used and expired history. A dashboard must
-- resolve to exactly one membership because the legacy row did not record the
-- minting account; ambiguity is a hard migration failure rather than a guess.
INSERT INTO bootstrap_tokens (
  token_hash,
  account_id,
  dashboard_id,
  kind,
  label,
  created_at_ms,
  expires_at_ms,
  used_at_ms,
  used_by_fp,
  minted_by_fp
)
SELECT
  digest.token_hash,
  membership.account_id,
  source.dashboard_id,
  source.kind,
  source.label,
  source.created_at_ms,
  source.expires_at_ms,
  source.used_at_ms,
  source.used_by_fp,
  source.minted_by_fp
FROM bootstrap_tokens_plaintext_0024 AS source
JOIN temp._roost_sha256_hex_0024 AS digest
  ON digest.plaintext = source.token
JOIN dashboard_memberships AS membership
  ON membership.dashboard_id = source.dashboard_id
JOIN accounts AS account
  ON account.id = membership.account_id
JOIN dashboards AS dashboard
  ON dashboard.id = source.dashboard_id
WHERE NOT (
  typeof(source.token) = 'text'
  AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
);

INSERT INTO _roost_0024_assertions (name, ok)
VALUES (
  'ordinary bootstrap token copy count',
  CASE WHEN
    (SELECT count(*)
     FROM bootstrap_tokens_plaintext_0024 AS source
     WHERE NOT (
       typeof(source.token) = 'text'
       AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
     ))
    = (SELECT count(*) FROM bootstrap_tokens)
  THEN 1 ELSE 0 END
);

INSERT INTO _roost_0024_assertions (name, ok)
VALUES (
  'roost_sha256_hex input staging count',
  CASE WHEN
    (SELECT count(*)
     FROM bootstrap_tokens_plaintext_0024 AS source
     WHERE NOT (
       typeof(source.token) = 'text'
       AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
     ))
    = (SELECT count(*) FROM temp._roost_sha256_hex_0024)
  THEN 1 ELSE 0 END
);

-- A used relocation sentinel becomes an account-scoped replay ledger row only
-- while its original delegator still has exactly one active account authority.
-- Stale, expired, revoked, malformed, or otherwise powerless sentinels vanish.
INSERT INTO coordinator_relocation_redemptions (
  jti,
  account_id,
  redeemed_at_ms,
  expires_at_ms,
  used_by_fp,
  delegated_by_fp
)
SELECT
  substr(source.token, length('roost_move_') + 1),
  device.account_id,
  source.used_at_ms,
  source.expires_at_ms,
  source.used_by_fp,
  source.minted_by_fp
FROM bootstrap_tokens_plaintext_0024 AS source
JOIN authorized_keys AS delegator
  ON delegator.fingerprint = source.minted_by_fp
JOIN account_devices AS device
  ON device.fingerprint = delegator.fingerprint
JOIN accounts AS account
  ON account.id = device.account_id
LEFT JOIN authorized_key_revocations AS revocation
  ON revocation.fingerprint = delegator.fingerprint
CROSS JOIN _roost_0024_context AS context
WHERE typeof(source.token) = 'text'
  AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
  AND length(source.token) > length('roost_move_')
  AND source.expires_at_ms > context.now_ms
  AND source.used_at_ms IS NOT NULL
  AND source.used_by_fp IS NOT NULL
  AND source.minted_by_fp IS NOT NULL
  AND account.status = 'active'
  AND revocation.fingerprint IS NULL
  AND (
    SELECT count(*)
    FROM authorized_keys AS exact_key
    JOIN account_devices AS exact_device
      ON exact_device.fingerprint = exact_key.fingerprint
    JOIN accounts AS exact_account
      ON exact_account.id = exact_device.account_id
    LEFT JOIN authorized_key_revocations AS exact_revocation
      ON exact_revocation.fingerprint = exact_key.fingerprint
    WHERE exact_key.fingerprint = source.minted_by_fp
      AND exact_account.status = 'active'
      AND exact_revocation.fingerprint IS NULL
  ) = 1;

INSERT INTO _roost_0024_assertions (name, ok)
VALUES (
  'eligible relocation sentinel copy count',
  CASE WHEN
    (
      SELECT count(*)
      FROM bootstrap_tokens_plaintext_0024 AS source
      CROSS JOIN _roost_0024_context AS context
      WHERE typeof(source.token) = 'text'
        AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
        AND length(source.token) > length('roost_move_')
        AND source.expires_at_ms > context.now_ms
        AND source.used_at_ms IS NOT NULL
        AND source.used_by_fp IS NOT NULL
        AND source.minted_by_fp IS NOT NULL
        AND (
          SELECT count(*)
          FROM authorized_keys AS exact_key
          JOIN account_devices AS exact_device
            ON exact_device.fingerprint = exact_key.fingerprint
          JOIN accounts AS exact_account
            ON exact_account.id = exact_device.account_id
          LEFT JOIN authorized_key_revocations AS exact_revocation
            ON exact_revocation.fingerprint = exact_key.fingerprint
          WHERE exact_key.fingerprint = source.minted_by_fp
            AND exact_account.status = 'active'
            AND exact_revocation.fingerprint IS NULL
        ) = 1
    ) = (SELECT count(*) FROM coordinator_relocation_redemptions)
  THEN 1 ELSE 0 END
);

-- The pre-migration tenant hook must have assigned every mandatory runtime
-- scope. Managed databases also abort here rather than retaining unsafe rows.
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('workers dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM workers WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('events dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM events WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('sessions dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM sessions WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('workspaces dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM workspaces WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('workspace_sessions dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM workspace_sessions WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('tasks dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM tasks WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('mcp_relays dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM mcp_relays WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);
INSERT INTO _roost_0024_assertions (name, ok)
VALUES ('push_subscriptions dashboard scope', CASE WHEN NOT EXISTS (
  SELECT 1 FROM push_subscriptions WHERE dashboard_id IS NULL
) THEN 1 ELSE 0 END);

-- Preserve legacy breadcrumbs that are already dangling for the tombstone
-- migration to repair, but never carry a resolvable cross-dashboard edge.
INSERT INTO _roost_0024_assertions (name, ok)
VALUES (
  'existing same-dashboard relationships',
  CASE WHEN
    NOT EXISTS (
      SELECT 1
      FROM sessions AS child
      JOIN workers AS parent ON parent.fp = child.worker_fp
      WHERE child.dashboard_id <> parent.dashboard_id
    )
    AND NOT EXISTS (
      SELECT 1
      FROM workspaces AS child
      JOIN workers AS parent ON parent.fp = child.worker_fp
      WHERE child.dashboard_id <> parent.dashboard_id
    )
    AND NOT EXISTS (
      SELECT 1
      FROM workspace_sessions AS child
      JOIN workspaces AS parent ON parent.id = child.workspace_id
      WHERE child.dashboard_id <> parent.dashboard_id
    )
    AND NOT EXISTS (
      SELECT 1
      FROM workspace_sessions AS child
      JOIN sessions AS parent ON parent.id = child.session_id
      WHERE child.dashboard_id <> parent.dashboard_id
    )
  THEN 1 ELSE 0 END
);

DROP TABLE bootstrap_tokens_plaintext_0024;

CREATE INDEX bootstrap_tokens_dashboard_idx
  ON bootstrap_tokens(dashboard_id, kind, used_at_ms);

CREATE TRIGGER bootstrap_tokens_reject_revoked_minter
BEFORE INSERT ON bootstrap_tokens
WHEN NEW.minted_by_fp IS NOT NULL AND EXISTS (
  SELECT 1 FROM authorized_key_revocations
  WHERE fingerprint = NEW.minted_by_fp
)
BEGIN
  SELECT RAISE(ABORT, 'bootstrap minter revoked');
END;
-- Nullable ALTER TABLE columns are mandatory at the write boundary.
CREATE TRIGGER workers_require_dashboard_insert
BEFORE INSERT ON workers WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'workers dashboard scope required'); END;
CREATE TRIGGER workers_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON workers WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'workers dashboard scope required'); END;
CREATE TRIGGER events_require_dashboard_insert
BEFORE INSERT ON events WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'events dashboard scope required'); END;
CREATE TRIGGER events_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON events WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'events dashboard scope required'); END;
CREATE TRIGGER sessions_require_dashboard_insert
BEFORE INSERT ON sessions WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'sessions dashboard scope required'); END;
CREATE TRIGGER sessions_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON sessions WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'sessions dashboard scope required'); END;
CREATE TRIGGER workspaces_require_dashboard_insert
BEFORE INSERT ON workspaces WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'workspaces dashboard scope required'); END;
CREATE TRIGGER workspaces_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON workspaces WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'workspaces dashboard scope required'); END;
CREATE TRIGGER workspace_sessions_require_dashboard_insert
BEFORE INSERT ON workspace_sessions WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'workspace_sessions dashboard scope required'); END;
CREATE TRIGGER workspace_sessions_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON workspace_sessions WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'workspace_sessions dashboard scope required'); END;
CREATE TRIGGER tasks_require_dashboard_insert
BEFORE INSERT ON tasks WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'tasks dashboard scope required'); END;
CREATE TRIGGER tasks_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON tasks WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'tasks dashboard scope required'); END;
CREATE TRIGGER mcp_relays_require_dashboard_insert
BEFORE INSERT ON mcp_relays WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'mcp_relays dashboard scope required'); END;
CREATE TRIGGER mcp_relays_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON mcp_relays WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'mcp_relays dashboard scope required'); END;
CREATE TRIGGER push_subscriptions_require_dashboard_insert
BEFORE INSERT ON push_subscriptions WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'push_subscriptions dashboard scope required'); END;
CREATE TRIGGER push_subscriptions_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON push_subscriptions WHEN NEW.dashboard_id IS NULL
BEGIN SELECT RAISE(ABORT, 'push_subscriptions dashboard scope required'); END;
-- Cross-dashboard references are rejected on creation and scope changes.
CREATE TRIGGER sessions_require_scoped_worker_insert
BEFORE INSERT ON sessions
WHEN NOT EXISTS (SELECT 1 FROM workers WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id)
BEGIN SELECT RAISE(ABORT, 'session worker dashboard mismatch'); END;
CREATE TRIGGER sessions_require_scoped_worker_update
BEFORE UPDATE OF worker_fp, dashboard_id ON sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workers WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id
) OR EXISTS (
  SELECT 1 FROM workspace_sessions
  WHERE session_id = OLD.id AND dashboard_id <> NEW.dashboard_id
)
BEGIN SELECT RAISE(ABORT, 'session worker dashboard mismatch'); END;
CREATE TRIGGER workspaces_require_scoped_worker_insert
BEFORE INSERT ON workspaces
WHEN NOT EXISTS (SELECT 1 FROM workers WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id)
BEGIN SELECT RAISE(ABORT, 'workspace worker dashboard mismatch'); END;
CREATE TRIGGER workspaces_require_scoped_worker_update
BEFORE UPDATE OF worker_fp, dashboard_id ON workspaces
WHEN NOT EXISTS (
  SELECT 1 FROM workers WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id
) OR EXISTS (
  SELECT 1 FROM workspace_sessions
  WHERE workspace_id = OLD.id AND dashboard_id <> NEW.dashboard_id
)
BEGIN SELECT RAISE(ABORT, 'workspace worker dashboard mismatch'); END;
CREATE TRIGGER workspace_sessions_require_scoped_parents_insert
BEFORE INSERT ON workspace_sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workspaces
  WHERE id = NEW.workspace_id AND dashboard_id = NEW.dashboard_id
) OR NOT EXISTS (
  SELECT 1 FROM sessions WHERE id = NEW.session_id AND dashboard_id = NEW.dashboard_id
)
BEGIN SELECT RAISE(ABORT, 'workspace session dashboard mismatch'); END;
CREATE TRIGGER workspace_sessions_require_scoped_parents_update
BEFORE UPDATE OF workspace_id, session_id, dashboard_id ON workspace_sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workspaces
  WHERE id = NEW.workspace_id AND dashboard_id = NEW.dashboard_id
) OR NOT EXISTS (
  SELECT 1 FROM sessions WHERE id = NEW.session_id AND dashboard_id = NEW.dashboard_id
)
BEGIN SELECT RAISE(ABORT, 'workspace session dashboard mismatch'); END;
CREATE TRIGGER workers_preserve_child_dashboard_update
BEFORE UPDATE OF dashboard_id ON workers
WHEN EXISTS (
  SELECT 1 FROM sessions
  WHERE worker_fp = OLD.fp AND dashboard_id <> NEW.dashboard_id
) OR EXISTS (
  SELECT 1 FROM workspaces
  WHERE worker_fp = OLD.fp AND dashboard_id <> NEW.dashboard_id
)
BEGIN SELECT RAISE(ABORT, 'worker child dashboard mismatch'); END;

DROP TABLE temp._roost_sha256_hex_0024;
DROP TABLE _roost_0024_assertions;
DROP TABLE _roost_0024_context;
