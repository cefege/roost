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
// startSyncFeed (handlers-streaming.ts), SHARED so this reader can't diverge
// from the frames the SPA already decodes. Same move already made for the
// worker↔coord transport (worker-ws-handler.ts).
//
// Auth: the device JWT travels in the standards-compliant `roost-auth`
// WebSocket subprotocol, never in the URL. Backfill cursor remains in
// `?since=<eventId>`.
//
// Lives in apps/coord (Bun-specific: server.upgrade). main.ts wires the
// upgrade + multiplexes the single Bun `websocket` handler with the worker WS.

import type { ServerWebSocket } from "bun";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordinatorRelocationFrameSchema,
  FirehoseFrameSchema,
  KeepaliveFrameSchema,
  SyncClientFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { jwtKeyGeneration, verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { startSyncFeed } from "./handlers-streaming.ts";
import type { ConnectDeps } from "./router.ts";

const WS_PATH = "/ws/coord-sync";

const KEEPALIVE_INTERVAL_MS = 30_000;
const BACKPRESSURE_LIMIT_BYTES = 8 * 1024 * 1024;
const BACKPRESSURE_TIMEOUT_MS = 10_000;
const MAX_TIMER_DELAY_MS = 2_147_483_647;
const APPLICATION_MAX_UNACKED_FRAMES = 512;
const APPLICATION_MAX_UNACKED_BYTES = 4 * 1024 * 1024;
const APPLICATION_ACK_TIMEOUT_MS = 3_000;

export interface SyncUpgradeServer {
  requestIP(req: Request): { address: string } | null;
  upgrade(
    req: Request,
    opts: { data: SyncWsData; headers?: HeadersInit },
  ): boolean;
}

export interface SyncDeadlineClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): Timer;
  clearTimeout(timer: Timer): void;
  maxDelayMs?: number;
}
export interface SyncDeadlineTimer {
  current: Timer | null;
}


const realDeadlineClock: SyncDeadlineClock = {
  now: Date.now,
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export function scheduleDeadline(
  ws: Pick<ServerWebSocket<SyncWsData>, "close">,
  deadlineMs: number,
  clock: SyncDeadlineClock = realDeadlineClock,
): SyncDeadlineTimer {
  const handle: SyncDeadlineTimer = { current: null };
  const arm = (): void => {
    handle.current = clock.setTimeout(() => {
      const remaining = deadlineMs - clock.now();
      if (remaining <= 0) {
        handle.current = null;
        ws.close(4003, "reauth required");
        return;
      }
      arm();
    }, Math.min(
      Math.max(0, deadlineMs - clock.now()),
      clock.maxDelayMs ?? MAX_TIMER_DELAY_MS,
    ));
  };
  arm();
  return handle;
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
  /** `${fingerprint}:${tabId}` — the same per-tab key sessionsResize claims
   *  use, so this socket can be fanned out only the sessions this tab claimed.
   *  null when the client sent no `tab=` (an older SPA build, the CLI, a test
   *  client): the fanout then FAILS OPEN and ships everything, as before. */
  viewerKey: string | null;
  feed: { seeded: Promise<void>; dispose: () => void } | null;
  keepaliveTimer: Timer | null;
  reauthAtMs: number | null;
  reauthTimer: SyncDeadlineTimer | null;
  pressureTimer: Timer | null;
  pressureFrame: string | null;
  pressureClosing: boolean;
  /** Enabled only by the exact `flow=1` upgrade query value. */
  flowControl: boolean;
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
  if (url.pathname !== WS_PATH) return null;
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
  if (protocols.length !== 2 || protocols[0] !== "roost-auth" || !protocols[1]) {
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
  const data: SyncWsData = {
    kind: "sync",
    caller,
    sinceEventId: since,
    viewerKey: tabId ? `${caller.fingerprint}:${tabId}` : null,
    feed: null,
    keepaliveTimer: null,
    reauthAtMs,
    reauthTimer: null,
    pressureTimer: null,
    pressureFrame: null,
    pressureClosing: false,
    flowControl: url.searchParams.get("flow") === "1",
    lastSentDeliverySeq: 0n,
    ackDeliverySeq: 0n,
    unackedEncodedBytes: 0,
    deliveryQueue: [],
    deliveryTimer: null,
    deliveryWaiters: new Set(),
  };
  const ok = server.upgrade(req, {
    data,
    headers: { "Sec-WebSocket-Protocol": "roost-auth" },
  });
  if (ok) return undefined; // hijacked
  return new Response("upgrade failed", { status: 400 });
}

export interface SyncWsHandlerOptions {
  keepaliveMs?: number;
  deadlineClock?: SyncDeadlineClock;
  backpressureLimitBytes?: number;
  backpressureTimeoutMs?: number;
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

  const clearNativePressure = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.pressureTimer) {
      deadlineClock.clearTimeout(ws.data.pressureTimer);
      ws.data.pressureTimer = null;
    }
    ws.data.pressureFrame = null;
  };

  const wakeDeliveryWaiters = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.deliveryWaiters.size === 0) return;
    const waiters = [...ws.data.deliveryWaiters];
    ws.data.deliveryWaiters.clear();
    for (const wake of waiters) wake();
  };

  const clearApplicationWindow = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.deliveryTimer) {
      deadlineClock.clearTimeout(ws.data.deliveryTimer);
      ws.data.deliveryTimer = null;
    }
    ws.data.deliveryQueue.length = 0;
    ws.data.unackedEncodedBytes = 0;
    ws.data.lastSentDeliverySeq = 0n;
    ws.data.ackDeliverySeq = 0n;
    wakeDeliveryWaiters(ws);
  };
  const cleanupSocket = (ws: ServerWebSocket<SyncWsData>): void => {
    sockets.delete(ws);
    if (ws.data.keepaliveTimer) {
      clearInterval(ws.data.keepaliveTimer);
      ws.data.keepaliveTimer = null;
    }
    if (ws.data.reauthTimer?.current) {
      deadlineClock.clearTimeout(ws.data.reauthTimer.current);
    }
    ws.data.reauthTimer = null;
    clearNativePressure(ws);
    clearApplicationWindow(ws);
    ws.data.feed?.dispose();
    ws.data.feed = null;
  };


  const applicationStats = (ws: ServerWebSocket<SyncWsData>): {
    unackedFrames: number;
    unackedBytes: number;
    oldestAgeMs: number;
  } => {
    const oldest = ws.data.deliveryQueue[0];
    return {
      unackedFrames: ws.data.deliveryQueue.length,
      unackedBytes: ws.data.unackedEncodedBytes,
      oldestAgeMs: oldest ? Math.max(0, deadlineClock.now() - oldest.sentAtMs) : 0,
    };
  };

  function closeForBackpressure(
    ws: ServerWebSocket<SyncWsData>,
    reason: "high_water" | "timeout" | "frame_limit" | "byte_limit" | "age_limit",
    frame: string,
  ): void {
    if (ws.data.pressureClosing) return;
    const stats = applicationStats(ws);
    let bufferedBytes = 0;
    try { bufferedBytes = ws.getBufferedAmount(); } catch { /* best-effort diagnostic */ }
    ws.data.pressureClosing = true;
    cleanupSocket(ws);
    signal("sync.queue_overflow", {
      caller_fp: ws.data.caller.fingerprint,
      reason,
      frame,
      buffered_bytes: bufferedBytes,
      unacked_frames: stats.unackedFrames,
      unacked_bytes: stats.unackedBytes,
      oldest_age_ms: stats.oldestAgeMs,
      cooldownKey: ws.data.caller.fingerprint,
    });
    ws.close(1013, "sync backpressure");
  }

  const rearmApplicationDeadline = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.deliveryTimer) {
      deadlineClock.clearTimeout(ws.data.deliveryTimer);
      ws.data.deliveryTimer = null;
    }
    const oldest = ws.data.deliveryQueue[0];
    if (!oldest || ws.data.pressureClosing) return;

    const onDeadline = (): void => {
      ws.data.deliveryTimer = null;
      if (ws.data.pressureClosing) return;
      const currentOldest = ws.data.deliveryQueue[0];
      if (!currentOldest) return;
      const remainingMs = currentOldest.sentAtMs + APPLICATION_ACK_TIMEOUT_MS - deadlineClock.now();
      if (remainingMs > 0) {
        ws.data.deliveryTimer = deadlineClock.setTimeout(onDeadline, remainingMs);
        return;
      }
      closeForBackpressure(ws, "age_limit", "age_deadline");
    };
    ws.data.deliveryTimer = deadlineClock.setTimeout(
      onDeadline,
      Math.max(0, oldest.sentAtMs + APPLICATION_ACK_TIMEOUT_MS - deadlineClock.now()),
    );
  };

  const closeForDroppedFrame = (
    ws: ServerWebSocket<SyncWsData>,
    frame: string,
    encodedBytes: number,
    bufferedBytes: number,
  ): void => {
    if (ws.data.pressureClosing) return;
    const stats = applicationStats(ws);
    ws.data.pressureClosing = true;
    cleanupSocket(ws);
    signal("sync.ws_frame_dropped", {
      caller_fp: ws.data.caller.fingerprint,
      frame,
      bytes: encodedBytes,
      buffered_bytes: bufferedBytes,
      unacked_frames: stats.unackedFrames,
      unacked_bytes: stats.unackedBytes,
      oldest_age_ms: stats.oldestAgeMs,
      cooldownKey: ws.data.caller.fingerprint,
    });
    ws.close(1013, "sync backpressure");
  };

  const sendGuarded = (ws: ServerWebSocket<SyncWsData>, frame: FirehoseFrame): boolean => {
    if (ws.data.pressureClosing) return false;
    try {
      const frameKind = frame.frame.case ?? "unknown";
      const nextSeq = ws.data.lastSentDeliverySeq + 1n;
      const outboundFrame = ws.data.flowControl
        ? create(FirehoseFrameSchema, { deliverySeq: nextSeq, frame: frame.frame })
        : frame;
      const bin = toBinary(FirehoseFrameSchema, outboundFrame);

      if (ws.data.flowControl) {
        if (ws.data.deliveryQueue.length + 1 > APPLICATION_MAX_UNACKED_FRAMES) {
          closeForBackpressure(ws, "frame_limit", frameKind);
          return false;
        }
        if (ws.data.unackedEncodedBytes + bin.byteLength > APPLICATION_MAX_UNACKED_BYTES) {
          closeForBackpressure(ws, "byte_limit", frameKind);
          return false;
        }
      }

      const sentAtMs = deadlineClock.now();
      const result = ws.send(bin);
      if (result !== 0 && ws.data.flowControl) {
        ws.data.lastSentDeliverySeq = nextSeq;
        ws.data.unackedEncodedBytes += bin.byteLength;
        ws.data.deliveryQueue.push({ seq: nextSeq, encodedBytes: bin.byteLength, sentAtMs });
        if (!ws.data.deliveryTimer) rearmApplicationDeadline(ws);
      }

      const bufferedBytes = ws.getBufferedAmount();
      if (result === 0) {
        closeForDroppedFrame(ws, frameKind, bin.byteLength, bufferedBytes);
        return false;
      }
      if (bufferedBytes > backpressureLimitBytes) {
        closeForBackpressure(ws, "high_water", frameKind);
        return false;
      }
      if (result === -1 && !ws.data.pressureTimer) {
        ws.data.pressureFrame = frameKind;
        ws.data.pressureTimer = deadlineClock.setTimeout(() => {
          ws.data.pressureTimer = null;
          closeForBackpressure(ws, "timeout", ws.data.pressureFrame ?? frameKind);
        }, backpressureTimeoutMs);
      }
      return true;
    } catch (e) {
      log.warn("sync-ws", "send_failed", { error: String(e) });
      return false;
    }
  };

  const waitForDeliveryChange = (
    ws: ServerWebSocket<SyncWsData>,
  ): Promise<void> => {
    const { promise, resolve } = Promise.withResolvers<void>();
    if (ws.data.pressureClosing) {
      resolve();
    } else {
      ws.data.deliveryWaiters.add(resolve);
    }
    return promise;
  };
  const pushPacedSeed = async (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
  ): Promise<boolean> => {
    // A retained seed is allowed onto the wire only when prior application
    // work has been cumulatively ACKed. This is ACK-coupled rather than a
    // fixed chunk, so an arbitrarily large seed cannot outrun the 512/4 MiB
    // window while live frames remain queued in startSyncFeed.
    while (!ws.data.pressureClosing && ws.data.deliveryQueue.length > 0) {
      await waitForDeliveryChange(ws);
    }
    if (ws.data.pressureClosing) return false;

    if (!sendGuarded(ws, frame)) {
      if (!ws.data.pressureClosing) {
        ws.data.pressureClosing = true;
        cleanupSocket(ws);
        ws.close(1013, "sync backpressure");
      }
      return false;
    }

    const sentSeq = ws.data.lastSentDeliverySeq;
    while (!ws.data.pressureClosing && ws.data.ackDeliverySeq < sentSeq) {
      await waitForDeliveryChange(ws);
    }
    return !ws.data.pressureClosing;
  };

  const closeForInvalidAck = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.pressureClosing) return;
    ws.data.pressureClosing = true;
    cleanupSocket(ws);
    ws.close(1008, "invalid sync ack");
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
      const push = (frame: FirehoseFrame): void => { sendGuarded(ws, frame); };
      const feed = startSyncFeed(
        deps,
        ws.data.sinceEventId,
        push,
        ws.data.viewerKey,
        ws.data.flowControl
          ? { pacedSeedPush: (frame) => pushPacedSeed(ws, frame) }
          : undefined,
      );
      if (ws.data.pressureClosing) {
        feed.dispose();
        return;
      }
      ws.data.feed = feed;
      // startSyncFeed subscribed synchronously. Opted-in sockets now finish
      // their retained snapshot and queued-live phase through ACK-paced sends
      // before durable backfill begins; legacy sockets keep synchronous seeds.
      void feed.backfill();
      // Keepalive: coord sends a FirehoseFrame{keepalive} every 30s so the
      // browser stale-link watchdog (apps/web/src/store/sync-watchdog.ts)
      // can tell a half-open connection (silence past 90s) from a merely
      // idle session. Mirrors worker-conn.ts. Stored on ws.data (per-conn),
      // NOT a closure — makeSyncWsHandler is called once; a closure var
      // would be shared across all connections (leak/cross-fire).
      ws.data.keepaliveTimer = setInterval(() => {
        sendGuarded(ws, create(FirehoseFrameSchema, {
          frame: { case: "keepalive", value: create(KeepaliveFrameSchema, { ts: BigInt(Date.now()) }) },
        }));
      }, keepaliveMs);
      log.info("sync-ws", "open", { caller_fp: ws.data.caller.fingerprint, since: ws.data.sinceEventId });
    },
    message(ws: ServerWebSocket<SyncWsData>, message: string | Buffer): void {
      if (!ws.data.flowControl || ws.data.pressureClosing) return;
      let ackDeliverySeq: bigint;
      try {
        if (typeof message === "string") throw new TypeError("Sync ACK must be binary");
        const clientFrame = fromBinary(SyncClientFrameSchema, message, {
          readUnknownFields: false,
        });
        ackDeliverySeq = clientFrame.ackDeliverySeq;
        const canonical = toBinary(SyncClientFrameSchema, clientFrame);
        if (
          ackDeliverySeq <= 0n
          || canonical.byteLength !== message.byteLength
          || canonical.some((byte, index) => byte !== message[index])
        ) throw new TypeError("Sync ACK must be positive and canonical");
      } catch {
        closeForInvalidAck(ws);
        return;
      }
      if (ackDeliverySeq > ws.data.lastSentDeliverySeq) {
        closeForInvalidAck(ws);
        return;
      }
      if (ackDeliverySeq <= ws.data.ackDeliverySeq) return;

      let releaseCount = 0;
      let releasedBytes = 0;
      for (const record of ws.data.deliveryQueue) {
        if (record.seq > ackDeliverySeq) break;
        releaseCount += 1;
        releasedBytes += record.encodedBytes;
      }
      if (releaseCount > 0) ws.data.deliveryQueue.splice(0, releaseCount);
      ws.data.unackedEncodedBytes -= releasedBytes;
      ws.data.ackDeliverySeq = ackDeliverySeq;
      rearmApplicationDeadline(ws);
      wakeDeliveryWaiters(ws);
    },
    drain(ws: ServerWebSocket<SyncWsData>): void {
      // Bun drain releases only native buffered-byte pressure. Application
      // delivery records remain until the browser cumulatively ACKs them.
      clearNativePressure(ws);
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
          if (sendGuarded(ws, frame)) ws.close();
        } catch { /* close handler cleans up */ }
      }
    },
  };
}
