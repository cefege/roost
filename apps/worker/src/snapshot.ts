// Emit snapshot SessionEvent on coord reconnect (R3.1).
// Coord reconciles: sessions in DB for this worker NOT in the snapshot
// get synthetic closed events. Ensures no ghost sessions after restart.
//
// 24a-5: routes through the same SessionEventSink as every other
// SessionEvent. Per phase-24.md R0.3 invariant, ONE emit boundary
// leaves the worker — no more split between snapshot (tRPC) + others
// (CoordLink). Reconciliation correctness depends on snapshot
// arriving AT coord before any browser query reads stale rows; the
// CoordLink pending queue is FIFO + drains in order on open, so
// snapshot ordering vs other events is preserved.

import type { SessionManager } from "./session-manager.ts";
import type { SessionEventSink } from "./event-sink.ts";
import type { WorkerFp } from "@roost/shared";
import { log } from "@roost/shared";

export async function emitSnapshot(opts: {
  mgr: SessionManager;
  sink: SessionEventSink;
  workerFp: WorkerFp;
}): Promise<void> {
  const { mgr, sink, workerFp } = opts;
  const live = mgr.allSessions();
  const sessions = live.map((r) => ({
    id: r.sessionId,
    worker_fp: workerFp,
    channel: r.channelId,
    kind: r.kind,
    cwd: r.cwd,
    workspace_id: null,
    status: "open" as const,
    created_at: Date.now(),
    closed_at: null,
    custom_title: null, // coord-owned; coord's snapshot fold preserves the real value
    git_branch: r.git_branch ?? null, // worker-authoritative; survives coord restart
    git_remote: r.git_remote ?? null,
    pr_number: r.pr?.number ?? null,  // retained on the record; re-announce so
    pr_state: r.pr?.state ?? null,    // coord's snapshot upsert keeps the badge
    pr_checks: r.pr?.checks ?? null,  // instead of clearing it on reconnect
    pr_url: r.pr?.url ?? null,
    ports: r.ports ?? [],             // re-announce so reconnect keeps the chips
  }));
  sink.emit({
    kind: "snapshot",
    worker_fp: workerFp,
    sessions,
    ts: Date.now(),
  });
  log.info("snapshot", "emitted", { count: sessions.length, workerFp });
}
