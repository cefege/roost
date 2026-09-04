// Owns each live Sync WebSocket after authenticated upgrade: feed startup,
// ACK/backpressure delivery, v2 command scheduling, and deterministic cleanup.
// Snapshot listeners are installed before the subscribed barrier, so no live
// terminal frame can escape before the socket has a complete baseline boundary.

import type { ServerWebSocket } from "bun";
import { clone, create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorRelocationFrameSchema,
  FirehoseFrameSchema,
  KeepaliveFrameSchema,
  SyncDomainGenerationSchema,
  SyncSubscribedFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { jwtKeyGeneration } from "../jwt.ts";
import { log } from "@roost/shared/log";
import {
  startSyncFeed,
  type SyncFeed,
} from "./sync-feed.ts";
import { registerSyncSnapshotSocket } from "./sync-snapshot-registry.ts";
import { makeSyncWsClientIngress } from "./sync-ws-client-ingress.ts";
import {
  realWsDeadlineClock,
  scheduleWsAuthDeadline,
  type WsDeadlineClock,
} from "./ws-auth-deadline.ts";
import { makeSyncV1Delivery } from "./sync-ws-v1-delivery.ts";
import {
  makeSyncV2CommandHandler,
  type SyncV2CommandContext,
} from "./sync-ws-v2-commands.ts";
import {
  makeSyncV2Scheduler,
  type SyncV2Scheduler,
} from "./sync-ws-v2-scheduler.ts";
import {
  V2_DOMAINS,
  clearV2State,
} from "./sync-ws-v2-state.ts";
import type { ConnectDeps } from "./router.ts";
import type { TerminalViewHub } from "./terminal-view-hub.ts";
import type { SyncWsData } from "./sync-ws-upgrade.ts";
export {
  handleSyncWsUpgrade,
  type SyncDashboardActor,
  type SyncDeliveryRecord,
  type SyncUpgradeServer,
  type SyncWsData,
} from "./sync-ws-upgrade.ts";

/** Coord pushes a keepalive data frame at this cadence on every long-lived
 *  WebSocket — the browser Sync firehose here and the worker transport
 *  (worker-conn.ts) — because Bun's idleTimeout resets on any RECEIVED
 *  message, so a healthy but quiet connection must be given something to
 *  receive. Both ends share one value: a socket reaped on one side of the
 *  fleet but held on the other is the failure this prevents. */
export const KEEPALIVE_INTERVAL_MS = 30_000;
const BACKPRESSURE_LIMIT_BYTES = 8 * 1024 * 1024;
const BACKPRESSURE_TIMEOUT_MS = 10_000;
const SYNC_PROCESS_EPOCH = randomUUID();

export interface SyncWsHandlerOptions {
  keepaliveMs?: number;
  deadlineClock?: WsDeadlineClock;
  backpressureLimitBytes?: number;
  backpressureTimeoutMs?: number;
  onV2Command?: (context: SyncV2CommandContext) => void;
  onV2Close?: (context: {
    viewerKey: string;
    socketId: string;
  }) => void;
  terminalViews?: TerminalViewHub;
}

export function makeSyncWsHandler(
  deps: ConnectDeps,
  options: SyncWsHandlerOptions = {},
) {
  const keepaliveMs = options.keepaliveMs ?? KEEPALIVE_INTERVAL_MS;
  const deadlineClock = options.deadlineClock ?? realWsDeadlineClock;
  const backpressureLimitBytes = options.backpressureLimitBytes ?? BACKPRESSURE_LIMIT_BYTES;
  const backpressureTimeoutMs = options.backpressureTimeoutMs ?? BACKPRESSURE_TIMEOUT_MS;
  const sockets = new Set<ServerWebSocket<SyncWsData>>();
  const leaseExpiredSockets = new Set<string>();

  // Construction below is a forward-reference cycle by necessity, resolved the
  // same way this file already resolved the v2 scheduler's own recursion: every
  // hook crossing a module boundary is invoked per-frame, never during
  // construction. delivery's close paths need cleanupSocket, cleanupSocket needs
  // delivery's window teardown, and an accepted ACK reopens the v2 lane window.
  let cleanupSocket: (ws: ServerWebSocket<SyncWsData>) => void;
  let v2Scheduler: SyncV2Scheduler;
  const delivery = makeSyncV1Delivery({
    deadlineClock,
    backpressureLimitBytes,
    backpressureTimeoutMs,
    cleanupSocket: (ws) => cleanupSocket(ws),
    scheduleV2: (ws) => v2Scheduler.scheduleV2(ws),
  });
  v2Scheduler = makeSyncV2Scheduler({
    deadlineClock,
    backpressureLimitBytes,
    backpressureTimeoutMs,
    closeForBackpressure: delivery.closeForBackpressure,
    closeForDroppedFrame: delivery.closeForDroppedFrame,
    rearmApplicationDeadline: delivery.rearmApplicationDeadline,
  });
  const v2Commands = makeSyncV2CommandHandler({
    sendV2ControlFrame: v2Scheduler.sendV2ControlFrame,
    resetV2Domain: v2Scheduler.resetV2Domain,
    scheduleV2: v2Scheduler.scheduleV2,
    onV2Command: options.onV2Command,
  });
  const handleClientMessage = makeSyncWsClientIngress({
    closeForInvalidAck: delivery.closeForInvalidAck,
    applyCumulativeAck: delivery.applyCumulativeAck,
    handleV2Command: v2Commands.handleV2Command,
  });

  cleanupSocket = (ws: ServerWebSocket<SyncWsData>): void => {
    sockets.delete(ws);
    const v2 = ws.data.v2;
    if (v2 && !v2.closeNotified) {
      v2.closeNotified = true;
      options.terminalViews?.closeSocket(v2.socketId);
      const viewerKey = ws.data.viewerKey;
      if (viewerKey !== null) options.onV2Close?.({ viewerKey, socketId: v2.socketId });
    }
    if (ws.data.keepaliveTimer) {
      clearInterval(ws.data.keepaliveTimer);
      ws.data.keepaliveTimer = null;
    }
    if (ws.data.reauthTimer?.current) {
      deadlineClock.clearTimeout(ws.data.reauthTimer.current);
    }
    ws.data.reauthTimer = null;
    delivery.clearNativePressure(ws);
    delivery.clearApplicationWindow(ws);
    clearV2State(ws, (timer) => deadlineClock.clearTimeout(timer));
    ws.data.feed?.dispose();
    ws.data.feed = null;
  };
  options.terminalViews?.setOnLiveViewExpired((socketId) => {
    if (leaseExpiredSockets.has(socketId)) return;
    let owner: ServerWebSocket<SyncWsData> | null = null;
    for (const ws of sockets) {
      if (ws.data.v2?.socketId === socketId) {
        owner = ws;
        break;
      }
    }
    if (!owner || owner.data.pressureClosing) return;
    leaseExpiredSockets.add(socketId);
    owner.data.pressureClosing = true;
    cleanupSocket(owner);
    try {
      owner.close(1013, "terminal view lease expired");
    } catch {
      // cleanupSocket already retired every owner and timer.
    }
    queueMicrotask(() => leaseExpiredSockets.delete(socketId));
  });


  return {
    open(ws: ServerWebSocket<SyncWsData>): void {
      if (jwtKeyGeneration(deps.jwtCache, ws.data.caller.fingerprint) !== ws.data.caller.keyGeneration) {
        ws.close(4001, "revoked");
        return;
      }
      if (ws.data.reauthAtMs !== null) {
        if (ws.data.reauthAtMs <= deadlineClock.now()) {
          ws.close(4003, "reauth required");
          return;
        }
        ws.data.reauthTimer = scheduleWsAuthDeadline(ws, ws.data.reauthAtMs, deadlineClock);
      }
      sockets.add(ws);
      const v2 = ws.data.v2;
      let feed: SyncFeed;
      if (v2) {
        v2.snapshotDispose = registerSyncSnapshotSocket(
          v2.socketId,
          ws.data.caller.fingerprint,
          ws.data.actor.dashboardId,
        );
        // startSyncFeed subscribes synchronously and performs no v2 seeding.
        // Only after every listener exists may the subscribed barrier escape.
        feed = startSyncFeed(
          deps,
          ws.data.scope,
          ws.data.sinceEventId,
          (frame, meta) => v2Scheduler.enqueueV2Frame(ws, frame, meta),
          ws.data.viewerKey,
          {
            version: 2,
            onRecoveryReset: (reason) => {
              if (ws.data.v2 === v2) v2Scheduler.resetV2Domain(ws, SyncDomain.TERMINAL, reason);
            },
          },
        );
        const generations = V2_DOMAINS.map((domainId) => {
          const domain = v2.domains.get(domainId)!;
          return create(SyncDomainGenerationSchema, {
            domain: domainId,
            generation: domain.generation,
            subscribed: domain.subscribed,
          });
        });
        if (!v2Scheduler.sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
          frame: {
            case: "subscribed",
            value: create(SyncSubscribedFrameSchema, {
              socketId: v2.socketId,
              processEpoch: SYNC_PROCESS_EPOCH,
              generations,
            }),
          },
        }))) {
          feed.dispose();
          cleanupSocket(ws);
          try { ws.close(1011, "subscribed send failed"); } catch { /* already closed */ }
          return;
        }
        options.terminalViews?.registerSocket({
          socketId: v2.socketId,
          viewerKey: ws.data.viewerKey,
          dashboardId: ws.data.actor.dashboardId,
          allowsSession: (sessionId) => ws.data.scope.sessionIds.has(sessionId),
          callerFingerprint: ws.data.caller.fingerprint,
          sink: {
            beginTerminalStream: (sessionId, streamId) =>
              v2Scheduler.beginTerminalStream(ws, sessionId, streamId),
            enqueueTerminalState: (frame, sessionId) =>
              v2Scheduler.enqueueTerminalState(ws, frame, sessionId),
            replaceTerminalSnapshot: (sessionId, streamId, frames) =>
              v2Scheduler.replaceTerminalSnapshot(ws, sessionId, streamId, frames),
            enqueueTerminalDelta: (sessionId, streamId, frame) => {
              const terminal = ws.data.v2?.domains.get(SyncDomain.TERMINAL);
              const generation = terminal?.generation;
              if (v2Scheduler.enqueueTerminalDelta(ws, sessionId, streamId, frame)) {
                return "queued";
              }
              const current = ws.data.v2?.domains.get(SyncDomain.TERMINAL);
              if (
                ws.data.pressureClosing
                || generation === undefined
                || current?.generation !== generation
              ) return "handled";
              return "needs_snapshot";
            },
            dropTerminalSession: (sessionId) =>
              v2Scheduler.dropTerminalSession(ws, sessionId),
          },
        });
      } else {
        const push = (frame: FirehoseFrame): void => { delivery.sendGuarded(ws, frame); };
        feed = startSyncFeed(
          deps,
          ws.data.scope,
          ws.data.sinceEventId,
          push,
          ws.data.viewerKey,
          ws.data.flowControl
            ? {
              pacedSeedPush: (frame) => delivery.pushPacedSeed(ws, frame),
              onBufferOverflow: (reason, frame) =>
                delivery.closeForBackpressure(ws, reason, frame),
            }
            : undefined,
        );
      }
      if (ws.data.pressureClosing) {
        feed.dispose();
        return;
      }
      ws.data.feed = feed;
      void feed.backfill();
      ws.data.keepaliveTimer = setInterval(() => {
        const frame = create(FirehoseFrameSchema, {
          frame: { case: "keepalive", value: create(KeepaliveFrameSchema, { ts: BigInt(Date.now()) }) },
        });
        if (ws.data.v2) v2Scheduler.sendV2ControlFrame(ws, frame);
        else delivery.sendGuarded(ws, frame);
      }, keepaliveMs);
      log.info("sync-ws", "open", {
        caller_fp: ws.data.caller.fingerprint,
        since: ws.data.sinceEventId,
        sync_v: v2 ? 2 : 1,
      });
    },
    message(ws: ServerWebSocket<SyncWsData>, message: string | Buffer): void {
      handleClientMessage(ws, message);
    },
    drain(ws: ServerWebSocket<SyncWsData>): void {
      // Bun drain releases only native buffered-byte pressure. Application
      // delivery records remain until the browser cumulatively ACKs them.
      delivery.clearNativePressure(ws);
    },
    close(ws: ServerWebSocket<SyncWsData>): void {
      ws.data.pressureClosing = true;
      cleanupSocket(ws);
      log.info("sync-ws", "close", { caller_fp: ws.data.caller.fingerprint });
    },
    /** Remove a tombstoned worker from every already-open mutable dashboard
     * scope before the presence delta is published. */
    removeWorkerFromScopes(dashboardId: string, workerFp: string): void {
      for (const ws of sockets) {
        if (ws.data.scope.dashboardId === dashboardId) {
          ws.data.scope.workerFps.delete(workerFp);
        }
      }
    },
    closeForFingerprint(fingerprint: string): void {
      for (const ws of sockets) {
        if (ws.data.caller.fingerprint === fingerprint) {
          try { ws.close(4001, "revoked"); } catch { /* close handler cleans up */ }
        }
      }
    },
    closeForDashboard(dashboardId: string, fingerprint?: string): void {
      // A membership-wide revocation removes every scoped view. Device
      // revocation narrows to the affected fingerprint and preserves peers.
      if (fingerprint === undefined) options.terminalViews?.removeDashboard(dashboardId);
      else options.terminalViews?.removeFingerprint(fingerprint);
      for (const ws of sockets) {
        if (
          ws.data.actor.dashboardId !== dashboardId
          || (fingerprint !== undefined && ws.data.caller.fingerprint !== fingerprint)
        ) continue;
        try { ws.close(4001, "dashboard access revoked"); } catch { /* close handler cleans up */ }
      }
    },
    publishRelocation(handoffId: string, sourceUrl: string, targetUrl: string): void {
      const frame = create(FirehoseFrameSchema, {
        frame: {
          case: "coordinatorRelocation",
          value: create(CoordinatorRelocationFrameSchema, { handoffId, sourceUrl, targetUrl }),
        },
      });
      for (const ws of sockets) {
        try {
          const sent = ws.data.v2
            ? v2Scheduler.sendV2ControlFrame(ws, clone(FirehoseFrameSchema, frame))
            : delivery.sendGuarded(ws, frame);
          if (sent) ws.close();
        } catch { /* close handler cleans up */ }
      }
    },
  };
}
