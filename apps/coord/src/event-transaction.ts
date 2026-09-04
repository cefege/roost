// This module owns durable event append transactions and post-commit publication.
// Session and worker handlers reach it through the stable event-log facade.
// It depends on Kysely, projection helpers, and bounded publication recovery.
// Duplicate delivery and route-before-bus ordering are invariants.

import type { KyselyDB } from "./db/connection.ts";
import { foldEvent } from "@roost/shared/wire";
import type { Session, SessionEvent, WorkerFp } from "@roost/shared/wire";
import {
  MAX_WORKER_SNAPSHOT_SESSIONS,
  normalizePersistedWorkerEvent,
} from "./persistence-input.ts";
import { log } from "@roost/shared/log";
import { resolveEventAdmission } from "./event-admission.ts";
import {
  _cascadeClosedSession,
  loadSession,
  projectSnapshotSessions,
  sessionToRow,
} from "./event-projection.ts";
import {
  reservePendingEventPublication,
  resolveEventPublication,
  type CommittedEventPublication,
  type PendingEventPublicationStore,
} from "./pending-event-publications.ts";

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
export interface AppendEventResult {
  /** False when persisted worker/resource scope could not be proven. */
  admitted: boolean;
  /** True only when this call inserted the durable event row. */
  inserted: boolean;
  /** True only after the committed event updated volatile routes and Sync. */
  published: boolean;
  /** Persisted dashboard selected for the append, when admission resolved one. */
  dashboardId: string | null;
  /** A dedupe payload differed from the retained unpublished committed event. */
  replayRejected: boolean;
  /** The normalized event actually persisted and published. */
  event: SessionEvent;
  /** Force-closed snapshot members omitted from `event` and awaiting reap. */
  snapshotReapIds: readonly string[];
}

export async function appendEvent(
  db: KyselyDB,
  event: SessionEvent,
  opts: {
    /** Authenticated worker fingerprint, never a worker-frame claim. */
    worker_fp: string | null;
    client_seq: number | null;
    /** Dashboard actor scope for coordinator-originated session mutations. */
    dashboardId?: string;
    /** Managed workers may create only a coordinator-reserved session id. */
    allowNewWorkerSession?: (
      dashboardId: string,
      workerFp: string,
      sessionId: string,
    ) => boolean;
    /** Managed snapshots may only re-announce persisted own sessions. */
    requireExistingWorkerSessions?: boolean;
    /** Auxiliary writes that MUST commit atomically with the event (e.g.
     *  junction-table moves). Runs inside the transaction, AFTER ownership
     *  admission, so a foreign request cannot make an auxiliary mutation. */
    extraWork?: (trx: KyselyDB) => Promise<void>;
    /** Post-commit generation/revocation fence for worker-originated events.
     * False keeps the durable row/projection but suppresses every live effect. */
    canPublish?: () => boolean;
    /** Bounded process owner for publication lost to a socket-generation race. */
    pendingPublications?: PendingEventPublicationStore;
    /** Worker connections defer orphan reaps until the snapshot ACK barrier has
     * made the exact current handle ready. Direct coordinator callers do not. */
    deferSnapshotReap?: boolean;
  } = { worker_fp: null, client_seq: null },
): Promise<AppendEventResult> {
  if (
    event.kind === "snapshot"
    && event.sessions.length > MAX_WORKER_SNAPSHOT_SESSIONS
  ) {
    throw new Error(
      `worker snapshot exceeds ${MAX_WORKER_SNAPSHOT_SESSIONS} sessions`,
    );
  }
  // Use this one normalized value for durable JSON, projection folding,
  // channel-index publication, and live Sync publication. Normalizing a row
  // later would make replay disagree with the sessions projection.
  event = normalizePersistedWorkerEvent(event);
  const publicationStore = opts.pendingPublications;
  const publicationReservation = await reservePendingEventPublication(
    publicationStore,
    opts.worker_fp,
    opts.client_seq,
  );
  let insertedId: number | undefined;
  let persistedEventJson = "";
  let dashboardId: string | null = null;
  // Snapshot reap: session ids force-closed while their worker was offline but
  // re-announced live by the returning worker. Sent a kill AFTER commit.
  let reapOrphanIds: string[] = [];
  // Publication decision, taken inside the transaction: a dedupe hit or a
  // fold that produced no row must not reach the channel index or the bus.
  let publishable = false;
  // Workspaces orphaned by a `closed` cascade; their deltas publish after commit.
  let cascadeOrphans: string[] = [];
  let admissionRejected = false;
  let sessionExisted = false;
  await db.transaction().execute(async (trx) => {
    const tx = trx as unknown as KyselyDB;
    const admission = await resolveEventAdmission(tx, event, opts);
    dashboardId = admission.dashboardId;
    if (!admission.admitted || admission.dashboardId === null) {
      admissionRejected = true;
      return;
    }
    const sessionId = admission.sessionId;
    const resolvedDashboardId = admission.dashboardId;
    sessionExisted = admission.sessionExists;
    if (event.kind === "workspace_assigned" && event.workspace_id !== null) {
      const workspace = await tx.selectFrom("workspaces")
        .select("id")
        .where("id", "=", event.workspace_id)
        .where("dashboard_id", "=", resolvedDashboardId)
        .executeTakeFirst();
      if (!workspace) {
        throw new Error("workspace is unavailable in this dashboard");
      }
    }
    if (event.kind === "snapshot") {
      // A prior durable `closed` is a permanent force-close tombstone. Compute
      // the effective snapshot before serializing the event so the log,
      // projection, route index, and Sync publication all see the same set.
      const announcedIds = event.sessions.map((session) => session.id);
      const tombstoned = announcedIds.length === 0
        ? []
        : await tx.selectFrom("events").select("session_id")
          .where("dashboard_id", "=", resolvedDashboardId)
          .where("kind", "=", "closed")
          .where("session_id", "in", announcedIds)
          .execute();
      const tombstonedIds = new Set(tombstoned.map((row) => row.session_id as string));
      if (tombstonedIds.size > 0) {
        reapOrphanIds = [...tombstonedIds];
        event = {
          ...event,
          sessions: event.sessions.filter((session) => !tombstonedIds.has(session.id)),
        };
      }
    }
    if (opts.extraWork) await opts.extraWork(tx);
    persistedEventJson = JSON.stringify(event);

    // 1. Append to event log. If (worker_fp, client_seq) is present and
    //    already exists, the partial unique index makes this a conflict
    //    that we silently swallow via onConflict-doNothing.
    const inserted = await tx
      .insertInto("events")
      .values({
        dashboard_id: resolvedDashboardId,
        kind: event.kind,
        session_id: sessionId,
        worker_fp: opts.worker_fp,
        payload_json: persistedEventJson,
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
      log.debug("event-log", "dedup_hit", {
        worker_fp: opts.worker_fp,
        client_seq: opts.client_seq,
      });
      return; // already applied — skip projection + bus publish
    }

    // 2. Handle snapshot: reconcile ghost sessions + project new set.
    if (event.kind === "snapshot") {
      await projectSnapshotSessions(tx, event, resolvedDashboardId);
      publishable = true;
      return;
    }

    // 3. Non-snapshot: fold event into projection.
    if (event.kind === "opened") {
      const prev = new Map<string, Session>();
      const next = foldEvent(prev, event);
      const session = next.get(event.session_id);
      if (!session) return;
      const insertedSession = await tx
        .insertInto("sessions")
        .values(sessionToRow(session, resolvedDashboardId))
        .onConflict((oc) => oc.column("id").doNothing())
        .returning("id")
        .executeTakeFirst();
      if (!sessionExisted && !insertedSession) {
        await tx.deleteFrom("events")
          .where("id", "=", insertedId!)
          .execute();
        insertedId = undefined;
        admissionRejected = true;
        return;
      }
    } else if (event.kind === "closed") {
      // Terminal exited (or post-undo kill fired) → DELETE the row, don't
      // park it as status="closed". Capture and remove workspace ownership
      // first because the session FK cascade would erase that evidence.
      // No-op if it's already gone (idempotent on dedup / double-close).
      cascadeOrphans = await _cascadeClosedSession(tx, resolvedDashboardId, event.session_id);
      await tx
        .deleteFrom("sessions")
        .where("id", "=", event.session_id)
        .where("dashboard_id", "=", resolvedDashboardId)
        .execute();
    } else if (sessionId !== null) {
      const existing = await loadSession(tx, sessionId, resolvedDashboardId);
      if (!existing) {
        log.warn("event-log", "session_not_found", { kind: event.kind, session_id: sessionId });
        return;
      }
      const prev = new Map([[existing.id, existing]]);
      const next = foldEvent(prev, event);
      const updated = next.get(sessionId);
      if (!updated) return;

      await tx
        .updateTable("sessions")
        .set(sessionToRow(updated, resolvedDashboardId))
        .where("id", "=", sessionId)
        .where("dashboard_id", "=", resolvedDashboardId)
        .execute();

      // Also update workspace_sessions junction on workspace_assigned.
      if (event.kind === "workspace_assigned") {
        await tx
          .deleteFrom("workspace_sessions")
          .where("session_id", "=", sessionId)
          .where("dashboard_id", "=", resolvedDashboardId)
          .execute();
        if (event.workspace_id) {
          await tx
            .insertInto("workspace_sessions")
            .values({
              dashboard_id: resolvedDashboardId,
              workspace_id: event.workspace_id,
              session_id: sessionId,
              added_at_ms: Date.now(),
            })
            .onConflict((oc) => oc.doNothing())
            .execute();
        }
      }
    }

    publishable = true;
  }).catch((error) => {
    if (publicationReservation) {
      publicationStore?.release(publicationReservation);
    }
    throw error;
  });

  const committedEffect: CommittedEventPublication | undefined =
    publishable && dashboardId !== null && insertedId !== undefined
      ? {
          event,
          authenticatedWorkerFp: opts.worker_fp as WorkerFp | null,
          dashboardId,
          eventId: insertedId,
          eventJson: persistedEventJson,
          cascadeOrphanIds: [...cascadeOrphans],
          snapshotReapIds: [...reapOrphanIds],
        }
      : undefined;
  const publication = await resolveEventPublication({
    store: publicationStore,
    reservation: publicationReservation,
    committedEffect,
    deduplicated: !admissionRejected && insertedId === undefined,
    canPublish: opts.canPublish,
    replayEventJson: persistedEventJson,
  });
  const publishedEffect = publication.publishedEffect;

  const published = publishedEffect !== undefined;
  const publishedEvent = publishedEffect?.event ?? event;
  const snapshotReapIds = publishedEffect?.snapshotReapIds ?? reapOrphanIds;
  if (
    published
    && !opts.deferSnapshotReap
    && snapshotReapIds.length > 0
    && publishedEvent.kind === "snapshot"
  ) {
    dispatchSnapshotOrphanReaps(publishedEvent.worker_fp, snapshotReapIds);
  }

  return {
    admitted: !admissionRejected,
    inserted: insertedId !== undefined,
    published,
    replayRejected: publication.replayRejected,
    dashboardId,
    event: publishedEvent,
    snapshotReapIds,
  };
}

/** Reap force-closed PTYs only after the caller's readiness barrier permits
 * coordinator commands. The durable effective snapshot has already omitted
 * these ids, so a failed best-effort kill cannot resurrect a route. */
export function dispatchSnapshotOrphanReaps(
  workerFp: WorkerFp,
  ids: readonly string[],
): void {
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
  }).catch((error) => {
    // Fire-and-forget must never surface as an unhandled rejection; the
    // snapshot reconcile re-offers these orphans on the next snapshot.
    log.warn("event-log", "reap_orphan_dispatch_failed", { error: String(error) });
  });
}
