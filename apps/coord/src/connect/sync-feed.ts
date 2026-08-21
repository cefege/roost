// The Sync firehose feed: subscribes every in-memory state bus for one socket,
// pages durable session events on reconnect, and hands each item to the socket
// scheduler. Split out of handlers-streaming.ts, which keeps only the retired
// Connect RPC stub. Frame adapters live in sync-feed-frames.ts and retained
// seeding in sync-feed-seed.ts.

import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema, type FirehoseFrame, SessionPresenceSchema,
  WorkerRoutableFrameSchema, TerminalTitleFrameSchema, LastActivityFrameSchema,
  UiStateFrameSchema, UiCommandFrameSchema, SyncDomain,
} from "@roost/shared/proto/sync_pb";
import {
  sessionBus, presenceBus, workspaceBus, taskBus, webhookBus,
  permissionBus, mcpBus, globalPresenceBus, auditBus,
  titleBus, lastActivityBus, workerRoutableBus, agentStatusBus,
  pairBus, uiBus,
} from "../buses.ts";
import { getEventMaxId, getEventsSince, getEventsThrough } from "../event-log.ts";
import { getUiStateSnapshot } from "./handlers-ui.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import type { SessionEvent } from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";
import {
  agentStatusFrame, auditFrame, frameMeta, mcpFrame, pairFrame, permFrame,
  presenceFrame, sessionFirehoseFrame, sessionMeta, taskFrame, webhookFrame,
  workspaceFrame, type SyncFeedFrameMeta,
} from "./sync-feed-frames.ts";
import {
  loadPairSnapshot, retainedSeedFrames, seedDomain,
  type SyncFeedSeedContext,
} from "./sync-feed-seed.ts";

export type { SyncFeedFrameMeta, SyncFeedLane } from "./sync-feed-frames.ts";

// startSyncFeed is shared by cached flow=1 clients and Sync v2. Legacy callers
// retain their synchronous/ACK-paced seed behavior. V2 subscribes every live
// bus synchronously, emits no application seed before readiness, and tags each
// application item for the socket scheduler.
export interface SyncFeedSeedOptions {
  readonly version?: 1;
  pacedSeedPush: (frame: FirehoseFrame) => Promise<boolean>;
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
  sinceEventId: number,
  sink: (frame: FirehoseFrame, meta?: SyncFeedFrameMeta) => void,
  /** Per-tab identity of the socket this feed serves. Non-null → the two hot
   *  per-session buses ship only sessions this tab claimed. null (older SPA,
   *  CLI, test client) FAILS OPEN and ships every session, as before. */
  viewerKey: string | null = null,
  seedOptions?: SyncFeedSeedOptions | SyncFeedV2Options,
): SyncFeed {
  const v2Options = seedOptions?.version === 2 ? seedOptions : null;
  const legacySeedOptions = seedOptions && seedOptions.version !== 2 ? seedOptions : null;
  let disposed = false;
  let seeding = legacySeedOptions !== null;
  const queuedLiveFrames: Array<{ frame: FirehoseFrame; meta: SyncFeedFrameMeta }> = [];
  const push = (frame: FirehoseFrame, meta = frameMeta(frame)): void => {
    if (disposed) return;
    if (seeding) {
      queuedLiveFrames.push({ frame, meta });
      return;
    }
    sink(frame, meta);
  };

  // Only terminal/session events are durable. V2 takes a stable DB cutoff
  // after subscribing, pages the complete interval, then drains bus events
  // above that cutoff in numeric id order. Every other domain is refreshed
  // from its authoritative list for each domain generation.
  const yieldedSessionIds = new Set<number>();
  const pendingRecoveryEvents = new Map<number, SessionEvent>();
  let recoveringSessions = v2Options !== null && sinceEventId > 0;
  let recoveryAborted = false;
  let pendingRecoveryBytes = 0;
  const emitSessionNow = (event: SessionEvent, eventId: number): void => {
    if (eventId > 0) {
      if (yieldedSessionIds.has(eventId)) return;
      yieldedSessionIds.add(eventId);
    }
    push(sessionFirehoseFrame(event, eventId), sessionMeta(event));
  };
  const emitSession = (event: SessionEvent, eventId: number): void => {
    if (recoveringSessions) {
      if (eventId <= 0) {
        recoveryAborted = true;
        recoveringSessions = false;
        pendingRecoveryEvents.clear();
        pendingRecoveryBytes = 0;
        v2Options?.onRecoveryReset("unstamped_session_event");
        return;
      }
      const estimatedBytes = JSON.stringify(event).length;
      if (
        pendingRecoveryEvents.size >= 512
        || pendingRecoveryBytes + estimatedBytes > 4 * 1024 * 1024
      ) {
        recoveryAborted = true;
        recoveringSessions = false;
        pendingRecoveryEvents.clear();
        pendingRecoveryBytes = 0;
        v2Options?.onRecoveryReset("recovery_live_overflow");
        emitSessionNow(event, eventId);
        return;
      }
      pendingRecoveryEvents.set(eventId, event);
      pendingRecoveryBytes += estimatedBytes;
      return;
    }
    emitSessionNow(event, eventId);
  };

  const unsubs = [
    sessionBus.subscribe(e => {
      // _event_id is stamped onto the payload by event-log.ts after the
      // durable insert; it is not part of the SessionEvent wire type.
      const stamped = e as SessionEvent & { _event_id?: number };
      const eid = stamped._event_id ?? 0;
      emitSession(e, eid);
    }),
    presenceBus.subscribe(e => { const f = presenceFrame(e); if (f) push(f); }),
    workspaceBus.subscribe(e => { const f = workspaceFrame(e); if (f) push(f); }),
    taskBus.subscribe(e => push(taskFrame(e))),
    permissionBus.subscribe(e => { const f = permFrame(e); if (f) push(f); }),
    mcpBus.subscribe(e => { const f = mcpFrame(e); if (f) push(f); }),
    webhookBus.subscribe(e => { const f = webhookFrame(e); if (f) push(f); }),
    auditBus.subscribe(e => push(auditFrame(e))),
    pairBus.subscribe(e => push(pairFrame(e))),
    globalPresenceBus.subscribe(({ session_id, data }) => {
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
        })},
      }));
    }),
    // V2 terminal cells are delivered exclusively by TerminalScreenHub. The
    titleBus.subscribe(({ session_id, title }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "terminalTitle", value: create(TerminalTitleFrameSchema, {
          sessionId: session_id, title,
        })},
      }))),
    lastActivityBus.subscribe(({ session_id, ts_ms }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "lastActivity", value: create(LastActivityFrameSchema, {
          sessionId: session_id, tsMs: ts_ms,
        })},
      }))),
    workerRoutableBus.subscribe(({ fps }) =>
      push(create(FirehoseFrameSchema, {
        frame: { case: "workerRoutable", value: create(WorkerRoutableFrameSchema, { fps })},
      }))),
    agentStatusBus.subscribe((status) => push(agentStatusFrame(status))),
    // ui-cc — both uiBus kinds map 1:1 onto their frames. state = a tab's
    // report re-broadcast (agents watch the spatial model live); command =
    // fire-and-forget UiDispatch relay the target tab executes.
    uiBus.subscribe((m) =>
      push(m.kind === "state"
        ? create(FirehoseFrameSchema, {
            frame: { case: "uiState", value: create(UiStateFrameSchema, {
              fp: m.fp, tabId: m.tabId, state: m.state,
            })},
          })
        : create(FirehoseFrameSchema, {
            frame: { case: "uiCommand", value: create(UiCommandFrameSchema, {
              targetTabId: m.targetTabId, command: m.command,
            })},
          }))),
  ];

  let seeded: Promise<void>;
  if (!seedOptions) {
    // Non-flow legacy clients retain the synchronous retained burst.
    for (const frame of retainedSeedFrames()) push(frame);
    seeded = Promise.resolve();
    void (async () => {
      const pairSnapshot = await loadPairSnapshot(deps.db);
      if (pairSnapshot) push(pairSnapshot);
    })();
  } else if (legacySeedOptions) {
    // Cached flow=1 clients retain the existing one-frame/one-ACK seed pacing.
    seeded = Promise.resolve().then(async () => {
      for (const frame of retainedSeedFrames()) {
        if (disposed || !(await legacySeedOptions.pacedSeedPush(frame))) return;
      }
      const pairSnapshot = await loadPairSnapshot(deps.db);
      if (
        pairSnapshot
        && (disposed || !(await legacySeedOptions.pacedSeedPush(pairSnapshot)))
      ) return;

      let queueIndex = 0;
      while (!disposed) {
        if (queueIndex === queuedLiveFrames.length) {
          seeding = false;
          queuedLiveFrames.length = 0;
          return;
        }
        const entry = queuedLiveFrames[queueIndex++]!;
        if (!(await legacySeedOptions.pacedSeedPush(entry.frame))) return;
      }
    }).catch((error) => {
      log.warn("connect.sync", "retained_seed_failed", { error: String(error) });
    });
  } else {
    // V2's caller emits subscribed only after startSyncFeed has synchronously
    // installed every bus listener. Defer control-only UI retention one
    // microtask so it cannot overtake that subscribed barrier.
    seeded = Promise.resolve();
    queueMicrotask(() => {
      if (disposed) return;
      for (const { fp, tabId, state } of getUiStateSnapshot()) {
        push(create(FirehoseFrameSchema, {
          frame: {
            case: "uiState",
            value: create(UiStateFrameSchema, { fp, tabId, state }),
          },
        }), { domain: null, lane: "control" });
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
          pendingRecoveryEvents.clear();
          v2Options.onRecoveryReset("cursor_ahead_of_log");
          return;
        }
        let cursor = sinceEventId;
        while (!disposed && !recoveryAborted && cursor < cutoff) {
          const rows = await getEventsThrough(deps.db, cursor, cutoff);
          if (rows.length === 0) {
            recoveringSessions = false;
            pendingRecoveryEvents.clear();
            v2Options.onRecoveryReset("recovery_gap");
            return;
          }
          for (const { id, event } of rows) {
            if (recoveryAborted) return;
            if (id <= cursor || id > cutoff) {
              recoveringSessions = false;
              pendingRecoveryEvents.clear();
              v2Options.onRecoveryReset("recovery_order");
              return;
            }
            emitSessionNow(event, id);
            cursor = id;
          }
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
        if (disposed || recoveryAborted) return;

        const liveTail = [...pendingRecoveryEvents.entries()]
          .filter(([id]) => id > cutoff)
          .sort(([left], [right]) => left - right);
        pendingRecoveryEvents.clear();
        pendingRecoveryBytes = 0;
        for (const [id, event] of liveTail) emitSessionNow(event, id);
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
        emitSession(event, id);
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
  };

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    seeding = false;
    recoveringSessions = false;
    queuedLiveFrames.length = 0;
    pendingRecoveryEvents.clear();
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
    seedDomain: (domain, sessionIds) => seedDomain(seedCtx, domain, sessionIds),
    dispose,
  };
}
