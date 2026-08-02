import type { ColumnType } from "kysely";

// Kysely DB interface — one interface per table, then composed into DB.
// Column types mirror the SQLite types from migrations/0001_init.sql.
// Nullable columns use `T | null`. JSON-blob columns are `string` (raw).

export interface WorkersTable {
  fp: string;
  label: string;
  os: string;
  git_sha: string | null;
  host_metrics_json: string | null;
  registered_at_ms: number;
  last_seen_ms: number;
  // Re-added in migration 0005 for the SPA right-click "Screen Share"
  // / "SSH" menu. Stored from the deploy-time ROOST_REACHABLE_ADDR;
  // worker no longer has an inbound surface so this is informational
  // only (not used by coord to dial the worker).
  reachable_addr: string | null;
  // Non-null = this worker's keeper is running stale code (value = the running
  // keeper's short build stamp); null = current. Set from heartbeat; drives the
  // MachinesPane badge. Added migration 0006.
  keeper_stale: string | null;
}

export interface EventsTable {
  id?: number;               // autoincrement — omit on INSERT
  kind: string;
  session_id: string | null;
  worker_fp: string | null;
  payload_json: string;
  ts: number;
  client_seq?: number | null; // D-4b: per-worker monotonic outbox seq
}

export interface SessionsTable {
  id: string;
  worker_fp: string;
  channel: number;
  kind: string;
  cwd: string;
  workspace_id: string | null;
  status: string;
  agent_json: ColumnType<string | null, undefined, never>;
  created_at: number;
  closed_at: number | null;
  custom_title: string | null; // user rename; null = auto title (sessionTitle.ts)
  git_branch: string | null;   // local branch of cwd (worker-resolved); null = not a repo
  git_remote: string | null;   // github owner/repo of origin (worker-resolved); null = none
  pr_number: number | null;    // github PR # for git_branch (worker gh); null = no PR
  pr_state: string | null;     // open|merged|closed|draft
  pr_checks: string | null;    // passing|failing|pending|none
  pr_url: string | null;       // PR html_url for click-through
  ports_json: string | null;   // JSON int[] of LISTEN ports (worker lsof); null = none
  spawn_cwd: string | null;    // immutable spawn folder (set on `opened`); backs /t/ URL; null = pre-migration
}

export interface WorkspacesTable {
  id: string;
  worker_fp: string;
  name: string;
  folder_path: string;  // workspace IS a folder; panes spawn here
  color: string | null;
  position: number;
  version: number;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface WorkspaceSessionsTable {
  workspace_id: string;
  session_id: string;
  added_at_ms: number;
}

export interface BootstrapTokensTable {
  token: string;
  kind: string;
  label: string;
  created_at_ms: number;
  expires_at_ms: number;
  used_at_ms: number | null;
  used_by_fp: string | null;
  minted_by_fp: string | null;
}

export interface PairRequestsTable {
  id: string;
  ephemeral_id: string;
  public_key: Uint8Array;
  label: string;
  status: string;
  created_at_ms: number;
  decided_at_ms: number | null;
}

export interface TasksTable {
  id: string;
  state: string;
  payload_json: string;
  enqueued_at_ms: number;
  claimed_at_ms: number | null;
  claimed_by: string | null;
  finished_at_ms: number | null;
  result_json: string | null;
  completion_check: string | null;
  completion_check_last_attempt_ms: number | null;
  claim_ttl_ms: number;
}

export interface WebhookTokensTable {
  id: string;
  label: string;
  hash: string;
  last4: string;
  scopes_json: string;
  created_at_ms: number;
  last_used_at_ms: number | null;
}

export interface PermissionRulesTable {
  id: string;
  tool_pattern: string;
  folder_glob: string;
  decision: string;
  enabled: number;  // 0 | 1
  created_at_ms: number;
}

export interface McpRelaysTable {
  id: string;
  label: string;
  kind: string;
  config_json: string;
  created_at_ms: number;
}

export interface AuditLogTable {
  id?: number;               // autoincrement — omit on INSERT
  ts: number;
  caller_fp: string | null;
  method: string;
  path: string;
  status: number;
  trace_id: string | null;
}

export interface AgentEntriesTable {
  session_id: string;
  seq: number;
  ts: number;
  entry_json: string;
}


export interface AuthorizedKeysTable {
  fingerprint: string;
  public_key: Uint8Array;
  label: string;
  added_at: number;
}

export interface AuthorizedKeyRevocationsTable {
  fingerprint: string;
  revoked_at_ms: number;
  revoked_by_fp: string;
  reason: string;
}

export interface MigrationsTable {
  name: string;
  applied_at: number;
}

// ─── Kysely DB type ────────────────────────────────────────────────────

export interface DB {
  workers: WorkersTable;
  events: EventsTable;
  sessions: SessionsTable;
  workspaces: WorkspacesTable;
  workspace_sessions: WorkspaceSessionsTable;
  bootstrap_tokens: BootstrapTokensTable;
  pair_requests: PairRequestsTable;
  tasks: TasksTable;
  webhook_tokens: WebhookTokensTable;
  permission_rules: PermissionRulesTable;
  mcp_relays: McpRelaysTable;
  audit_log: AuditLogTable;
  agent_entries: AgentEntriesTable;
  authorized_keys: AuthorizedKeysTable;
  authorized_key_revocations: AuthorizedKeyRevocationsTable;
  app_settings: AppSettingsTable;
  _migrations: MigrationsTable;
}

// app_settings — coord-held key→value config (migration 0006). First use:
// voice-transcription secrets + options. value holds a plain string (raw API
// key) or a JSON blob; callers own the encoding per key.
export interface AppSettingsTable {
  key: string;
  value: string;
  updated_at_ms: number;
}
