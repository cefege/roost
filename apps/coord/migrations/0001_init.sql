-- coordinator_v2.db initial schema. Fresh start; coexists with legacy
-- coordinator.db until R4.5 cutover. Tables reflect the R3 data model:
-- events table is append-only source of truth; sessions is a projection.

-- ─── identity ──────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS authorized_keys (
    fingerprint  TEXT    PRIMARY KEY,  -- hex SHA-256 of raw 32-byte ed25519 pubkey
    public_key   BLOB    NOT NULL,     -- raw 32 bytes
    label        TEXT    NOT NULL,
    added_at     INTEGER NOT NULL      -- unix epoch ms
);

-- ─── workers ───────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS workers (
    fp               TEXT    PRIMARY KEY,   -- hex SHA-256 of ed25519 pubkey
    label            TEXT    NOT NULL,
    reachable_addr   TEXT    NOT NULL,      -- tailnet FQDN; no scheme/port
    ssh_port         INTEGER NOT NULL,
    ws_listen_port   INTEGER NOT NULL,
    ws_scheme        TEXT    NOT NULL,      -- 'ws' | 'wss'
    os               TEXT    NOT NULL,      -- 'darwin' | 'linux'
    git_sha          TEXT,
    host_metrics_json TEXT,                 -- serialized HostMetrics | NULL
    registered_at_ms INTEGER NOT NULL,
    last_seen_ms     INTEGER NOT NULL
);

-- ─── session event log (append-only) ──────────────────────────────────

CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL,      -- session event discriminator
    session_id   TEXT,                  -- NULL for 'snapshot'
    worker_fp    TEXT,                  -- NULL except 'opened' + 'snapshot'
    payload_json TEXT    NOT NULL,      -- full SessionEvent JSON
    ts           INTEGER NOT NULL       -- unix epoch ms from the event itself
);

CREATE INDEX IF NOT EXISTS events_by_session  ON events(session_id) WHERE session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS events_by_worker   ON events(worker_fp)  WHERE worker_fp  IS NOT NULL;
CREATE INDEX IF NOT EXISTS events_by_ts       ON events(ts);

-- ─── sessions projection (folded from events) ─────────────────────────

CREATE TABLE IF NOT EXISTS sessions (
    id           TEXT    PRIMARY KEY,   -- uuid
    worker_fp    TEXT    NOT NULL REFERENCES workers(fp),
    channel      INTEGER NOT NULL,      -- worker-local PTY id
    kind         TEXT    NOT NULL,      -- 'shell'
    cwd          TEXT    NOT NULL,
    workspace_id TEXT,                  -- NULL = orphan
    status       TEXT    NOT NULL,      -- 'open' | 'closed'
    agent_json   TEXT,                  -- preserved structured-session history | NULL
    created_at   INTEGER NOT NULL,
    closed_at    INTEGER
);

CREATE INDEX IF NOT EXISTS sessions_by_worker    ON sessions(worker_fp);
CREATE INDEX IF NOT EXISTS sessions_open         ON sessions(status) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS sessions_by_workspace ON sessions(workspace_id) WHERE workspace_id IS NOT NULL;

-- ─── workspaces ────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS workspaces (
    id           TEXT    PRIMARY KEY,   -- uuid
    worker_fp    TEXT    NOT NULL,      -- pinned to one worker
    name         TEXT    NOT NULL,
    color        TEXT,
    position     INTEGER NOT NULL DEFAULT 0,
    version      INTEGER NOT NULL DEFAULT 0,  -- CAS counter for If-Match
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_sessions (
    workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    session_id   TEXT    NOT NULL,
    added_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, session_id)
);

-- ─── bootstrap tokens ──────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS bootstrap_tokens (
    token            TEXT    PRIMARY KEY,
    kind             TEXT    NOT NULL,      -- 'worker' | 'browser'
    label            TEXT    NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    expires_at_ms    INTEGER NOT NULL,
    used_at_ms       INTEGER,               -- NULL = unused; one-shot
    used_by_fp       TEXT
);

-- ─── tap-to-pair ───────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS pair_requests (
    id           TEXT    PRIMARY KEY,   -- uuid
    ephemeral_id TEXT    NOT NULL UNIQUE,
    public_key   BLOB    NOT NULL,      -- raw 32 bytes
    label        TEXT    NOT NULL,
    status       TEXT    NOT NULL,      -- 'pending' | 'approved' | 'denied'
    created_at_ms INTEGER NOT NULL,
    decided_at_ms INTEGER
);

-- ─── tasks ─────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS tasks (
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
    claim_ttl_ms                  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS tasks_state ON tasks(state, enqueued_at_ms);

-- ─── webhook tokens ────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS webhook_tokens (
    id             TEXT    PRIMARY KEY,  -- uuid
    label          TEXT    NOT NULL,
    hash           TEXT    NOT NULL UNIQUE,  -- hex SHA-256 of plaintext
    last4          TEXT    NOT NULL,
    scopes_json    TEXT    NOT NULL,     -- JSON array of WebhookScope
    created_at_ms  INTEGER NOT NULL,
    last_used_at_ms INTEGER
);

-- ─── permission rules ──────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS permission_rules (
    id            TEXT    PRIMARY KEY,  -- uuid
    tool_pattern  TEXT    NOT NULL,
    folder_glob   TEXT    NOT NULL,
    decision      TEXT    NOT NULL,     -- 'allow' | 'deny' | 'allow-and-remember'
    enabled       INTEGER NOT NULL DEFAULT 1,  -- 0 | 1
    created_at_ms INTEGER NOT NULL
);

-- ─── MCP relays ────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS mcp_relays (
    id            TEXT    PRIMARY KEY,  -- uuid
    label         TEXT    NOT NULL,
    kind          TEXT    NOT NULL,     -- 'stdio' | 'sse'
    config_json   TEXT    NOT NULL,     -- free-form JSON
    created_at_ms INTEGER NOT NULL
);

-- ─── audit log ─────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS audit_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    caller_fp    TEXT,
    method       TEXT    NOT NULL,      -- HTTP method
    path         TEXT    NOT NULL,
    status       INTEGER NOT NULL,
    trace_id     TEXT
);

CREATE INDEX IF NOT EXISTS audit_log_ts ON audit_log(ts);

-- ─── migration meta ────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS _migrations (
    name         TEXT    PRIMARY KEY,
    applied_at   INTEGER NOT NULL
);
