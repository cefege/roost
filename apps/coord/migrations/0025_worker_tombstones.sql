-- Preserves deleted worker identity as tombstones with structural history FKs.
-- It repairs legacy dangling joins before rebuilding workspace relationships.
-- The migration runner validates foreign keys before committing this cutover.

ALTER TABLE workers ADD COLUMN deleted_at_ms INTEGER;

-- A historical revocation already consumed the worker credential. Convert the
-- worker row into the referential tombstone and remove any key that was left
-- active alongside the revocation.
UPDATE workers
SET deleted_at_ms = (
  SELECT revocation.revoked_at_ms
  FROM authorized_key_revocations AS revocation
  WHERE revocation.fingerprint = workers.fp
)
WHERE EXISTS (
  SELECT 1
  FROM authorized_key_revocations AS revocation
  WHERE revocation.fingerprint = workers.fp
);

DELETE FROM authorized_keys
WHERE EXISTS (
  SELECT 1
  FROM workers
  WHERE workers.fp = authorized_keys.fingerprint
    AND workers.deleted_at_ms IS NOT NULL
);

CREATE INDEX workers_dashboard_active_idx
  ON workers(dashboard_id, fp)
  WHERE deleted_at_ms IS NULL;

-- Rebuilding the workspace parent requires rebuilding its child while foreign
-- keys remain enabled. Remove the cross-table triggers first and restore the
-- complete 0024 write boundary after both staged tables have their final names.
DROP TRIGGER IF EXISTS workers_require_dashboard_insert;
DROP TRIGGER IF EXISTS workers_require_dashboard_update;
DROP TRIGGER IF EXISTS events_require_dashboard_insert;
DROP TRIGGER IF EXISTS events_require_dashboard_update;
DROP TRIGGER IF EXISTS sessions_require_dashboard_insert;
DROP TRIGGER IF EXISTS sessions_require_dashboard_update;
DROP TRIGGER IF EXISTS workspaces_require_dashboard_insert;
DROP TRIGGER IF EXISTS workspaces_require_dashboard_update;
DROP TRIGGER IF EXISTS workspace_sessions_require_dashboard_insert;
DROP TRIGGER IF EXISTS workspace_sessions_require_dashboard_update;
DROP TRIGGER IF EXISTS tasks_require_dashboard_insert;
DROP TRIGGER IF EXISTS tasks_require_dashboard_update;
DROP TRIGGER IF EXISTS mcp_relays_require_dashboard_insert;
DROP TRIGGER IF EXISTS mcp_relays_require_dashboard_update;
DROP TRIGGER IF EXISTS push_subscriptions_require_dashboard_insert;
DROP TRIGGER IF EXISTS push_subscriptions_require_dashboard_update;
DROP TRIGGER IF EXISTS sessions_require_scoped_worker_insert;
DROP TRIGGER IF EXISTS sessions_require_scoped_worker_update;
DROP TRIGGER IF EXISTS workspaces_require_scoped_worker_insert;
DROP TRIGGER IF EXISTS workspaces_require_scoped_worker_update;
DROP TRIGGER IF EXISTS workspace_sessions_require_scoped_parents_insert;
DROP TRIGGER IF EXISTS workspace_sessions_require_scoped_parents_update;
DROP TRIGGER IF EXISTS workers_preserve_child_dashboard_update;

-- These rows predate a workspace-to-worker foreign key and already have no
-- historical worker anchor. Their junction rows disappear through the existing
-- workspace ON DELETE CASCADE; every workspace backed by a worker is retained.
DELETE FROM workspaces
WHERE NOT EXISTS (
  SELECT 1 FROM workers WHERE workers.fp = workspaces.worker_fp
);

-- 0024 deliberately tolerated legacy junction breadcrumbs whose parent was
-- already absent. Repair only those dangling links before both parent FKs
-- become structural; valid workspace/session history is copied unchanged.
DELETE FROM workspace_sessions
WHERE NOT EXISTS (
  SELECT 1
  FROM workspaces
  WHERE workspaces.id = workspace_sessions.workspace_id
) OR NOT EXISTS (
  SELECT 1
  FROM sessions
  WHERE sessions.id = workspace_sessions.session_id
);

CREATE TABLE workspaces_new (
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

INSERT INTO workspaces_new (
  id,
  worker_fp,
  name,
  color,
  position,
  version,
  created_at_ms,
  updated_at_ms,
  folder_path,
  dashboard_id
)
SELECT
  id,
  worker_fp,
  name,
  color,
  position,
  version,
  created_at_ms,
  updated_at_ms,
  folder_path,
  dashboard_id
FROM workspaces;

-- Point the staged child at the staged parent. SQLite rewrites this reference
-- to workspaces when the parent receives its final name below.
CREATE TABLE workspace_sessions_new (
  workspace_id TEXT NOT NULL REFERENCES workspaces_new(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  added_at_ms INTEGER NOT NULL,
  dashboard_id TEXT REFERENCES dashboards(id),
  PRIMARY KEY (workspace_id, session_id)
);

INSERT INTO workspace_sessions_new (
  workspace_id,
  session_id,
  added_at_ms,
  dashboard_id
)
SELECT
  workspace_id,
  session_id,
  added_at_ms,
  dashboard_id
FROM workspace_sessions;

DROP TABLE workspace_sessions;
DROP TABLE workspaces;
ALTER TABLE workspaces_new RENAME TO workspaces;
ALTER TABLE workspace_sessions_new RENAME TO workspace_sessions;

CREATE INDEX workspaces_dashboard_idx ON workspaces(dashboard_id, id);
CREATE INDEX workspace_sessions_dashboard_session_idx
  ON workspace_sessions(dashboard_id, session_id);

-- Mandatory dashboard scope guards from 0024.
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

-- Cross-dashboard relationship guards from 0024.
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
