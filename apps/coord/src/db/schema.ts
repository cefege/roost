// Compile-time Kysely types for the coordinator's SQLite database. These
// interfaces are NOT checked against the real schema: drift from
// migrations/*.sql only surfaces as runtime SQLite errors, so every column
// change requires a new migration file AND this interface updated together.
import type { ColumnType } from "kysely";

// Kysely DB interface — one interface per table, then composed into DB.
// Column types mirror the SQLite types from migrations/0001_init.sql.
// Nullable columns use `T | null`. JSON-blob columns are `string` (raw).

export interface WorkersTable {
  fp: string;
  dashboard_id: string;
  label: string;
  os: string;
  git_sha: string | null;
  host_metrics_json: string | null;
  registered_at_ms: number;
  last_seen_ms: number;
  deleted_at_ms: ColumnType<
    number | null,
    number | null | undefined,
    number | null
  >;
  // Re-added in migration 0005 for the SPA right-click "Screen Share"
  // / "SSH" menu. Stored from the deploy-time ROOST_REACHABLE_ADDR;
  // worker no longer has an inbound surface so this is informational
  // only (not used by coord to dial the worker).
  reachable_addr: string | null;
  // Static worker-sampled model, chip, and Linux distribution. Old workers and
  // rows predate this optional display metadata.
  host_identity_json: ColumnType<
    string | null,
    string | null | undefined,
    string | null
  >;
  // Authenticated post-reconciliation KeeperRuntimeObservationV1. Heartbeats
  // clear it to NULL whenever fresh complete proof is unavailable.
  keeper_runtime_json: ColumnType<
    string | null,
    string | null | undefined,
    string | null
  >;
  // Worker-owned live terminal-core admission report. NULL is fail-closed for
  // workers that have not yet sent a valid capacity heartbeat.
  terminal_core_capacity_json: ColumnType<
    string | null,
    string | null | undefined,
    string | null
  >;
}

export interface EventsTable {
  id?: number;               // autoincrement — omit on INSERT
  dashboard_id: string;
  kind: string;
  session_id: string | null;
  worker_fp: string | null;
  payload_json: string;
  ts: number;
  client_seq?: number | null; // D-4b: per-worker monotonic outbox seq
}

export interface SessionsTable {
  id: string;
  dashboard_id: string;
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
  // Private worker-recovery projection. Null JSON with a non-null sequence is
  // a durable clear; neither column belongs in public Session reads.
  agent_reference_json: ColumnType<
    string | null,
    string | null | undefined,
    string | null
  >;
  agent_reference_client_seq: ColumnType<
    number | null,
    number | null | undefined,
    number | null
  >;
}

export interface WorkspacesTable {
  id: string;
  dashboard_id: string;
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
  dashboard_id: string;
  session_id: string;
  added_at_ms: number;
}

export interface BootstrapTokensTable {
  token_hash: string;
  account_id: string;
  dashboard_id: string;
  kind: "worker" | "browser";
  label: string;
  created_at_ms: number;
  expires_at_ms: number;
  used_at_ms: number | null;
  used_by_fp: string | null;
  minted_by_fp: string | null;
}

export interface CoordinatorRelocationRedemptionsTable {
  jti: string;
  account_id: string;
  redeemed_at_ms: number;
  expires_at_ms: number;
  used_by_fp: string;
  delegated_by_fp: string;
}

export interface PairRequestsTable {
  id: string;
  ephemeral_id: string;
  public_key: Uint8Array;
  label: string;
  status: string;
  created_at_ms: number;
  decided_at_ms: number | null;
  user_agent: string | null;
  client_browser: string | null;
  client_os: string | null;
  client_device_type: string | null;
  source_ip: string | null;
  country_code: string | null;
  region: string | null;
  city: string | null;
  edge_identity_provider: string | null;
  edge_identity: string | null;
  edge_identity_verified: number;
  expires_at_ms: number;
}

export interface TasksTable {
  id: string;
  dashboard_id: string;
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


export interface McpRelaysTable {
  id: string;
  dashboard_id: string;
  label: string;
  kind: string;
  config_json: string;
  created_at_ms: number;
}

export interface AuditLogTable {
  id?: number;               // autoincrement — omit on INSERT
  dashboard_id: string | null;
  ts: number;
  caller_fp: string | null;
  method: string;
  path: string;
  status: number;
  trace_id: string | null;
}

export interface AuthorizedKeysTable {
  fingerprint: string;
  public_key: Uint8Array;
  label: string;
  added_at: number;
  paired_from_ip: string | null;
  paired_country: string | null;
  paired_user_agent: string | null;
  paired_edge_identity: string | null;
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
// Web Push subscriptions, keyed by authenticated browser fingerprint and
// endpoint. viewer_fp references authorized_keys with ON DELETE CASCADE;
// p256dh/auth are RFC 8291 client encryption keys.
export interface PushSubscriptionsTable {
  dashboard_id: string;
  viewer_fp: string;
  endpoint: string;
  p256dh: string;
  auth: string;
  created_at_ms: number;
}

export interface AccountsTable {
  id: string;
  email_normalized: string;
  status: string;
  created_at_ms: number;
}

export interface AccountDevicesTable {
  fingerprint: string;
  account_id: string;
  added_at_ms: number;
  last_seen_at_ms: number;
}

export interface OrganizationsTable {
  id: string;
  slug: string;
  name: string;
  status: string;
  created_at_ms: number;
}

export interface OrganizationMembershipsTable {
  organization_id: string;
  account_id: string;
  role: string;
  created_at_ms: number;
}

export interface DashboardsTable {
  id: string;
  organization_id: string;
  slug: string;
  name: string;
  status: string;
  created_at_ms: number;
}

export interface DashboardMembershipsTable {
  dashboard_id: string;
  account_id: string;
  role: string;
  created_at_ms: number;
}

// ─── Kysely DB type ────────────────────────────────────────────────────

export interface DB {
  accounts: AccountsTable;
  account_devices: AccountDevicesTable;
  organizations: OrganizationsTable;
  organization_memberships: OrganizationMembershipsTable;
  dashboards: DashboardsTable;
  dashboard_memberships: DashboardMembershipsTable;
  workers: WorkersTable;
  events: EventsTable;
  sessions: SessionsTable;
  workspaces: WorkspacesTable;
  workspace_sessions: WorkspaceSessionsTable;
  bootstrap_tokens: BootstrapTokensTable;
  coordinator_relocation_redemptions: CoordinatorRelocationRedemptionsTable;
  pair_requests: PairRequestsTable;
  tasks: TasksTable;
  mcp_relays: McpRelaysTable;
  audit_log: AuditLogTable;
  authorized_keys: AuthorizedKeysTable;
  authorized_key_revocations: AuthorizedKeyRevocationsTable;
  app_settings: AppSettingsTable;
  push_subscriptions: PushSubscriptionsTable;
  _migrations: MigrationsTable;
}

// app_settings — coord-held key→value config (migration 0006). First use:
// voice-transcription secrets + options. value holds a plain string (raw API
// key) or a JSON blob; callers own the encoding per key.
export interface AppSettingsTable {
  dashboard_id: string | null;
  key: string;
  value: string;
  updated_at_ms: number;
}
