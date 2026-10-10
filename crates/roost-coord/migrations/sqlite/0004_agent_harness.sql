CREATE TABLE agent_conversations (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  worker_fp TEXT NOT NULL,
  worker_label TEXT NOT NULL,
  worker_os TEXT NOT NULL,
  cwd TEXT NOT NULL,
  model_provider TEXT,
  model_id TEXT,
  thinking_level TEXT NOT NULL,
  mode TEXT NOT NULL,
  pre_plan_model TEXT,
  parent_id TEXT REFERENCES agent_conversations(id) ON DELETE CASCADE,
  agent TEXT,
  advisor INTEGER CHECK (advisor IS NULL OR advisor IN (0, 1)),
  run_state TEXT NOT NULL,
  error TEXT,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL
);

CREATE TABLE agent_entries (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  entry_json TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  PRIMARY KEY (conversation_id, seq)
);

CREATE TABLE agent_credentials (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  provider TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('oauth', 'api_key')),
  identity_key TEXT NOT NULL,
  label TEXT NOT NULL,
  data_json TEXT NOT NULL,
  disabled_cause TEXT,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  UNIQUE (provider, identity_key)
);

CREATE TABLE agent_credential_blocks (
  credential_id INTEGER PRIMARY KEY REFERENCES agent_credentials(id) ON DELETE CASCADE,
  until_ms INTEGER NOT NULL
);

CREATE TABLE agent_credential_sticky (
  conversation_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  credential_id INTEGER NOT NULL,
  PRIMARY KEY (conversation_id, provider)
);

CREATE TABLE agent_settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);
