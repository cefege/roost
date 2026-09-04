// Builds the exact reconnect snapshot from one SessionManager membership copy.
// Immutable launch fields come from admitted records while current terminal
// metadata repairs replaceable events after reconnect.
import type { WorkerFp, SessionEvent } from "@roost/shared/wire";
import type { SessionManager } from "./session-manager.ts";

export type WorkerSnapshotEvent = Extract<SessionEvent, { kind: "snapshot" }>;

/**
 * Copies SessionManager membership exactly once and builds the authoritative
 * reconnect barrier. Immutable launch fields come from the session record;
 * mutable terminal metadata comes from the same copied records.
 */
export function buildSnapshot(
  mgr: Pick<SessionManager, "allSessions">,
  workerFp: WorkerFp,
  now = Date.now(),
): WorkerSnapshotEvent {
  const records = mgr.allSessions();
  return {
    kind: "snapshot",
    worker_fp: workerFp,
    ts: now,
    sessions: records.map((record) => ({
      id: record.sessionId,
      worker_fp: workerFp,
      channel: record.channelId,
      kind: record.kind,
      cwd: record.cwd,
      spawn_cwd: record.shellSpec.cwd,
      workspace_id: null,
      status: "open" as const,
      created_at: record.spawnedAtMs,
      closed_at: null,
      custom_title: null,
      git_branch: record.git_branch ?? null,
      git_remote: record.git_remote ?? null,
      pr_number: record.pr?.number ?? null,
      pr_state: record.pr?.state ?? null,
      pr_checks: record.pr?.checks ?? null,
      pr_url: record.pr?.url ?? null,
      ports: record.ports ?? [],
    })),
  };
}
