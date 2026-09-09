// The sync firehose merges durable, retained, and live sources into one ordered
// stream per socket: durable session events keep a monotonic replay cutoff,
// retained snapshots seed volatile state, and live bus fan-out follows them.
// Session-keyed frames are gated by the socket's resource index, which the
// upgrade seeds and durable events keep current; sync-feed-seed.ts owns the
// retained half.

import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema, type FirehoseFrame, SessionPresenceSchema,
  WorkerRoutableFrameSchema, TerminalTitleFrameSchema, LastActivityFrameSchema,
  SyncDomain,
} from "@roost/shared/proto/sync_pb";
import {
  sessionBus, presenceBus, workspaceBus, taskBus, mcpBus, globalPresenceBus,
  auditBus, titleBus, lastActivityBus, workerRoutableBus, agentStatusBus,
  pairBus,
} from "../buses.ts";
import { getEventMaxId, getEventsSince, getEventsThrough } from "../event-log.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import type { SessionEvent } from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";
import {
  agentStatusFrame, auditFrame, frameMeta, mcpFrame, pairFrame, presenceFrame,
  sessionFirehoseFrame, sessionMeta, taskFrame, workspaceFrame,
  type SyncFeedFrameMeta,
} from "./sync-feed-frames.ts";
import {
  retainedSeedFrames, seedDomain,
  type SyncFeedSeedContext, type SyncResourceIndex,
} from "./sync-feed-seed.ts";
import {
  createSyncFeedV1SeedDelivery,
  type SyncFeedSeedOptions,
} from "./sync-feed-v1-seed.ts";
import { subscribeUiFeed, uiStateSeedFrames } from "./sync-feed-ui.ts";
import {
  APPLICATION_MAX_UNACKED_BYTES,
  APPLICATION_MAX_UNACKED_FRAMES,
} from "./sync-ws-v1-delivery.ts";

export type { SyncFeedFrameMeta, SyncFeedLane } from "./sync-feed-frames.ts";
export {
  loadSyncResourceIndex,
  type SyncResourceIndex,
} from "./sync-feed-seed.ts";
export type { SyncFeedSeedOptions } from "./sync-feed-v1-seed.ts";

export interface SyncFeedV2Options {
  readonly version: 2;
  readonly socketId: string;
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
  scope: SyncResourceIndex,
  sinceEventId: number,
  sink: (frame: FirehoseFrame, meta?: SyncFeedFrameMeta) => void,
  viewerKey: string | null,
  browserUi: boolean,
  seedOptions?: SyncFeedSeedOptions | SyncFeedV2Options,
): SyncFeed {
  const v2Options = seedOptions?.version === 2 ? seedOptions : null;
  const legacySeedOptions = seedOptions && seedOptions.version !== 2 ? seedOptions : null;
  let disposed = false;
  const v1SeedDelivery = legacySeedOptions
    ? createSyncFeedV1SeedDelivery(
      legacySeedOptions,
      retainedSeedFrames(scope, deps.uiStates, browserUi),
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
  // A worker socket carries only its own sessions. Ownership is recorded from
  // `opened`/`snapshot` and retained after close, so a deferred or recovered
  // `closed` for an owned session still reaches the socket.
  const ownedSessionIds = scope.ownerWorkerFp === null
    ? null
    : new Set(scope.sessionIds);
  const admitOwnedSessionEvent = (event: SessionEvent): boolean => {
    if (ownedSessionIds === null) return true;
    if (event.kind === "snapshot") {
      if (event.worker_fp !== scope.ownerWorkerFp) return false;
      for (const session of event.sessions) ownedSessionIds.add(session.id);
      return true;
    }
    if (event.kind === "opened") {
      if (event.worker_fp !== scope.ownerWorkerFp) return false;
      ownedSessionIds.add(event.session_id);
      return true;
    }
    return ownedSessionIds.has(event.session_id);
  };
  const emitSessionFrame = (event: SessionEvent, eventId: number): void => {
    if (!admitOwnedSessionEvent(event)) return;
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

  const ownsWorker = (workerFp: string): boolean =>
    scope.ownerWorkerFp === null || scope.ownerWorkerFp === workerFp;
  const installWideViewer = scope.ownerWorkerFp === null;
  const unsubs = [
    sessionBus.subscribe((event) => {
      if (event.kind === "snapshot") {
        if (ownsWorker(event.worker_fp)) {
          for (const session of event.sessions) scope.sessionIds.add(session.id);
        }
      } else if (event.kind === "opened") {
        if (ownsWorker(event.worker_fp)) scope.sessionIds.add(event.session_id);
      } else if (event.kind === "closed") {
        scope.sessionIds.delete(event.session_id);
      }
      emitSession(event, event._event_id ?? 0);
    }),
    presenceBus.subscribe((event) => {
      const workerFp = event.kind === "registered" ? event.worker.fp : event.fp;
      if (!ownsWorker(workerFp)) return;
      if (event.kind === "registered") scope.workerFps.add(workerFp);
      else if (event.kind === "removed") scope.workerFps.delete(workerFp);
      const frame = presenceFrame(event);
      if (frame) push(frame);
    }),
    workspaceBus.subscribe((event) => {
      const owned = event.kind === "created" || event.kind === "updated"
        ? ownsWorker(event.workspace.worker_fp)
        : scope.ownerWorkerFp === null || scope.workspaceIds.has(event.id);
      if (event.kind === "deleted") scope.workspaceIds.delete(event.id);
      else if (owned && event.kind !== "sessions-set") {
        scope.workspaceIds.add(event.workspace.id);
      }
      if (!owned) return;
      const frame = workspaceFrame(event);
      if (frame) push(frame);
    }),
    // Tasks, MCP relays, audit rows and pair requests are install-wide with no
    // worker owner, so a read-only worker socket is not one of their viewers.
    taskBus.subscribe((event) => {
      if (installWideViewer) push(taskFrame(event));
    }),
    mcpBus.subscribe((event) => {
      if (!installWideViewer) return;
      const frame = mcpFrame(event);
      if (frame) push(frame);
    }),
    auditBus.subscribe((event) => {
      if (installWideViewer) push(auditFrame(event));
    }),
    pairBus.subscribe((event) => {
      if (installWideViewer) push(pairFrame(event));
    }),
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
      push(create(FirehoseFrameSchema, {
        frame: { case: "workerRoutable", value: create(WorkerRoutableFrameSchema, {
          fps: fps.filter((fp) => scope.workerFps.has(fp)),
        }) },
      }));
    }),
    agentStatusBus.subscribe((status) => {
      if (scope.sessionIds.has(status.session_id)) push(agentStatusFrame(status));
    }),
    subscribeUiFeed({
      browserUi,
      targetSocketId: v2Options?.socketId ?? null,
      push,
    }),
  ];

  let seeded: Promise<void>;
  if (!seedOptions) {
    for (const frame of retainedSeedFrames(scope, deps.uiStates, browserUi)) push(frame);
    seeded = Promise.resolve();
  } else if (v1SeedDelivery) {
    seeded = v1SeedDelivery.seeded;
  } else {
    seeded = Promise.resolve();
    queueMicrotask(() => {
      if (disposed) return;
      if (!browserUi) return;
      for (const frame of uiStateSeedFrames(deps.uiStates)) {
        push(frame, { domain: null, lane: "control" });
      }
    });
  }

  const backfill = async (): Promise<void> => {
    await seeded;
    if (disposed || sinceEventId <= 0) return;
    if (v2Options) {
      try {
        const cutoff = await getEventMaxId(deps.db);
        if (recoveryAborted) return;
        if (cutoff < sinceEventId) {
          recoveringSessions = false;
          pendingRecoveryEvents.clear(); pendingRecoveryBytes = 0;
          v2Options.onRecoveryReset("cursor_ahead_of_log");
          return;
        }
        let cursor = sinceEventId;
        while (!disposed && !recoveryAborted && cursor < cutoff) {
          const rows = await getEventsThrough(deps.db, cursor, cutoff);
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
      const rows = await getEventsSince(deps.db, sinceEventId, 1000);
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
