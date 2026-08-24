// appendEvent: one Kysely transaction inserts into the events table and folds
// the sessions projection. DURABLE PUBLICATION then happens strictly AFTER
// commit, in order: the byte-hub channel index (applyDurableChannelIndex),
// then sessionBus for browser Sync. So no browser can observe
// `opened`/`respawned`/`snapshot` before that event's exact authenticated
// worker/channel binding is installed — publishing from inside the
// transaction used to race Sync fan-out ahead of the binding, and the first
// claim/keystroke after a respawn was routed into a channel no keeper owned.
// Snapshot variant: upserts the announced set exactly, keeps absent sessions as
// offline breadcrumb rows, and reaps force-closed orphans after commit. The
// channel index is replaced exactly for the announcing worker. R0.3, R3.1.

import type { KyselyDB } from "./db/connection.ts";
import { sessionBus, workspaceBus } from "./buses.ts";
import { applyDurableChannelIndex } from "./byte-hub.ts";
import { foldEvent, asWorkspaceId } from "@roost/shared/wire";
import type { SessionEvent, Session, WorkerFp } from "@roost/shared/wire";
import type { SessionsTable } from "./db/schema.ts";
import { log } from "@roost/shared/log";
import { safeJsonParse } from "@roost/shared/json";

// ─── projection helpers ────────────────────────────────────────────────

// The exact session projection column list, shared with every handler that
// reads sessions rows back for the same row→proto adapters.
export const SESSION_COLUMNS = [
  "id", "worker_fp", "channel", "kind", "cwd", "workspace_id", "status",
  "created_at", "closed_at", "custom_title", "git_branch", "git_remote",
  "pr_number", "pr_state", "pr_checks", "pr_url", "ports_json", "spawn_cwd",
] as const;

function sessionToRow(s: Session): Omit<SessionsTable, "agent_json"> {
  return {
    id: s.id,
    worker_fp: s.worker_fp,
    channel: s.channel,
    kind: s.kind,
    cwd: s.cwd,
    workspace_id: s.workspace_id,
    status: s.status,
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
    .select([...SESSION_COLUMNS])
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

/** Capture a durable recovery cutoff after the live bus subscription exists. */
export async function getEventMaxId(db: KyselyDB): Promise<number> {
  const row = await db
    .selectFrom("events")
    .select(({ fn }) => fn.max<number>("id").as("max_id"))
    .executeTakeFirst();
  return Number(row?.max_id ?? 0);
}

/** Page one stable recovery interval: cursor < id <= cutoff. */
export async function getEventsThrough(
  db: KyselyDB,
  cursor: number,
  cutoff: number,
  limit = 256,
): Promise<Array<{ id: number; event: SessionEvent }>> {
  const rows = await db
    .selectFrom("events")
    .select(["id", "payload_json"])
    .where("id", ">", cursor)
    .where("id", "<=", cutoff)
    .orderBy("id", "asc")
    .limit(limit)
    .execute();
  return rows.map((row) => ({
    id: Number(row.id),
    event: JSON.parse(row.payload_json) as SessionEvent,
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
  opts: {
    worker_fp: string | null;
    client_seq: number | null;
    /** Auxiliary writes that MUST commit atomically with the event (e.g.
     *  junction-table moves). Runs inside the transaction, BEFORE the
     *  projection fold, so it can still read pre-event state. Bus/channel
     *  publication stays strictly post-commit — never publish from here. */
    extraWork?: (trx: KyselyDB) => Promise<void>;
  } = { worker_fp: null, client_seq: null },
): Promise<void> {
  let insertedId: number | undefined;
  let deduped = false;
  // Snapshot reap: session ids force-closed while their worker was offline but
  // re-announced live by the returning worker. Sent a kill AFTER commit.
  let reapOrphanIds: string[] = [];
  // Publication decision, taken inside the transaction: a dedupe hit or a
  // fold that produced no row must not reach the channel index or the bus.
  let publishable = false;
  // Workspaces orphaned by a `closed` cascade; their deltas publish after commit.
  let cascadeOrphans: string[] = [];
  await db.transaction().execute(async (trx) => {
    if (opts.extraWork) await opts.extraWork(trx as unknown as KyselyDB);

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
        const row = sessionToRow(s);
        // The conflict path mirrors foldEvent's snapshot case exactly: every
        // WORKER-owned column takes the announced value — `channel` above all,
        // because a reconcile can hand a session a new keeper channel, and a DB
        // row left on the old one re-primes the dead route on the next coord
        // restart (and lies to resolveSessionRoute's pre-reconcile fallback).
        // Only the three coord/DB-owned columns are preserved: the worker
        // announces them null in every snapshot, so writing them would drop a
        // rename, collapse sidebar grouping, or lose the original spawn cwd.
        const { id: _id, workspace_id: _ws, custom_title: _ct, spawn_cwd: _sc, ...workerOwned } = row;
        await trx
          .insertInto("sessions")
          .values(row)
          .onConflict((oc) => oc.column("id").doUpdateSet(workerOwned))
          .execute();
      }

      publishable = true;
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
      cascadeOrphans = await _cascadeClosedSession(trx as unknown as KyselyDB, event.session_id);
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

    publishable = true;
  });

  if (publishable) {
    // Durable publication, in order. The fingerprint is the one that
    // authenticated the worker socket (JWT-verified 64-hex upstream), so it
    // needs no re-parse here: this step runs post-commit and must never throw.
    const authenticatedFp = opts.worker_fp as WorkerFp | null;
    applyDurableChannelIndex(event, authenticatedFp);
    // Stamp the row id so the SPA can track it for reconnect backfill; the Sync
    // feed reads it back off the payload (sync-feed.ts).
    const stamped = event as SessionEvent & { _event_id?: number };
    if (insertedId !== undefined) stamped._event_id = insertedId;
    sessionBus.publish(event);
    for (const id of cascadeOrphans) {
      workspaceBus.publish({ kind: "deleted", id: asWorkspaceId(id) });
    }
  }

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
    }).catch((e) => {
      // Fire-and-forget must never surface as an unhandled rejection; the
      // snapshot reconcile re-offers these orphans on the next snapshot.
      log.warn("event-log", "reap_orphan_dispatch_failed", { error: String(e) });
    });
  }

}
