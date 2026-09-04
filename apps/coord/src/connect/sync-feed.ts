// The sync firehose merges durable, retained, and live sources only after each
// frame is scoped to its persisted dashboard. That boundary prevents one socket
// queue or browser-facing cache from retaining another tenant's state.

import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema, type FirehoseFrame, SessionPresenceSchema,
  WorkerRoutableFrameSchema, TerminalTitleFrameSchema, LastActivityFrameSchema,
  UiStateFrameSchema, UiCommandFrameSchema, SyncDomain,
} from "@roost/shared/proto/sync_pb";
import {
  sessionBus, presenceBus, workspaceBus, taskBus, mcpBus, globalPresenceBus,
  auditBus, titleBus, lastActivityBus, workerRoutableBus, agentStatusBus,
  pairBus, uiBus,
} from "../buses.ts";
import { getEventMaxId, getEventsSince, getEventsThrough } from "../event-log.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import type { SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "../db/connection.ts";
import type { ConnectDeps } from "./router.ts";
import { getUiStateSnapshot } from "./handlers-ui.ts";
import {
  agentStatusFrame, auditFrame, frameMeta, mcpFrame, pairFrame, presenceFrame,
  sessionFirehoseFrame, sessionMeta, taskFrame, workspaceFrame,
  type SyncFeedFrameMeta,
} from "./sync-feed-frames.ts";
import {
  retainedSeedFrames, seedDomain,
  type SyncDashboardScope, type SyncFeedSeedContext,
} from "./sync-feed-seed.ts";
import {
  createSyncFeedV1SeedDelivery,
  type SyncFeedSeedOptions,
} from "./sync-feed-v1-seed.ts";
import {
  APPLICATION_MAX_UNACKED_BYTES,
  APPLICATION_MAX_UNACKED_FRAMES,
} from "./sync-ws-v1-delivery.ts";

export type { SyncFeedFrameMeta, SyncFeedLane } from "./sync-feed-frames.ts";
export type { SyncDashboardScope } from "./sync-feed-seed.ts";
export type { SyncFeedSeedOptions } from "./sync-feed-v1-seed.ts";

/** Load persisted runtime ownership after DashboardActor has accepted the
 * selected dashboard and before the socket is upgraded. */
export async function loadSyncDashboardScope(
  db: KyselyDB,
  dashboardId: string,
): Promise<SyncDashboardScope> {
  const [workers, sessions, workspaces] = await Promise.all([
    db.selectFrom("workers").select("fp")
      .where("dashboard_id", "=", dashboardId)
      .where("deleted_at_ms", "is", null)
      .execute(),
    db.selectFrom("sessions").select("id").where("dashboard_id", "=", dashboardId).execute(),
    db.selectFrom("workspaces").select("id").where("dashboard_id", "=", dashboardId).execute(),
  ]);
  return {
    dashboardId,
    workerFps: new Set(workers.map((row) => row.fp)),
    sessionIds: new Set(sessions.map((row) => row.id)),
    workspaceIds: new Set(workspaces.map((row) => row.id)),
  };
}

export interface SyncFeedV2Options {
  readonly version: 2;
  onRecoveryReset: (reason: string) => void;
}

export interface SyncFeed {
  readonly seeded: Promise<void>;
  backfill(): Promise<void>;
  seedDomain(domain: SyncDomain, sessionIds?: ReadonlySet<string>): Promise<void>;
  dispose(): void;
}

export function startSyncFeed(
  deps: ConnectDeps,
  scope: SyncDashboardScope,
  sinceEventId: number,
  sink: (frame: FirehoseFrame, meta?: SyncFeedFrameMeta) => void,
  viewerKey: string | null,
  seedOptions?: SyncFeedSeedOptions | SyncFeedV2Options,
): SyncFeed {
  const v2Options = seedOptions?.version === 2 ? seedOptions : null;
  const legacySeedOptions = seedOptions && seedOptions.version !== 2 ? seedOptions : null;
  let disposed = false;
  const v1SeedDelivery = legacySeedOptions
    ? createSyncFeedV1SeedDelivery(
      legacySeedOptions,
      retainedSeedFrames(scope),
      sink,
      APPLICATION_MAX_UNACKED_FRAMES,
      APPLICATION_MAX_UNACKED_BYTES,
    )
    : null;
  const push = (frame: FirehoseFrame, meta = frameMeta(frame)): void => {
    if (disposed) return;
    if (v1SeedDelivery) return v1SeedDelivery.push(frame, meta);
    sink(frame, meta);
  };
  // Recovery remembers only its live boundary; the scalar cutoff rejects old
  // repeats while later live IDs never grow the set.
  let replayedSessionCutoff = Math.max(0, sinceEventId);
  let collectingRecoveryBoundary = v2Options === null && sinceEventId > 0;
  const recoveryBoundaryEventIds = new Set<number>();
  const pendingRecoveryEvents = new Map<number, SessionEvent>();
  let recoveringSessions = v2Options !== null && sinceEventId > 0;
  let recoveryAborted = false;
  let pendingRecoveryBytes = 0;
  const emitSessionFrame = (event: SessionEvent, eventId: number): void => {
    push(sessionFirehoseFrame(event, eventId), sessionMeta(event));
  };
  const emitLiveSessionNow = (event: SessionEvent, eventId: number): void => {
    if (eventId > 0) {
      if (eventId <= replayedSessionCutoff || recoveryBoundaryEventIds.has(eventId)) return;
      if (collectingRecoveryBoundary) recoveryBoundaryEventIds.add(eventId);
    }
    emitSessionFrame(event, eventId);
  };
  const emitRecoveredSession = (event: SessionEvent, eventId: number): void => {
    if (eventId <= replayedSessionCutoff) return;
    const alreadyYieldedLive = recoveryBoundaryEventIds.delete(eventId);
    replayedSessionCutoff = eventId;
    if (!alreadyYieldedLive) emitSessionFrame(event, eventId);
  };
  const emitSession = (event: SessionEvent, eventId: number): void => {
    if (!recoveringSessions) return emitLiveSessionNow(event, eventId);
    if (eventId <= 0) {
      recoveryAborted = true;
      recoveringSessions = false;
      pendingRecoveryEvents.clear();
      pendingRecoveryBytes = 0;
      v2Options?.onRecoveryReset("unstamped_session_event");
      return;
    }
    const estimatedBytes = JSON.stringify(event).length;
    const previous = pendingRecoveryEvents.get(eventId);
    const nextBytes = pendingRecoveryBytes
      - (previous ? JSON.stringify(previous).length : 0)
      + estimatedBytes;
    if (
      (!previous && pendingRecoveryEvents.size >= 512)
      || nextBytes > 4 * 1024 * 1024
    ) {
      recoveryAborted = true;
      recoveringSessions = false;
      pendingRecoveryEvents.clear();
      pendingRecoveryBytes = 0;
      v2Options?.onRecoveryReset("recovery_live_overflow");
      emitLiveSessionNow(event, eventId);
      return;
    }
    pendingRecoveryEvents.set(eventId, event);
    pendingRecoveryBytes = nextBytes;
  };

  const unsubs = [
    sessionBus.subscribe((event) => {
      if (event._dashboard_id !== scope.dashboardId) return;
      if (event.kind === "snapshot") {
        for (const session of event.sessions) scope.sessionIds.add(session.id);
      } else if (event.kind === "opened") {
        scope.sessionIds.add(event.session_id);
      } else if (event.kind === "closed") {
        scope.sessionIds.delete(event.session_id);
      }
      emitSession(event, event._event_id ?? 0);
    }, scope.dashboardId),
    presenceBus.subscribe((event) => {
      if (event._dashboard_id !== scope.dashboardId) return;
      if (event.kind === "registered") scope.workerFps.add(event.worker.fp);
      else if (event.kind === "removed") scope.workerFps.delete(event.fp);
      const frame = presenceFrame(event);
      if (frame) push(frame);
    }, scope.dashboardId),
    workspaceBus.subscribe((event) => {
      if (event._dashboard_id !== scope.dashboardId) return;
      if (event.kind === "deleted") scope.workspaceIds.delete(event.id);
      else if (event.kind === "created" || event.kind === "updated") {
        scope.workspaceIds.add(event.workspace.id);
      }
      const frame = workspaceFrame(event);
      if (frame) push(frame);
    }, scope.dashboardId),
    taskBus.subscribe((event) => {
      if (event._dashboard_id === scope.dashboardId) push(taskFrame(event));
    }, scope.dashboardId),
    mcpBus.subscribe((event) => {
      if (event._dashboard_id !== scope.dashboardId) return;
      const frame = mcpFrame(event);
      if (frame) push(frame);
    }, scope.dashboardId),
    auditBus.subscribe((event) => {
      // Audit events must be stamped by the authenticated producer. This check
      // happens before auditFrame/push so a foreign row never reaches a socket.
      if (event._dashboard_id === scope.dashboardId) push(auditFrame(event));
    }, scope.dashboardId),
    pairBus.subscribe((event) => {
      if (event._dashboard_id === scope.dashboardId) push(pairFrame(event));
    }, scope.dashboardId),
    globalPresenceBus.subscribe(({ session_id, data }) => {
      if (!scope.sessionIds.has(session_id)) return;
      if (viewerKey !== null && typeof data === "object" && data !== null) {
        const payload = data as { kind?: unknown; viewer_id?: unknown };
        if (
          (payload.kind === "presence-delta" || payload.kind === "presence-leave")
          && payload.viewer_id === viewerKey
        ) return;
      }
      push(create(FirehoseFrameSchema, {
        frame: { case: "sessionPresence", value: create(SessionPresenceSchema, {
          sessionId: session_id, payloadJson: JSON.stringify(data),
        }) },
      }));
    }),
    titleBus.subscribe(({ session_id, title }) => {
      if (!scope.sessionIds.has(session_id)) return;
      push(create(FirehoseFrameSchema, {
        frame: { case: "terminalTitle", value: create(TerminalTitleFrameSchema, {
          sessionId: session_id, title,
        }) },
      }));
    }),
    lastActivityBus.subscribe(({ session_id, ts_ms }) => {
      if (!scope.sessionIds.has(session_id)) return;
      push(create(FirehoseFrameSchema, {
        frame: { case: "lastActivity", value: create(LastActivityFrameSchema, {
          sessionId: session_id, tsMs: ts_ms,
        }) },
      }));
    }),
    workerRoutableBus.subscribe(({ fps }) => {
      const scopedFps = fps.filter((fp) => scope.workerFps.has(fp));
      push(create(FirehoseFrameSchema, {
        frame: { case: "workerRoutable", value: create(WorkerRoutableFrameSchema, {
          fps: scopedFps,
        }) },
      }));
    }),
    agentStatusBus.subscribe((status) => {
      if (scope.sessionIds.has(status.session_id)) push(agentStatusFrame(status));
    }),
    uiBus.subscribe((message) => {
      if (message._dashboard_id !== scope.dashboardId) return;
      push(message.kind === "state"
        ? create(FirehoseFrameSchema, {
          frame: { case: "uiState", value: create(UiStateFrameSchema, {
            fp: message.fp, tabId: message.tabId, state: message.state,
          }) },
        })
        : create(FirehoseFrameSchema, {
          frame: { case: "uiCommand", value: create(UiCommandFrameSchema, {
            targetTabId: message.targetTabId, command: message.command,
          }) },
        }));
    }, scope.dashboardId),
  ];

  let seeded: Promise<void>;
  if (!seedOptions) {
    for (const frame of retainedSeedFrames(scope)) push(frame);
    seeded = Promise.resolve();
  } else if (v1SeedDelivery) {
    seeded = v1SeedDelivery.seeded;
  } else {
    seeded = Promise.resolve();
    queueMicrotask(() => {
      if (disposed) return;
      for (const { fp, tabId, state } of getUiStateSnapshot(scope.dashboardId)) {
        push(create(FirehoseFrameSchema, {
          frame: { case: "uiState", value: create(UiStateFrameSchema, { fp, tabId, state }) },
        }), { domain: null, lane: "control" });
      }
    });
  }

  const backfill = async (): Promise<void> => {
    await seeded;
    if (disposed || sinceEventId <= 0) return;
    if (v2Options) {
      try {
        const cutoff = await getEventMaxId(deps.db, scope.dashboardId);
        if (recoveryAborted) return;
        if (cutoff < sinceEventId) {
          recoveringSessions = false;
          pendingRecoveryEvents.clear(); pendingRecoveryBytes = 0;
          v2Options.onRecoveryReset("cursor_ahead_of_log");
          return;
        }
        let cursor = sinceEventId;
        while (!disposed && !recoveryAborted && cursor < cutoff) {
          const rows = await getEventsThrough(deps.db, scope.dashboardId, cursor, cutoff);
          if (rows.length === 0) {
            recoveringSessions = false;
            pendingRecoveryEvents.clear(); pendingRecoveryBytes = 0;
            v2Options.onRecoveryReset("recovery_gap");
            return;
          }
          for (const { id, event } of rows) {
            if (recoveryAborted) return;
            if (id <= cursor || id > cutoff) {
              recoveringSessions = false;
              pendingRecoveryEvents.clear(); pendingRecoveryBytes = 0;
              v2Options.onRecoveryReset("recovery_order");
              return;
            }
            emitRecoveredSession(event, id);
            cursor = id;
          }
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
        if (disposed || recoveryAborted) return;
        replayedSessionCutoff = cutoff;
        const liveTail = [...pendingRecoveryEvents.entries()]
          .filter(([id]) => id > cutoff)
          .sort(([left], [right]) => left - right);
        pendingRecoveryEvents.clear();
        pendingRecoveryBytes = 0;
        for (const [id, event] of liveTail) {
          recoveryBoundaryEventIds.add(id);
          emitSessionFrame(event, id);
        }
        recoveringSessions = false;
      } catch (error) {
        recoveringSessions = false;
        pendingRecoveryEvents.clear();
        pendingRecoveryBytes = 0;
        log.warn("connect.sync", "backfill_failed", { error: String(error), sinceEventId });
        signal("sync.backfill_failed", { error: String(error), sinceEventId, cooldownKey: "sync" });
        v2Options.onRecoveryReset("recovery_failed");
      }
      return;
    }
    try {
      const rows = await getEventsSince(deps.db, scope.dashboardId, sinceEventId, 1000);
      for (let index = 0; index < rows.length; index += 1) {
        const { id, event } = rows[index]!;
        emitRecoveredSession(event, id);
        if ((index + 1) % 16 === 0) {
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
      }
      if (rows.length === 1000) {
        signal("sync.backfill_truncated", { sinceEventId, returned: rows.length, cooldownKey: "sync" });
      }
    } catch (error) {
      log.warn("connect.sync", "backfill_failed", { error: String(error), sinceEventId });
      signal("sync.backfill_failed", { error: String(error), sinceEventId, cooldownKey: "sync" });
    }
    collectingRecoveryBoundary = false;
  };

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    v1SeedDelivery?.dispose();
    recoveringSessions = false;
    collectingRecoveryBoundary = false;
    recoveryBoundaryEventIds.clear();
    pendingRecoveryEvents.clear();
    pendingRecoveryBytes = 0;
    for (const unsubscribe of unsubs) unsubscribe();
  };
  const seedCtx: SyncFeedSeedContext = {
    v2: v2Options !== null,
    isDisposed: () => disposed,
    push,
  };
  return {
    seeded,
    backfill,
    seedDomain: (domain, sessionIds) => seedDomain(seedCtx, scope, domain, sessionIds),
    dispose,
  };
}
