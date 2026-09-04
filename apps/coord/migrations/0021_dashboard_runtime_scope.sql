-- Dashboard ownership stays nullable through the explicit legacy bootstrap.
-- New runtime writes must be rejected by application admission until a later
-- constraint migration can make these columns non-null.
ALTER TABLE workers ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE bootstrap_tokens ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE events ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE sessions ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE workspaces ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE workspace_sessions ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE tasks ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE webhook_tokens ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE permission_rules ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE mcp_relays ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);
ALTER TABLE audit_log ADD COLUMN dashboard_id TEXT REFERENCES dashboards(id);

-- These tables' original keys were global. Rebuild them to allow the same
-- device or setting key in separate dashboards while copying legacy rows
-- exactly with NULL scope for bootstrap-legacy to assign.
CREATE TABLE app_settings_dashboard_scope (
  dashboard_id TEXT REFERENCES dashboards(id),
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, key)
);
INSERT INTO app_settings_dashboard_scope (dashboard_id, key, value, updated_at_ms)
  SELECT NULL, key, value, updated_at_ms FROM app_settings;
DROP TABLE app_settings;
ALTER TABLE app_settings_dashboard_scope RENAME TO app_settings;

CREATE TABLE push_subscriptions_dashboard_scope (
  dashboard_id TEXT REFERENCES dashboards(id),
  viewer_fp TEXT NOT NULL,
  endpoint TEXT NOT NULL,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, viewer_fp, endpoint)
);
INSERT INTO push_subscriptions_dashboard_scope
  (dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms)
  SELECT NULL, viewer_fp, endpoint, p256dh, auth, created_at_ms
  FROM push_subscriptions;
DROP TABLE push_subscriptions;
ALTER TABLE push_subscriptions_dashboard_scope RENAME TO push_subscriptions;

CREATE INDEX workers_dashboard_idx ON workers(dashboard_id, fp);
CREATE INDEX bootstrap_tokens_dashboard_idx ON bootstrap_tokens(dashboard_id, kind, used_at_ms);
CREATE INDEX events_dashboard_replay_idx ON events(dashboard_id, id);
CREATE INDEX sessions_dashboard_idx ON sessions(dashboard_id, id);
CREATE INDEX sessions_dashboard_worker_idx ON sessions(dashboard_id, worker_fp);
CREATE INDEX workspaces_dashboard_idx ON workspaces(dashboard_id, id);
CREATE INDEX workspace_sessions_dashboard_session_idx ON workspace_sessions(dashboard_id, session_id);
CREATE INDEX tasks_dashboard_idx ON tasks(dashboard_id, id);
CREATE INDEX webhook_tokens_dashboard_idx ON webhook_tokens(dashboard_id, id);
CREATE INDEX permission_rules_dashboard_idx ON permission_rules(dashboard_id, id);
CREATE INDEX mcp_relays_dashboard_idx ON mcp_relays(dashboard_id, id);
CREATE INDEX audit_log_dashboard_ts_idx ON audit_log(dashboard_id, ts, id);
CREATE INDEX app_settings_dashboard_key_idx ON app_settings(dashboard_id, key);
CREATE INDEX push_subscriptions_dashboard_viewer_idx ON push_subscriptions(dashboard_id, viewer_fp, endpoint);
