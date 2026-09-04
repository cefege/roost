// This module owns session row conversion and transactional projection writes.
// Event append transactions call it while holding the same Kysely transaction.
// It depends on shared wire shapes and tolerant persisted JSON parsing.
// Snapshot upserts must never overwrite coordinator-owned session fields.

import { safeJsonParse } from "@roost/shared/json";
import type { Session, SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "./db/connection.ts";
import type { SessionsTable } from "./db/schema.ts";

// The exact session projection column list, shared with every handler that
// reads sessions rows back for the same row→proto adapters.
export const SESSION_COLUMNS = [
  "id", "worker_fp", "channel", "kind", "cwd", "workspace_id", "status",
  "created_at", "closed_at", "custom_title", "git_branch", "git_remote",
  "pr_number", "pr_state", "pr_checks", "pr_url", "ports_json", "spawn_cwd",
] as const;

export function sessionToRow(
  session: Session,
  dashboardId: string,
): Omit<SessionsTable, "agent_json"> {
  return {
    id: session.id,
    dashboard_id: dashboardId,
    worker_fp: session.worker_fp,
    channel: session.channel,
    kind: session.kind,
    cwd: session.cwd,
    workspace_id: session.workspace_id,
    status: session.status,
    created_at: session.created_at,
    closed_at: session.closed_at ?? null,
    custom_title: session.custom_title ?? null,
    git_branch: session.git_branch ?? null,
    git_remote: session.git_remote ?? null,
    pr_number: session.pr_number ?? null,
    pr_state: session.pr_state ?? null,
    pr_checks: session.pr_checks ?? null,
    pr_url: session.pr_url ?? null,
    ports_json: session.ports && session.ports.length > 0 ? JSON.stringify(session.ports) : null,
    spawn_cwd: session.spawn_cwd ?? null,
  };
}

export async function loadSession(
  db: KyselyDB,
  id: string,
  dashboardId: string,
): Promise<Session | null> {
  const row = await db
    .selectFrom("sessions")
    .select([...SESSION_COLUMNS])
    .where("id", "=", id)
    .where("dashboard_id", "=", dashboardId)
    .executeTakeFirst();
  if (!row) return null;
  return {
    id: row.id as Session["id"],
    worker_fp: row.worker_fp as Session["worker_fp"],
    channel: row.channel as Session["channel"],
    kind: row.kind as Session["kind"],
    cwd: row.cwd,
    workspace_id: (row.workspace_id ?? null) as Session["workspace_id"],
    status: row.status as Session["status"],
    created_at: row.created_at,
    closed_at: row.closed_at ?? null,
    custom_title: row.custom_title ?? null,
    git_branch: row.git_branch ?? null,
    git_remote: row.git_remote ?? null,
    pr_number: row.pr_number ?? null,
    pr_state: (row.pr_state ?? null) as Session["pr_state"],
    pr_checks: (row.pr_checks ?? null) as Session["pr_checks"],
    pr_url: row.pr_url ?? null,
    // L11 safeJsonParse: a hand-edited/partial ports_json must not throw on the
    // bus.publish path (RPC 500 → split-brain). Fallback [] = no chips.
    ports: row.ports_json ? safeJsonParse<number[]>(row.ports_json, [], "session.ports") : [],
    spawn_cwd: row.spawn_cwd ?? null,
  };
}

// Delete a session's workspace_sessions junction row and, if that
// session was the LAST pane in any workspace, delete the workspace
// itself. Returns the orphaned workspace ids so the caller can emit
// workspaceBus `deleted` deltas after the surrounding tx commits.
//
// Used by BOTH the synthetic-ghost-close path inside the snapshot
// branch AND the regular `closed` event branch — previously only the
// regular branch ran the cascade, so a worker restart that snapshotted
// without a session left its workspace_sessions row + parent workspace
// orphaned in DB until the next coord-startup janitor.
export async function _cascadeClosedSession(
  trx: KyselyDB,
  dashboardId: string,
  sessionId: string,
): Promise<string[]> {
  const ownerRows = await trx
    .selectFrom("workspace_sessions")
    .select("workspace_id")
    .where("session_id", "=", sessionId)
    .where("dashboard_id", "=", dashboardId)
    .execute();
  const ownerIds = [...new Set(ownerRows.map((row) => row.workspace_id as string))];
  await trx
    .deleteFrom("workspace_sessions")
    .where("session_id", "=", sessionId)
    .where("dashboard_id", "=", dashboardId)
    .execute();
  if (ownerIds.length === 0) return [];
  const remaining = await trx
    .selectFrom("workspace_sessions")
    .select("workspace_id")
    .where("workspace_id", "in", ownerIds)
    .where("dashboard_id", "=", dashboardId)
    .execute();
  const stillHasSessions = new Set(remaining.map((row) => row.workspace_id as string));
  const orphanIds = ownerIds.filter((id) => !stillHasSessions.has(id));
  if (orphanIds.length > 0) {
    await trx.deleteFrom("workspaces")
      .where("id", "in", orphanIds)
      .where("dashboard_id", "=", dashboardId)
      .execute();
  }
  return orphanIds;
}

export async function projectSnapshotSessions(
  tx: KyselyDB,
  event: Extract<SessionEvent, { kind: "snapshot" }>,
  resolvedDashboardId: string,
): Promise<void> {
  // Breadcrumb model: sessions open in coord but ABSENT from this worker's
  // snapshot are NOT pruned. A worker restart kills the PTY, but the row
  // survives as an offline breadcrumb so the sidebar keeps showing where you
  // were working; a reconnect respawn re-binds it, and only an explicit
  // `closed` (real PTY exit) or the user's ✕ removes it. This mirrors
  // foldEvent's snapshot case (event.ts) so the SPA + coord projections stay
  // in agreement. Only the ANNOUNCED sessions are upserted below.
  for (const session of event.sessions) {
    const row = sessionToRow(session, resolvedDashboardId);
    // The conflict path mirrors foldEvent's snapshot case exactly: every
    // WORKER-owned column takes the announced value — `channel` above all,
    // because a reconcile can hand a session a new keeper channel, and a DB
    // row left on the old one re-primes the dead route on the next coord
    // restart (and lies to resolveSessionRoute's pre-reconcile fallback).
    // Existing coordinator/DB-owned fields are immutable across a worker
    // snapshot: original creation/spawn time, workspace grouping, custom
    // title, and dashboard scope.
    const {
      id: _id,
      dashboard_id: _dashboardId,
      created_at: _createdAt,
      workspace_id: _ws,
      custom_title: _ct,
      spawn_cwd: _sc,
      ...workerOwned
    } = row;
    await tx
      .insertInto("sessions")
      .values(row)
      .onConflict((oc) => oc.column("id").doUpdateSet(workerOwned))
      .execute();
  }
}
