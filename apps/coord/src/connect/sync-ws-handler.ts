// Raw-WebSocket Sync firehose transport (Bun-native, server→client only).
// Replaces the Connect server-streaming CoordinatorService.sync, which
// crashed coord under Bun 1.3.14: a browser aborting the long-lived
// streaming response tripped a use-after-free in RequestContext.onAbort
// (bun.report: HttpContext::onClose → Response.onAborted →
// RequestContext.onAbort → [js abort listener] → bus error). A raw WS close
// routes through Bun's websocket.close(ws) callback, NEVER through
// RequestContext.onAbort, so that fault path is unreachable.
//
// Carries the SAME FirehoseFrame proto frames as BINARY WS messages
// (toBinary). Only the transport tube changed — the feed itself lives in
// startSyncFeed (sync-feed.ts), SHARED so this reader can't diverge
// from the frames the SPA already decodes. Same move already made for the
// worker↔coord transport (worker-ws-handler.ts).
//
// Auth: the device JWT travels in the standards-compliant `roost-auth`
// WebSocket subprotocol, never in the URL. Backfill cursor remains in
// `?since=<eventId>`.
//
// Lives in apps/coord (Bun-specific: server.upgrade). main.ts wires the
// upgrade + multiplexes the single Bun `websocket` handler with the worker WS.
//
// This file is the transport ENTRY POINT and the socket wiring only. The three
// protocols it multiplexes live beside it:
//   sync-ws-deadline.ts     — reauth deadline scheduler
//   sync-ws-v1-delivery.ts  — ACK window + backpressure engine, v1 send path
//   sync-ws-v2-scheduler.ts — v2 weighted-lane egress scheduler
//   sync-ws-v2-commands.ts  — v2 client-command ingress
//   sync-ws-v2-state.ts     — v2 per-socket state vocabulary

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
import { jwtKeyGeneration, verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { startSyncFeed, type SyncFeed } from "./sync-feed.ts";
import { registerSyncSnapshotSocket } from "./sync-snapshot-registry.ts";
import { makeSyncWsClientIngress } from "./sync-ws-client-ingress.ts";
import {
  realDeadlineClock,
  scheduleDeadline,
  type SyncDeadlineClock,
  type SyncDeadlineTimer,
} from "./sync-ws-deadline.ts";
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
  createSyncV2SocketState,
  type SyncV2SocketState,
} from "./sync-ws-v2-state.ts";
import type { ConnectDeps } from "./router.ts";
import type { TerminalViewHub } from "./terminal-view-hub.ts";
// Wire protocol values (path/subprotocol/query negotiation) live in
// @roost/shared/wire/sync-ws so the SPA and CLI dials cannot drift from the
// upgrade match here.
import {
  SYNC_WS_PATH,
  SYNC_AUTH_SUBPROTOCOL,
  SYNC_QUERY_FLOW_V1,
  SYNC_QUERY_V2,
} from "@roost/shared/wire/sync-ws";

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

export interface SyncUpgradeServer {
  requestIP(req: Request): { address: string } | null;
  upgrade(
    req: Request,
    opts: { data: SyncWsData; headers?: HeadersInit },
  ): boolean;
}

function isAllowedWsOrigin(origin: string, host: string, cfg: ConnectDeps["cfg"]): boolean {
  if (
    origin === cfg.webPublicUrl
    || origin === cfg.publicUrl
    || cfg.corsAllowedOrigins.includes(origin)
    || origin === `https://${host}`
  ) return true;
  return cfg.relaxedCsp && origin === `http://${host}`;
}

export interface SyncDeliveryRecord {
  readonly seq: bigint;
  readonly encodedBytes: number;
  readonly sentAtMs: number;
}

export interface SyncWsData {
  kind: "sync";
  caller: { fingerprint: string; label?: string; keyGeneration: number };
  sinceEventId: number;
  /** `${fingerprint}:${tabId}` identifies the browser tab that owns socket-bound
   * terminal view handles and attributes typed input. A v2 socket without
   * `tab=` remains a read-only firehose consumer and cannot issue either. */
  viewerKey: string | null;
  remoteAddress?: string | null;
  feed: SyncFeed | null;
  keepaliveTimer: Timer | null;
  reauthAtMs: number | null;
  reauthTimer: SyncDeadlineTimer | null;
  pressureTimer: Timer | null;
  pressureFrame: string | null;
  pressureClosing: boolean;
  /** Enabled only by the exact `flow=1` upgrade query value. */
  flowControl: boolean;
  /** Present only for the exact `flow=1&sync_v=2` capability negotiation. */
  v2?: SyncV2SocketState;
  /** Last sequence accepted by ws.send (not merely encoded or attempted). */
  lastSentDeliverySeq: bigint;
  /** Highest cumulative ACK accepted from this socket. */
  ackDeliverySeq: bigint;
  unackedEncodedBytes: number;
  /** Metadata only: payloads and encoded buffers must never enter this queue. */
  deliveryQueue: SyncDeliveryRecord[];
  deliveryTimer: Timer | null;
  /** ACK/close notifications for the single retained-seed pacing phase. */
  deliveryWaiters: Set<() => void>;
}

/** Bun fetch-handler hook. Returns:
 *  - null      → not the sync-WS path; caller should continue to coord.fetch.
 *  - undefined → upgrade succeeded (Bun hijacked); return undefined from fetch.
 *  - Response  → reject (401 / 400); return it from fetch. */
export async function handleSyncWsUpgrade(
  req: Request,
  server: SyncUpgradeServer,
  deps: ConnectDeps,
  reauthAtMs: number | null = null,
): Promise<Response | undefined | null> {
  const url = new URL(req.url);
  if (url.pathname !== SYNC_WS_PATH) return null;
  // WS handshakes are GET, so main.ts's retired gate (`req.method !== "GET"`)
  // cannot see them. Any non-active mode must fail fast here, or a browser
  // reconnecting mid-move attaches to a frozen DB and gets keepalives forever
  // instead of falling into the AuthCoordIdentity discovery path.
  if (deps.move && deps.move.gate.mode !== "active") {
    return new Response("coordinator move in progress", { status: deps.move.gate.mode === "retired" ? 410 : 503 });
  }
  const wsOrigin = req.headers.get("origin");
  if (wsOrigin && !isAllowedWsOrigin(wsOrigin, url.host, deps.cfg)) {
    const addr = server.requestIP(req)?.address ?? undefined;
    signal("sync.auth_rejected", {
      reason: "origin_rejected",
      addr,
      cooldownKey: addr ?? "sync-origin",
    });
    return new Response("forbidden origin", { status: 403 });
  }
  const addr = server.requestIP(req)?.address ?? undefined;
  const protocols = (req.headers.get("sec-websocket-protocol") ?? "")
    .split(",")
    .map((part) => part.trim());
  if (protocols.length !== 2 || protocols[0] !== SYNC_AUTH_SUBPROTOCOL || !protocols[1]) {
    return new Response("unauthorized", { status: 401 });
  }
  const token = protocols[1];
  let caller: { fingerprint: string; label?: string; keyGeneration: number };
  try {
    caller = await verifyJwt(token, {
      db: deps.db, cache: deps.jwtCache, jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
    });
  } catch (e) {
    log.warn("sync-ws", "upgrade_jwt_failed", { error: String(e) });
    signal("sync.auth_rejected", { reason: "jwt_invalid", addr, cooldownKey: addr ?? "sync-auth" });
    return new Response("unauthorized", { status: 401 });
  }
  const since = Number(url.searchParams.get("since")) || 0;
  const tabId = url.searchParams.get("tab");
  const flowControl = url.searchParams.get("flow") === SYNC_QUERY_FLOW_V1;
  const syncV2 = flowControl && url.searchParams.get("sync_v") === SYNC_QUERY_V2;
  const data: SyncWsData = {
    kind: "sync",
    caller,
    sinceEventId: since,
    viewerKey: tabId ? `${caller.fingerprint}:${tabId}` : null,
    remoteAddress: addr ?? null,
    feed: null,
    keepaliveTimer: null,
    reauthAtMs,
    reauthTimer: null,
    pressureTimer: null,
    pressureFrame: null,
    pressureClosing: false,
    flowControl,
    v2: syncV2 ? createSyncV2SocketState() : undefined,
    lastSentDeliverySeq: 0n,
    ackDeliverySeq: 0n,
    unackedEncodedBytes: 0,
    deliveryQueue: [],
    deliveryTimer: null,
    deliveryWaiters: new Set(),
  };
  const ok = server.upgrade(req, {
    data,
    headers: { "Sec-WebSocket-Protocol": SYNC_AUTH_SUBPROTOCOL },
  });
  if (ok) return undefined; // hijacked
  return new Response("upgrade failed", { status: 400 });
}

export interface SyncWsHandlerOptions {
  keepaliveMs?: number;
  deadlineClock?: SyncDeadlineClock;
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
  const deadlineClock = options.deadlineClock ?? realDeadlineClock;
  const backpressureLimitBytes = options.backpressureLimitBytes ?? BACKPRESSURE_LIMIT_BYTES;
  const backpressureTimeoutMs = options.backpressureTimeoutMs ?? BACKPRESSURE_TIMEOUT_MS;
  const sockets = new Set<ServerWebSocket<SyncWsData>>();

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
        ws.data.reauthTimer = scheduleDeadline(ws, ws.data.reauthAtMs, deadlineClock);
      }
      sockets.add(ws);
      const v2 = ws.data.v2;
      let feed: SyncFeed;
      if (v2) {
        v2.snapshotDispose = registerSyncSnapshotSocket(v2.socketId, ws.data.caller.fingerprint);
        // startSyncFeed subscribes synchronously and performs no v2 seeding.
        // Only after every listener exists may the subscribed barrier escape.
        feed = startSyncFeed(
          deps,
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
          callerFingerprint: ws.data.caller.fingerprint,
          sink: {
            beginTerminalStream: (sessionId, streamId) =>
              v2Scheduler.beginTerminalStream(ws, sessionId, streamId),
            enqueueTerminalState: (frame, sessionId) =>
              v2Scheduler.enqueueTerminalState(ws, frame, sessionId),
            replaceTerminalSnapshot: (sessionId, streamId, frames) =>
              v2Scheduler.replaceTerminalSnapshot(ws, sessionId, streamId, frames),
            enqueueTerminalDelta: (sessionId, streamId, frame) =>
              v2Scheduler.enqueueTerminalDelta(ws, sessionId, streamId, frame),
            dropTerminalSession: (sessionId) =>
              v2Scheduler.dropTerminalSession(ws, sessionId),
          },
        });
      } else {
        const push = (frame: FirehoseFrame): void => { delivery.sendGuarded(ws, frame); };
        feed = startSyncFeed(
          deps,
          ws.data.sinceEventId,
          push,
          ws.data.viewerKey,
          ws.data.flowControl
            ? { pacedSeedPush: (frame) => delivery.pushPacedSeed(ws, frame) }
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
    closeForFingerprint(fingerprint: string): void {
      for (const ws of sockets) {
        if (ws.data.caller.fingerprint === fingerprint) {
          try { ws.close(4001, "revoked"); } catch { /* close handler cleans up */ }
        }
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
