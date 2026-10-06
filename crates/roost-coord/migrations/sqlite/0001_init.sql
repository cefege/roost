-- The coordinator's whole schema, in one migration.
--
-- GENERATED, NOT HAND-WRITTEN, and do not hand-edit the body. v3 never opens a
-- v2 database, so the squashed result of the v2 migration history IS the v3
-- initial schema. This file is that result, produced by replaying every
-- `apps/coord/migrations/0001`..`0033` file onto an empty SQLite database --
-- each in its own transaction with `PRAGMA foreign_keys = ON`, in the v2
-- runner's own order -- and then dumping `sqlite_master`. The table shapes are
-- identical to v2's final state on purpose, so a smoke fixture that writes the
-- database directly keeps working unchanged.
--
-- The two `0030` files (`0030_pair_request_provenance`,
-- `0030_workers_terminal_core_capacity`) are a v2 numbering collision. The v2
-- runner sorts migration stems with `localeCompare`, which puts `pair` first;
-- that is the order this file was taken in.
--
-- Four v2 tables are ABSENT by construction, because v2 created and later
-- dropped them and the final state is what ships: `agent_entries` (0019),
-- `invitations` + `invitations_redeem_idx` (0026), `permission_rules` +
-- `permission_rules_dashboard_idx` (0026), and `webhook_tokens` +
-- `webhook_tokens_dashboard_idx` (0026). A v3 install must not recreate them.
--
-- Neither `_migrations` (v2's own bookkeeping) nor `sqlite_sequence` (which
-- SQLite creates itself for the AUTOINCREMENT columns below) belongs in the
-- schema. `sqlx`'s migrator records this file in `_sqlx_migrations` instead.
--
-- WHY THE SCOPING TRIGGERS ARE NOT OPTIONAL. Every `*_require_dashboard_*`
-- trigger refuses a row whose tenancy scope is NULL, and every
-- `*_require_scoped_*` plus `*_preserve_child_dashboard_update` trigger refuses
-- a child that disagrees with its parent. That is the tenancy invariant enforced
-- at the DATABASE rather than in a handler, so no code path -- present or
-- future -- can skip the check by forgetting to call the right function.
-- Softening one of these is how a session from one dashboard becomes visible
-- in another, which is why they are triggers and not application checks.
--
-- WHY `events_worker_client_seq` IS NOT OPTIONAL. It is the partial unique
-- index the worker link's at-least-once replay deduplicates on. Drop it and
-- every redelivered `WSessionEvent` becomes a second durable event: a `closed`
-- lands twice and a browser folds two closures for one session.
--
-- A constant in this file with no reason above is a liability -- it is a number
-- nobody will be able to re-derive.

-- ─── carried over from v2 0001_init ────────────────────────────────────────────────

CREATE TABLE authorized_keys (
    fingerprint  TEXT    PRIMARY KEY,  -- hex SHA-256 of raw 32-byte ed25519 pubkey
    public_key   BLOB    NOT NULL,     -- raw 32 bytes
    label        TEXT    NOT NULL,
    added_at     INTEGER NOT NULL,      -- unix epoch ms
    paired_from_ip TEXT,
    paired_country TEXT,
    paired_user_agent TEXT,
    paired_edge_identity TEXT);

CREATE TABLE workers (
    fp               TEXT    PRIMARY KEY,   -- hex SHA-256 of ed25519 pubkey
    label            TEXT    NOT NULL,
    os               TEXT    NOT NULL,      -- 'darwin' | 'linux'
    git_sha          TEXT,
    host_metrics_json TEXT,                 -- serialized HostMetrics | NULL
    registered_at_ms INTEGER NOT NULL,
    last_seen_ms     INTEGER NOT NULL,
    reachable_addr TEXT,
    keeper_stale TEXT,
    dashboard_id TEXT REFERENCES dashboards(id),
    deleted_at_ms INTEGER,
    keeper_runtime_json TEXT,
    terminal_core_capacity_json TEXT,
    host_identity_json TEXT,
    mecatl_runtime_json TEXT);

CREATE TABLE events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL,      -- session event discriminator
    session_id   TEXT,                  -- NULL for 'snapshot'
    worker_fp    TEXT,                  -- NULL except 'opened' + 'snapshot'
    payload_json TEXT    NOT NULL,      -- full SessionEvent JSON
    ts           INTEGER NOT NULL,       -- unix epoch ms from the event itself
    client_seq INTEGER,
    dashboard_id TEXT REFERENCES dashboards(id));

CREATE TABLE sessions (
    id           TEXT    PRIMARY KEY,   -- uuid
    worker_fp    TEXT    NOT NULL REFERENCES workers(fp),
    channel      INTEGER NOT NULL,      -- worker-local PTY id
    kind         TEXT    NOT NULL,      -- 'shell'
    cwd          TEXT    NOT NULL,
    workspace_id TEXT,                  -- NULL = orphan
    status       TEXT    NOT NULL,      -- 'open' | 'closed'
    agent_json   TEXT,                  -- preserved structured-session history | NULL
    created_at   INTEGER NOT NULL,
    closed_at    INTEGER,
    custom_title TEXT,
    git_branch TEXT,
    git_remote TEXT,
    pr_number INTEGER,
    pr_state TEXT,
    pr_checks TEXT,
    pr_url TEXT,
    ports_json TEXT,
    spawn_cwd TEXT,
    dashboard_id TEXT REFERENCES dashboards(id),
    agent_reference_json TEXT,
    agent_reference_client_seq INTEGER
  CHECK (
    (
      agent_reference_json IS NULL
      AND agent_reference_client_seq IS NULL
    )
    OR (
      typeof(agent_reference_client_seq) = 'integer'
      AND agent_reference_client_seq > 0
      AND agent_reference_client_seq <= 9007199254740991
    )
  ));

CREATE TABLE pair_requests (
    id           TEXT    PRIMARY KEY,   -- uuid
    ephemeral_id TEXT    NOT NULL UNIQUE,
    public_key   BLOB    NOT NULL,      -- raw 32 bytes
    label        TEXT    NOT NULL,
    status       TEXT    NOT NULL,      -- 'pending' | 'approved' | 'denied'
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER,
    user_agent TEXT,
    client_browser TEXT,
    client_os TEXT,
    client_device_type TEXT,
    source_ip TEXT,
    country_code TEXT,
    region TEXT,
    city TEXT,
    edge_identity_provider TEXT,
    edge_identity TEXT,
    edge_identity_verified INTEGER NOT NULL DEFAULT 0,
    expires_at_ms INTEGER NOT NULL DEFAULT 0,
    ceremony_version INTEGER NOT NULL DEFAULT 0,
    requester_token_hash TEXT NOT NULL DEFAULT '',
    verification_code_hash TEXT,
    verification_attempts INTEGER NOT NULL DEFAULT 0,
    approved_by_fp TEXT,
    approved_account_id TEXT);

CREATE TABLE tasks (
    id                            TEXT    PRIMARY KEY,  -- uuid
    state                         TEXT    NOT NULL,     -- 'pending' | 'claimed' | 'running' | 'done' | 'failed' | 'cancelled'
    payload_json                  TEXT    NOT NULL,     -- free-form JSON
    enqueued_at_ms                INTEGER NOT NULL,
    claimed_at_ms                 INTEGER,
    claimed_by                    TEXT,                 -- worker fp
    finished_at_ms                INTEGER,
    result_json                   TEXT,
    completion_check              TEXT,
    completion_check_last_attempt_ms INTEGER,
    claim_ttl_ms                  INTEGER NOT NULL,
dashboard_id TEXT REFERENCES dashboards(id));

CREATE TABLE mcp_relays (
    id            TEXT    PRIMARY KEY,  -- uuid
    label         TEXT    NOT NULL,
    kind          TEXT    NOT NULL,     -- 'stdio' | 'sse'
    config_json   TEXT    NOT NULL,     -- free-form JSON
    created_at_ms INTEGER NOT NULL,
dashboard_id TEXT REFERENCES dashboards(id));

CREATE TABLE audit_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    caller_fp    TEXT,
    method       TEXT    NOT NULL,      -- HTTP method
    path         TEXT    NOT NULL,
    status       INTEGER NOT NULL,
    trace_id     TEXT,
dashboard_id TEXT REFERENCES dashboards(id));

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

CREATE TABLE "workspaces" (
  id TEXT PRIMARY KEY,
  worker_fp TEXT NOT NULL REFERENCES workers(fp),
  name TEXT NOT NULL,
  color TEXT,
  position INTEGER NOT NULL DEFAULT 0,
  version INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  folder_path TEXT NOT NULL DEFAULT '~',
  dashboard_id TEXT REFERENCES dashboards(id)
);

CREATE TABLE "workspace_sessions" (
  workspace_id TEXT NOT NULL REFERENCES "workspaces"(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  added_at_ms INTEGER NOT NULL,
  dashboard_id TEXT REFERENCES dashboards(id),
  PRIMARY KEY (workspace_id, session_id)
);

CREATE INDEX events_by_session  ON events(session_id) WHERE session_id IS NOT NULL;

CREATE INDEX events_by_worker   ON events(worker_fp)  WHERE worker_fp  IS NOT NULL;

CREATE INDEX events_by_ts       ON events(ts);

CREATE INDEX sessions_by_worker    ON sessions(worker_fp);

CREATE INDEX sessions_open         ON sessions(status) WHERE status = 'open';

CREATE INDEX sessions_by_workspace ON sessions(workspace_id) WHERE workspace_id IS NOT NULL;

CREATE INDEX tasks_state ON tasks(state, enqueued_at_ms);

CREATE INDEX audit_log_ts ON audit_log(ts);


-- ─── carried over from v2 0004_events_client_seq ───────────────────────────────────

CREATE UNIQUE INDEX events_worker_client_seq
    ON events(worker_fp, client_seq)
    WHERE worker_fp IS NOT NULL AND client_seq IS NOT NULL;


-- ─── carried over from v2 0006_app_settings ────────────────────────────────────────

CREATE TABLE "app_settings" (
  dashboard_id TEXT REFERENCES dashboards(id),
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, key)
);


-- ─── carried over from v2 0014_push_subscriptions ──────────────────────────────────

CREATE TABLE "push_subscriptions" (
  dashboard_id TEXT REFERENCES dashboards(id),
  viewer_fp TEXT NOT NULL REFERENCES authorized_keys(fingerprint) ON DELETE CASCADE,
  endpoint TEXT NOT NULL,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, viewer_fp, endpoint)
);


-- ─── carried over from v2 0018_authorized_key_revocations ──────────────────────────

CREATE TABLE authorized_key_revocations (
  fingerprint   TEXT PRIMARY KEY,
  revoked_at_ms INTEGER NOT NULL,
  revoked_by_fp TEXT NOT NULL,
  reason        TEXT NOT NULL
);

CREATE TRIGGER authorized_keys_reject_revoked_insert
BEFORE INSERT ON authorized_keys
WHEN EXISTS (
  SELECT 1 FROM authorized_key_revocations
  WHERE fingerprint = NEW.fingerprint
)
BEGIN
  SELECT RAISE(ABORT, 'authorized key revoked');
END;

CREATE TRIGGER bootstrap_tokens_reject_revoked_minter
BEFORE INSERT ON bootstrap_tokens
WHEN NEW.minted_by_fp IS NOT NULL AND EXISTS (
  SELECT 1 FROM authorized_key_revocations
  WHERE fingerprint = NEW.minted_by_fp
)
BEGIN
  SELECT RAISE(ABORT, 'bootstrap minter revoked');
END;


-- ─── carried over from v2 0020_saas_identity ───────────────────────────────────────

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  email_normalized TEXT UNIQUE NOT NULL,
  password_hash TEXT,
  status TEXT NOT NULL CHECK(status IN ('active','disabled')),
  created_at_ms INTEGER NOT NULL,
  password_changed_at_ms INTEGER
);

CREATE TABLE account_devices (
  fingerprint TEXT PRIMARY KEY REFERENCES authorized_keys(fingerprint),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  added_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER NOT NULL
);

CREATE TABLE organizations (
  id TEXT PRIMARY KEY,
  slug TEXT UNIQUE NOT NULL,
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','suspended','deleting')),
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE organization_memberships (
  organization_id TEXT NOT NULL REFERENCES organizations(id),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  role TEXT NOT NULL CHECK(role IN ('owner','admin','member')),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (organization_id, account_id)
);

CREATE TABLE dashboards (
  id TEXT PRIMARY KEY,
  organization_id TEXT NOT NULL REFERENCES organizations(id),
  slug TEXT NOT NULL,
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','suspended','deleting')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE (organization_id, slug)
);

CREATE TABLE dashboard_memberships (
  dashboard_id TEXT NOT NULL REFERENCES dashboards(id),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  role TEXT NOT NULL CHECK(role IN ('admin','member')),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, account_id)
);

CREATE TABLE password_reset_tokens (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  email_normalized TEXT NOT NULL,
  token_hash TEXT PRIMARY KEY,
  expires_at_ms INTEGER NOT NULL,
  used_at_ms INTEGER
);

CREATE TABLE email_outbox (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  recipient TEXT NOT NULL,
  encrypted_payload TEXT NOT NULL,
  idempotency_key TEXT UNIQUE NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','sending','sent','failed')),
  attempts INTEGER NOT NULL DEFAULT 0,
  locked_until_ms INTEGER,
  lease_token TEXT,
  next_attempt_ms INTEGER NOT NULL,
  provider_message_id TEXT,
  sent_at_ms INTEGER,
  failed_at_ms INTEGER,
  last_error TEXT
);

CREATE TABLE "account_identities" (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  issuer TEXT NOT NULL,
  subject TEXT NOT NULL,
  email_normalized TEXT NOT NULL,
  linked_at_ms INTEGER NOT NULL,
  last_authenticated_at_ms INTEGER,
  revoked_at_ms INTEGER,
  PRIMARY KEY (issuer, subject)
);

CREATE INDEX account_devices_account_idx ON account_devices(account_id);

CREATE INDEX organization_memberships_account_idx ON organization_memberships(account_id, organization_id);

CREATE INDEX dashboards_organization_idx ON dashboards(organization_id, id);

CREATE INDEX dashboard_memberships_account_idx ON dashboard_memberships(account_id, dashboard_id);

CREATE INDEX password_reset_tokens_account_idx ON password_reset_tokens(account_id, expires_at_ms);

CREATE INDEX email_outbox_due_idx ON email_outbox(state, next_attempt_ms, locked_until_ms);

CREATE INDEX account_identities_account_idx
  ON account_identities(account_id);


-- ─── carried over from v2 0021_dashboard_runtime_scope ─────────────────────────────

CREATE INDEX workers_dashboard_idx ON workers(dashboard_id, fp);

CREATE INDEX events_dashboard_replay_idx ON events(dashboard_id, id);

CREATE INDEX sessions_dashboard_idx ON sessions(dashboard_id, id);

CREATE INDEX sessions_dashboard_worker_idx ON sessions(dashboard_id, worker_fp);

CREATE INDEX tasks_dashboard_idx ON tasks(dashboard_id, id);

CREATE INDEX mcp_relays_dashboard_idx ON mcp_relays(dashboard_id, id);

CREATE INDEX audit_log_dashboard_ts_idx ON audit_log(dashboard_id, ts, id);

CREATE INDEX app_settings_dashboard_key_idx ON app_settings(dashboard_id, key);

CREATE INDEX push_subscriptions_dashboard_viewer_idx
  ON push_subscriptions(dashboard_id, viewer_fp, endpoint);

CREATE INDEX bootstrap_tokens_dashboard_idx
  ON bootstrap_tokens(dashboard_id, kind, used_at_ms);

CREATE INDEX workspaces_dashboard_idx ON workspaces(dashboard_id, id);

CREATE INDEX workspace_sessions_dashboard_session_idx
  ON workspace_sessions(dashboard_id, session_id);


-- ─── carried over from v2 0022_owner_activation ────────────────────────────────────

CREATE TABLE "owner_activation_tokens" (
  coordinator_id TEXT PRIMARY KEY,
  account_id TEXT UNIQUE NOT NULL,
  email_normalized TEXT UNIQUE NOT NULL,
  token_hash TEXT UNIQUE NOT NULL,
  outbox_id TEXT REFERENCES email_outbox(id),
  delivery TEXT NOT NULL CHECK(delivery IN ('coordinator-email','signup-gateway')),
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  accepted_at_ms INTEGER,
  revoked_at_ms INTEGER
);


-- ─── carried over from v2 0023_federated_identity ──────────────────────────────────

CREATE TABLE federated_assertion_redemptions (
  jti TEXT PRIMARY KEY,
  purpose TEXT NOT NULL,
  device_fp TEXT NOT NULL,
  redeemed_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX account_identities_active_google_account_unique
  ON account_identities(account_id)
  WHERE issuer = 'https://accounts.google.com' AND revoked_at_ms IS NULL;


-- ─── carried over from v2 0024_auth_tenancy_stabilization ──────────────────────────

CREATE TABLE coordinator_relocation_redemptions (
  jti TEXT PRIMARY KEY NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  redeemed_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  used_by_fp TEXT NOT NULL,
  delegated_by_fp TEXT NOT NULL
);

CREATE TRIGGER workers_require_dashboard_insert
BEFORE INSERT ON workers
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'workers dashboard scope required');
END;

CREATE TRIGGER workers_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON workers
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'workers dashboard scope required');
END;

CREATE TRIGGER events_require_dashboard_insert
BEFORE INSERT ON events
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'events dashboard scope required');
END;

CREATE TRIGGER events_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON events
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'events dashboard scope required');
END;

CREATE TRIGGER sessions_require_dashboard_insert
BEFORE INSERT ON sessions
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'sessions dashboard scope required');
END;

CREATE TRIGGER sessions_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON sessions
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'sessions dashboard scope required');
END;

CREATE TRIGGER workspaces_require_dashboard_insert
BEFORE INSERT ON workspaces
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'workspaces dashboard scope required');
END;

CREATE TRIGGER workspaces_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON workspaces
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'workspaces dashboard scope required');
END;

CREATE TRIGGER workspace_sessions_require_dashboard_insert
BEFORE INSERT ON workspace_sessions
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'workspace_sessions dashboard scope required');
END;

CREATE TRIGGER workspace_sessions_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON workspace_sessions
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'workspace_sessions dashboard scope required');
END;

CREATE TRIGGER tasks_require_dashboard_insert
BEFORE INSERT ON tasks
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'tasks dashboard scope required');
END;

CREATE TRIGGER tasks_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON tasks
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'tasks dashboard scope required');
END;

CREATE TRIGGER mcp_relays_require_dashboard_insert
BEFORE INSERT ON mcp_relays
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'mcp_relays dashboard scope required');
END;

CREATE TRIGGER mcp_relays_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON mcp_relays
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'mcp_relays dashboard scope required');
END;

CREATE TRIGGER push_subscriptions_require_dashboard_insert
BEFORE INSERT ON push_subscriptions
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'push_subscriptions dashboard scope required');
END;

CREATE TRIGGER push_subscriptions_require_dashboard_update
BEFORE UPDATE OF dashboard_id ON push_subscriptions
WHEN NEW.dashboard_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'push_subscriptions dashboard scope required');
END;

CREATE TRIGGER sessions_require_scoped_worker_insert
BEFORE INSERT ON sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workers
  WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'session worker dashboard mismatch');
END;

CREATE TRIGGER sessions_require_scoped_worker_update
BEFORE UPDATE OF worker_fp, dashboard_id ON sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workers
  WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id
) OR EXISTS (
  SELECT 1 FROM workspace_sessions
  WHERE session_id = OLD.id AND dashboard_id <> NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'session worker dashboard mismatch');
END;

CREATE TRIGGER workspaces_require_scoped_worker_insert
BEFORE INSERT ON workspaces
WHEN NOT EXISTS (
  SELECT 1 FROM workers
  WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'workspace worker dashboard mismatch');
END;

CREATE TRIGGER workspaces_require_scoped_worker_update
BEFORE UPDATE OF worker_fp, dashboard_id ON workspaces
WHEN NOT EXISTS (
  SELECT 1 FROM workers
  WHERE fp = NEW.worker_fp AND dashboard_id = NEW.dashboard_id
) OR EXISTS (
  SELECT 1 FROM workspace_sessions
  WHERE workspace_id = OLD.id AND dashboard_id <> NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'workspace worker dashboard mismatch');
END;

CREATE TRIGGER workspace_sessions_require_scoped_parents_insert
BEFORE INSERT ON workspace_sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workspaces
  WHERE id = NEW.workspace_id AND dashboard_id = NEW.dashboard_id
) OR NOT EXISTS (
  SELECT 1 FROM sessions
  WHERE id = NEW.session_id AND dashboard_id = NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'workspace session dashboard mismatch');
END;

CREATE TRIGGER workspace_sessions_require_scoped_parents_update
BEFORE UPDATE OF workspace_id, session_id, dashboard_id ON workspace_sessions
WHEN NOT EXISTS (
  SELECT 1 FROM workspaces
  WHERE id = NEW.workspace_id AND dashboard_id = NEW.dashboard_id
) OR NOT EXISTS (
  SELECT 1 FROM sessions
  WHERE id = NEW.session_id AND dashboard_id = NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'workspace session dashboard mismatch');
END;

CREATE TRIGGER workers_preserve_child_dashboard_update
BEFORE UPDATE OF dashboard_id ON workers
WHEN EXISTS (
  SELECT 1 FROM sessions
  WHERE worker_fp = OLD.fp AND dashboard_id <> NEW.dashboard_id
) OR EXISTS (
  SELECT 1 FROM workspaces
  WHERE worker_fp = OLD.fp AND dashboard_id <> NEW.dashboard_id
)
BEGIN
  SELECT RAISE(ABORT, 'worker child dashboard mismatch');
END;


-- ─── carried over from v2 0025_worker_tombstones ───────────────────────────────────

CREATE INDEX workers_dashboard_active_idx
  ON workers(dashboard_id, fp)
  WHERE deleted_at_ms IS NULL;


-- ─── carried over from v2 0030_pair_request_provenance ─────────────────────────────

CREATE INDEX idx_pair_requests_status_expires
    ON pair_requests (status, expires_at_ms);
