// appendEvent: inserts into events table + folds into sessions projection +
// publishes to sessionBus. All in one Kysely transaction.
// Snapshot variant: reconciles ghost sessions (synthetic closes for any
// DB-open session from this worker not in the snapshot). R0.3, R3.1.

import type { KyselyDB } from "./db/connection.ts";
import { sessionBus, workspaceBus } from "./buses.ts";
import { foldEvent, asWorkspaceId } from "@roost/shared/wire";
import type { SessionEvent, Session } from "@roost/shared/wire";
import type { SessionsTable } from "./db/schema.ts";
import { log } from "@roost/shared/log";
import { safeJsonParse } from "@roost/shared/json";
import { classifyPushTransition, firePushForTransition } from "./push-dispatch.ts";

// ─── projection helpers ────────────────────────────────────────────────

function sessionToRow(s: Session): Omit<SessionsTable, never> {
  return {
    id: s.id,
    worker_fp: s.worker_fp,
    channel: s.channel,
    kind: s.kind,
    cwd: s.cwd,
    workspace_id: s.workspace_id,
    status: s.status,
    agent_json: s.agent ? JSON.stringify(s.agent) : null,
    created_at: s.created_at,
    closed_at: s.closed_at ?? null,
    custom_title: s.custom_title ?? null,
    git_branch: s.git_branch ?? null,
    git_remote: s.git_remote ?? null,
    pr_number: s.pr_number ?? null,
    pr_state: s.pr_state ?? null,
    pr_checks: s.pr_checks ?? null,
    pr_url: s.pr_url ?? null,
    ports_json: s.ports && s.ports.length > 0 ? JSON.stringify(s.ports) : null,
    spawn_cwd: s.spawn_cwd ?? null,
  };
}

async function loadSession(db: KyselyDB, id: string): Promise<Session | null> {
  const row = await db
    .selectFrom("sessions")
    .selectAll()
    .where("id", "=", id)
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
    agent: row.agent_json ? JSON.parse(row.agent_json) : null,
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

// ─── appendEvent ────────────────────────────────────────────────────────

/** Read back events with id > sinceId for backfill on reconnect. */
export async function getEventsSince(
  db: KyselyDB, sinceId: number, limit = 1000,
): Promise<Array<{ id: number; event: SessionEvent }>> {
  const rows = await db
    .selectFrom("events")
    .select(["id", "payload_json"])
    .where("id", ">", sinceId)
    .orderBy("id", "asc")
    .limit(limit)
    .execute();
  return rows.map(r => ({
    id: r.id as number,
    event: JSON.parse(r.payload_json as string) as SessionEvent,
  }));
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
async function _cascadeClosedSession(
  trx: KyselyDB,
  sessionId: string,
): Promise<string[]> {
  const ownerRows = await trx
    .selectFrom("workspace_sessions")
    .select("workspace_id")
    .where("session_id", "=", sessionId)
    .execute();
  const ownerIds = [...new Set(ownerRows.map((r) => r.workspace_id as string))];
  await trx
    .deleteFrom("workspace_sessions")
    .where("session_id", "=", sessionId)
    .execute();
  if (ownerIds.length === 0) return [];
  const remaining = await trx
    .selectFrom("workspace_sessions")
    .select("workspace_id")
    .where("workspace_id", "in", ownerIds)
    .execute();
  const stillHasSessions = new Set(remaining.map((r) => r.workspace_id as string));
  const orphanIds = ownerIds.filter((id) => !stillHasSessions.has(id));
  if (orphanIds.length > 0) {
    await trx.deleteFrom("workspaces").where("id", "in", orphanIds).execute();
  }
  return orphanIds;
}

/**
 * D-4b at-least-once delivery:
 *   - `opts.worker_fp` + `opts.client_seq` from the worker bidi let us
 *     dedup duplicate deliveries via UNIQUE INDEX (worker_fp, client_seq).
 *   - On dup conflict the INSERT no-ops, the projection update is
 *     skipped (already applied), and the function returns normally so
 *     the caller still sends DEventAck.
 *   - Non-worker producers (synthetic ghost closes, deploy lines) omit
 *     opts entirely; the partial index ignores rows with NULLs.
 */
export async function appendEvent(
  db: KyselyDB,
  event: SessionEvent,
  opts: { worker_fp: string | null; client_seq: number | null } = { worker_fp: null, client_seq: null },
): Promise<void> {
  let insertedId: number | undefined;
  let deduped = false;
  // Snapshot reap: session ids force-closed while their worker was offline but
  // re-announced live by the returning worker. Sent a kill AFTER commit.
  let reapOrphanIds: string[] = [];
  // Agent status transition, captured inside the tx for a post-commit push.
  let pushPrevStatus: string | undefined;
  let pushNextStatus: string | undefined;
  await db.transaction().execute(async (trx) => {
    // 1. Append to event log. If (worker_fp, client_seq) is present and
    //    already exists, the partial unique index makes this a conflict
    //    that we silently swallow via onConflict-doNothing.
    const inserted = await trx
      .insertInto("events")
      .values({
        kind: event.kind,
        session_id: "session_id" in event ? event.session_id : null,
        worker_fp: event.kind === "opened" ? event.worker_fp
          : event.kind === "snapshot" ? event.worker_fp
          : opts.worker_fp,
        payload_json: JSON.stringify(event),
        ts: event.ts,
        client_seq: opts.client_seq,
      })
      .onConflict((oc) => oc
        .columns(["worker_fp", "client_seq"])
        .where("worker_fp", "is not", null)
        .where("client_seq", "is not", null)
        .doNothing())
      .returning("id")
      .executeTakeFirst();
    insertedId = inserted?.id as number | undefined;
    if (insertedId === undefined && opts.client_seq !== null) {
      deduped = true;
      log.debug("event-log", "dedup_hit", { worker_fp: opts.worker_fp, client_seq: opts.client_seq });
      return; // already applied — skip projection + bus publish
    }

    // 2. Handle snapshot: reconcile ghost sessions + project new set.
    if (event.kind === "snapshot") {
      const liveIds = new Set(event.sessions.map((s) => s.id));

      // Force-closed reap: a snapshot id that ALREADY has a `closed` event is
      // an orphan PTY on a worker that returned after a force-close
      // (sessionsKill force=true while offline). session ids are never reused →
      // a closed event is a permanent tombstone, so this can't false-positive
      // on a live session. Skip the resurrection upsert below + reap the PTY
      // after commit. (No worker-service import here — that file imports this
      // one; the post-commit reap uses a dynamic import to dodge the cycle.)
      const liveIdList = [...liveIds];
      const tombstoned = liveIdList.length
        ? await trx.selectFrom("events").select("session_id")
            .where("kind", "=", "closed")
            .where("session_id", "in", liveIdList).execute()
        : [];
      const tombstonedIds = new Set(tombstoned.map((r) => r.session_id as string));
      reapOrphanIds = [...tombstonedIds];

      // Breadcrumb model: sessions open in coord but ABSENT from this worker's
      // snapshot are NOT pruned. A worker restart kills the PTY, but the row
      // survives as an offline breadcrumb so the sidebar keeps showing where you
      // were working; a reconnect respawn re-binds it, and only an explicit
      // `closed` (real PTY exit) or the user's ✕ removes it. This mirrors
      // foldEvent's snapshot case (event.ts) so the SPA + coord projections stay
      // in agreement. Only the ANNOUNCED sessions are upserted below.
      for (const s of event.sessions) {
        if (tombstonedIds.has(s.id)) continue; // force-closed orphan — don't resurrect
        await trx
          .insertInto("sessions")
          .values(sessionToRow(s))
          .onConflict((oc) => oc.column("id").doUpdateSet({
            cwd: s.cwd,
            status: s.status,
            agent_json: s.agent ? JSON.stringify(s.agent) : null,
            closed_at: s.closed_at ?? null,
            git_branch: s.git_branch ?? null,
            git_remote: s.git_remote ?? null,
            pr_number: s.pr_number ?? null,
            pr_state: s.pr_state ?? null,
            pr_checks: s.pr_checks ?? null,
            pr_url: s.pr_url ?? null,
            ports_json: s.ports && s.ports.length > 0 ? JSON.stringify(s.ports) : null,
          }))
          .execute();
      }

      sessionBus.publish(event);
      return;
    }

    // 3. Non-snapshot: fold event into projection.
    if (event.kind === "opened") {
      const prev = new Map<string, Session>();
      const next = foldEvent(prev, event);
      const s = next.get(event.session_id);
      if (!s) return;
      await trx
        .insertInto("sessions")
        .values(sessionToRow(s))
        .onConflict((oc) => oc.column("id").doNothing())
        .execute();
    } else if (event.kind === "closed") {
      // Terminal exited (or post-undo kill fired) → DELETE the row, don't
      // park it as status="closed". foldEvent now returns no row for closed,
      // so we can't reuse the update path below — delete + cascade directly.
      // No-op if it's already gone (idempotent on dedup / double-close).
      await trx
        .deleteFrom("sessions")
        .where("id", "=", event.session_id)
        .execute();
      const orphans = await _cascadeClosedSession(trx as unknown as KyselyDB, event.session_id);
      if (orphans.length > 0) {
        (event as SessionEvent & { _cascadeOrphans?: string[] })._cascadeOrphans = orphans;
      }
    } else if ("session_id" in event) {
      const existing = await loadSession(trx as unknown as KyselyDB, event.session_id);
      if (!existing) {
        log.warn("event-log", "session_not_found", { kind: event.kind, session_id: event.session_id });
        return;
      }
      const prev = new Map([[existing.id, existing]]);
      const next = foldEvent(prev, event);
      const updated = next.get(event.session_id);
      if (!updated) return;

      if (event.kind === "agent") {
        pushPrevStatus = existing.agent?.status;
        pushNextStatus = updated.agent?.status;
      }

      await trx
        .updateTable("sessions")
        .set(sessionToRow(updated))
        .where("id", "=", event.session_id)
        .execute();

      // Also update workspace_sessions junction on workspace_assigned.
      if (event.kind === "workspace_assigned") {
        await trx
          .deleteFrom("workspace_sessions")
          .where("session_id", "=", event.session_id)
          .execute();
        if (event.workspace_id) {
          await trx
            .insertInto("workspace_sessions")
            .values({
              workspace_id: event.workspace_id,
              session_id: event.session_id,
              added_at_ms: Date.now(),
            })
            .onConflict((oc) => oc.doNothing())
            .execute();
        }
      }
    }

    // 4. Broadcast (stamp the row id so SPA can track for reconnect backfill).
    if (insertedId !== undefined) {
      (event as SessionEvent & { _event_id?: number })._event_id = insertedId;
    }
    sessionBus.publish(event);
    const cascade = (event as SessionEvent & { _cascadeOrphans?: string[] })._cascadeOrphans;
    if (cascade && cascade.length > 0) {
      for (const id of cascade) {
        workspaceBus.publish({ kind: "deleted", id: asWorkspaceId(id) });
      }
    }
  });

  // Post-commit: reap force-closed orphans on the now-online worker. Dynamic
  // import dodges the worker-service↔event-log static cycle. Fire-and-forget;
  // the worker's `case "kill"` acks with an (idempotent) closed event.
  if (reapOrphanIds.length > 0 && event.kind === "snapshot") {
    const workerFp = event.worker_fp;
    const ids = reapOrphanIds;
    void import("./connect/worker-service.ts").then(({ sendBrowserCommand }) => {
      for (const sessionId of ids) {
        const ok = sendBrowserCommand(workerFp, {
          browser_id: "coord-reap", viewer_id: "coord-reap",
          request_id: crypto.randomUUID(),
          frame: { kind: "kill", session_id: sessionId },
        });
        log.info("event-log", ok ? "reap_orphan_kill_sent" : "reap_orphan_kill_no_socket",
          { session_id: sessionId, worker_fp: workerFp });
      }
    });
  }

  // Post-commit: fire OS Web Push for agent status transitions (blocked /
  // finished). Fire-and-forget, mirroring the reap block above — a push failure
  // must never affect the committed event.
  if (event.kind === "agent" && pushPrevStatus && pushNextStatus) {
    const pushKind = classifyPushTransition(pushPrevStatus, pushNextStatus);
    if (pushKind) void firePushForTransition(db, event.session_id, pushKind);
  }
}
