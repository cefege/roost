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
import { clone, create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorRelocationFrameSchema,
  FirehoseFrameSchema,
  KeepaliveFrameSchema,
  SyncClientFrameSchema,
  SyncDomainGenerationSchema,
  SyncDomainResetFrameSchema,
  SyncSubscribedFrameSchema,
  SyncDomain,
  type FirehoseFrame,
  type SyncClientFrame,
} from "@roost/shared/proto/sync_pb";
import { jwtKeyGeneration, verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import {
  startSyncFeed,
  type SyncFeed,
  type SyncFeedFrameMeta,
  type SyncFeedLane,
} from "./handlers-streaming.ts";
import { consumeSyncSessionSnapshot, registerSyncSnapshotSocket } from "./sync-snapshot-registry.ts";
import type { ConnectDeps } from "./router.ts";

const WS_PATH = "/ws/coord-sync";

const KEEPALIVE_INTERVAL_MS = 30_000;
const BACKPRESSURE_LIMIT_BYTES = 8 * 1024 * 1024;
const BACKPRESSURE_TIMEOUT_MS = 10_000;
const MAX_TIMER_DELAY_MS = 2_147_483_647;
const APPLICATION_MAX_UNACKED_FRAMES = 512;
const APPLICATION_MAX_UNACKED_BYTES = 4 * 1024 * 1024;
const APPLICATION_ACK_TIMEOUT_MS = 3_000;
const V2_DOMAIN_MAX_QUEUED_FRAMES = 512;
const V2_DOMAIN_MAX_QUEUED_BYTES = 4 * 1024 * 1024;
const V2_AGGREGATE_MAX_QUEUED_FRAMES = 1_024;
const V2_AGGREGATE_MAX_QUEUED_BYTES = 8 * 1024 * 1024;
const V2_LOW_LANE_MAX_AGE_MS = 100;
const SYNC_PROCESS_EPOCH = randomUUID();
const V2_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.PERMISSIONS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
  SyncDomain.WEBHOOK,
  SyncDomain.AUDIT,
] as const;
const V2_WEIGHTED_LANES: readonly SyncFeedLane[] = [
  "cell", "cell", "cell", "cell", "cell", "cell", "cell", "cell",
  "session", "session", "session", "session",
  "retained", "retained",
  "nonterminal",
];
let nextDomainGeneration = BigInt(Date.now()) * 1024n;

function allocateDomainGeneration(): bigint {
  nextDomainGeneration += 1n;
  return nextDomainGeneration;
}

function isLazyDomain(domain: SyncDomain): boolean {
  return domain === SyncDomain.WEBHOOK || domain === SyncDomain.AUDIT;
}

type SyncTerminalCommand = Extract<
  SyncClientFrame["command"],
  { case: "viewport" | "input" }
>;
export type SyncV2ResultControl = Extract<
  FirehoseFrame["frame"],
  {
    case:
      | "viewportAccepted"
      | "viewportRejected"
      | "inputAccepted"
      | "inputRejected"
      | "inputAmbiguous";
  }
>;

export interface SyncV2CommandContext {
  readonly caller: SyncWsData["caller"];
  readonly viewerKey: string;
  readonly remoteAddress?: string;
  readonly socketId: string;
  readonly command: SyncTerminalCommand;
  reply(control: SyncV2ResultControl): boolean;
}

interface SyncV2QueuedFrame {
  readonly frame: FirehoseFrame;
  readonly meta: SyncFeedFrameMeta;
  readonly queuedAtMs: number;
  readonly estimatedBytes: number;
}

interface SyncV2DomainState {
  generation: bigint;
  subscribed: boolean;
  ready: boolean;
  queue: SyncV2QueuedFrame[];
  queuedBytes: number;
  /** Next insertion point for the retained snapshot preceding buffered live frames. */
  seedInsertIndex: number;
}

interface SyncV2SocketState {
  readonly socketId: string;
  readonly domains: Map<SyncDomain, SyncV2DomainState>;
  readonly announcedSessions: Set<string>;
  readonly pendingSessionAnnouncements: Map<string, bigint>;
  queuedFrames: number;
  queuedBytes: number;
  laneCursor: number;
  schedulerPending: boolean;
  snapshotDispose: (() => void) | null;
  closeNotified: boolean;
}

function createSyncV2SocketState(): SyncV2SocketState {
  const domains = new Map<SyncDomain, SyncV2DomainState>();
  for (const domain of V2_DOMAINS) {
    domains.set(domain, {
      generation: allocateDomainGeneration(),
      subscribed: !isLazyDomain(domain),
      ready: false,
      queue: [],
      queuedBytes: 0,
      seedInsertIndex: 0,
    });
  }
  return {
    socketId: randomUUID(),
    domains,
    announcedSessions: new Set(),
    pendingSessionAnnouncements: new Map(),
    queuedFrames: 0,
    queuedBytes: 0,
    laneCursor: 0,
    schedulerPending: false,
    snapshotDispose: null,
    closeNotified: false,
  };
}

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
  const flowControl = url.searchParams.get("flow") === "1";
  const syncV2 = flowControl && url.searchParams.get("sync_v") === "2";
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
  onV2Command?: (context: SyncV2CommandContext) => void;
  onV2Close?: (context: {
    viewerKey: string;
    socketId: string;
  }) => void;
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
  const clearV2State = (ws: ServerWebSocket<SyncWsData>): void => {
    const v2 = ws.data.v2;
    if (!v2) return;
    v2.schedulerPending = false;
    v2.queuedFrames = 0;
    v2.queuedBytes = 0;
    v2.announcedSessions.clear();
    v2.pendingSessionAnnouncements.clear();
    for (const domain of v2.domains.values()) {
      domain.queue.length = 0;
      domain.queuedBytes = 0;
      domain.seedInsertIndex = 0;
      domain.ready = false;
    }
    v2.snapshotDispose?.();
    v2.snapshotDispose = null;
  };
  const cleanupSocket = (ws: ServerWebSocket<SyncWsData>): void => {
    sockets.delete(ws);
    const v2 = ws.data.v2;
    if (v2 && !v2.closeNotified) {
      v2.closeNotified = true;
      const viewerKey = ws.data.viewerKey;
      if (viewerKey !== null) {
        options.onV2Close?.({ viewerKey, socketId: v2.socketId });
      }
    }
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
    clearV2State(ws);
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

  const sendV2ControlFrame = (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
  ): boolean => {
    if (!ws.data.v2 || ws.data.pressureClosing) return false;
    try {
      frame.deliverySeq = 0n;
      frame.domain = SyncDomain.UNSPECIFIED;
      frame.domainGeneration = 0n;
      const binary = toBinary(FirehoseFrameSchema, frame);
      const result = ws.send(binary);
      const bufferedBytes = ws.getBufferedAmount();
      if (result === 0) {
        closeForDroppedFrame(ws, frame.frame.case ?? "control", binary.byteLength, bufferedBytes);
        return false;
      }
      if (bufferedBytes > backpressureLimitBytes) {
        closeForBackpressure(ws, "high_water", frame.frame.case ?? "control");
        return false;
      }
      if (result === -1 && !ws.data.pressureTimer) {
        ws.data.pressureFrame = frame.frame.case ?? "control";
        ws.data.pressureTimer = deadlineClock.setTimeout(() => {
          ws.data.pressureTimer = null;
          closeForBackpressure(ws, "timeout", ws.data.pressureFrame ?? "control");
        }, backpressureTimeoutMs);
      }
      return true;
    } catch (error) {
      log.warn("sync-ws", "control_send_failed", { error: String(error) });
      return false;
    }
  };

  const clearV2DomainQueue = (
    ws: ServerWebSocket<SyncWsData>,
    domain: SyncV2DomainState,
  ): void => {
    const v2 = ws.data.v2;
    if (!v2 || domain.queue.length === 0) {
      domain.queue.length = 0;
      domain.queuedBytes = 0;
      domain.seedInsertIndex = 0;
      return;
    }
    v2.queuedFrames -= domain.queue.length;
    v2.queuedBytes -= domain.queuedBytes;
    domain.queue.length = 0;
    domain.queuedBytes = 0;
    domain.seedInsertIndex = 0;
  };

  const resetV2Domain = (
    ws: ServerWebSocket<SyncWsData>,
    domainId: SyncDomain,
    reason: string,
  ): void => {
    const v2 = ws.data.v2;
    const domain = v2?.domains.get(domainId);
    if (!v2 || !domain || ws.data.pressureClosing) return;
    clearV2DomainQueue(ws, domain);
    domain.generation = allocateDomainGeneration();
    domain.ready = false;
    if (domainId === SyncDomain.TERMINAL) {
      v2.announcedSessions.clear();
      v2.pendingSessionAnnouncements.clear();
    }
    sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
      frame: {
        case: "domainReset",
        value: create(SyncDomainResetFrameSchema, {
          domain: domainId,
          generation: domain.generation,
          reason,
          subscribed: domain.subscribed,
        }),
      },
    }));
  };

  const removeQueuedV2Cells = (
    ws: ServerWebSocket<SyncWsData>,
    sessionIds: ReadonlySet<string>,
  ): void => {
    const v2 = ws.data.v2;
    const terminal = v2?.domains.get(SyncDomain.TERMINAL);
    if (!v2 || !terminal || sessionIds.size === 0) return;
    for (let index = terminal.queue.length - 1; index >= 0; index -= 1) {
      const item = terminal.queue[index]!;
      if (item.meta.lane !== "cell" || !item.meta.sessionId || !sessionIds.has(item.meta.sessionId)) continue;
      if (index < terminal.seedInsertIndex) terminal.seedInsertIndex -= 1;
      terminal.queue.splice(index, 1);
      terminal.queuedBytes -= item.estimatedBytes;
      v2.queuedFrames -= 1;
      v2.queuedBytes -= item.estimatedBytes;
    }
  };

  const queuedV2FrameEligible = (
    v2: SyncV2SocketState,
    ackDeliverySeq: bigint,
    item: SyncV2QueuedFrame,
  ): boolean => {
    if (item.meta.lane !== "cell") return true;
    if (!item.meta.sessionId || !v2.announcedSessions.has(item.meta.sessionId)) return false;
    const announcementSeq = v2.pendingSessionAnnouncements.get(item.meta.sessionId);
    return announcementSeq === undefined || ackDeliverySeq >= announcementSeq;
  };

  const selectV2Candidate = (
    ws: ServerWebSocket<SyncWsData>,
  ): { domain: SyncV2DomainState; index: number; item: SyncV2QueuedFrame } | null => {
    const v2 = ws.data.v2;
    if (!v2) return null;
    type Candidate = { domain: SyncV2DomainState; index: number; item: SyncV2QueuedFrame };
    const heads: Candidate[] = [];
    // Preserve each domain's snapshot/live boundary. We may step past an
    // ineligible fenced cell to reach its opened event, but never reorder two
    // eligible items from the same domain.
    for (const domain of v2.domains.values()) {
      if (!domain.subscribed || !domain.ready) continue;
      for (let index = 0; index < domain.queue.length; index += 1) {
        const item = domain.queue[index]!;
        if (!queuedV2FrameEligible(v2, ws.data.ackDeliverySeq, item)) continue;
        heads.push({ domain, index, item });
        break;
      }
    }
    if (heads.length === 0) return null;

    const now = deadlineClock.now();
    let overdue: Candidate | null = null;
    for (const candidate of heads) {
      if (
        candidate.item.meta.lane !== "cell"
        && now - candidate.item.queuedAtMs >= V2_LOW_LANE_MAX_AGE_MS
        && (!overdue || candidate.item.queuedAtMs < overdue.item.queuedAtMs)
      ) overdue = candidate;
    }
    if (overdue) return overdue;

    for (let attempt = 0; attempt < V2_WEIGHTED_LANES.length; attempt += 1) {
      const lane = V2_WEIGHTED_LANES[v2.laneCursor]!;
      v2.laneCursor = (v2.laneCursor + 1) % V2_WEIGHTED_LANES.length;
      let selected: Candidate | null = null;
      for (const candidate of heads) {
        if (candidate.item.meta.lane !== lane) continue;
        if (!selected || candidate.item.queuedAtMs < selected.item.queuedAtMs) {
          selected = candidate;
        }
      }
      if (selected) return selected;
    }
    return heads.reduce((oldest, candidate) =>
      candidate.item.queuedAtMs < oldest.item.queuedAtMs ? candidate : oldest
    );
  };

  let scheduleV2: (ws: ServerWebSocket<SyncWsData>) => void;
  const flushV2 = (ws: ServerWebSocket<SyncWsData>): void => {
    const v2 = ws.data.v2;
    if (!v2 || ws.data.pressureClosing) return;
    v2.schedulerPending = false;
    for (let sentCount = 0; sentCount < 64; sentCount += 1) {
      const candidate = selectV2Candidate(ws);
      if (!candidate) return;
      const nextSeq = ws.data.lastSentDeliverySeq + 1n;
      const outbound = clone(FirehoseFrameSchema, candidate.item.frame);
      outbound.deliverySeq = nextSeq;
      const binary = toBinary(FirehoseFrameSchema, outbound);
      if (
        ws.data.deliveryQueue.length >= APPLICATION_MAX_UNACKED_FRAMES
        || ws.data.unackedEncodedBytes + binary.byteLength > APPLICATION_MAX_UNACKED_BYTES
      ) return;

      const sentAtMs = deadlineClock.now();
      const result = ws.send(binary);
      const bufferedBytes = ws.getBufferedAmount();
      if (result === 0) {
        closeForDroppedFrame(ws, outbound.frame.case ?? "application", binary.byteLength, bufferedBytes);
        return;
      }
      candidate.domain.queue.splice(candidate.index, 1);
      if (candidate.index < candidate.domain.seedInsertIndex) {
        candidate.domain.seedInsertIndex -= 1;
      }
      candidate.domain.queuedBytes -= candidate.item.estimatedBytes;
      v2.queuedFrames -= 1;
      v2.queuedBytes -= candidate.item.estimatedBytes;
      ws.data.lastSentDeliverySeq = nextSeq;
      ws.data.unackedEncodedBytes += binary.byteLength;
      ws.data.deliveryQueue.push({ seq: nextSeq, encodedBytes: binary.byteLength, sentAtMs });
      if (!ws.data.deliveryTimer) rearmApplicationDeadline(ws);

      if (candidate.item.meta.announces) {
        for (const sessionId of candidate.item.meta.announces) {
          v2.announcedSessions.add(sessionId);
          v2.pendingSessionAnnouncements.set(sessionId, nextSeq);
        }
      }
      if (candidate.item.meta.closes) {
        const closed = new Set(candidate.item.meta.closes);
        for (const sessionId of closed) {
          v2.announcedSessions.delete(sessionId);
          v2.pendingSessionAnnouncements.delete(sessionId);
        }
        removeQueuedV2Cells(ws, closed);
      }
      if (bufferedBytes > backpressureLimitBytes) {
        closeForBackpressure(ws, "high_water", outbound.frame.case ?? "application");
        return;
      }
      if (result === -1 && !ws.data.pressureTimer) {
        ws.data.pressureFrame = outbound.frame.case ?? "application";
        ws.data.pressureTimer = deadlineClock.setTimeout(() => {
          ws.data.pressureTimer = null;
          closeForBackpressure(ws, "timeout", ws.data.pressureFrame ?? "application");
        }, backpressureTimeoutMs);
      }
    }
    if (selectV2Candidate(ws)) scheduleV2(ws);
  };
  scheduleV2 = (ws): void => {
    const v2 = ws.data.v2;
    if (!v2 || v2.schedulerPending || ws.data.pressureClosing) return;
    v2.schedulerPending = true;
    queueMicrotask(() => {
      if (ws.data.v2 === v2 && v2.schedulerPending && !ws.data.pressureClosing) flushV2(ws);
    });
  };

  const enqueueV2Frame = (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
    meta?: SyncFeedFrameMeta,
  ): void => {
    const v2 = ws.data.v2;
    const effectiveMeta = meta ?? { domain: null, lane: "control" as const };
    if (!v2 || ws.data.pressureClosing) return;
    if (effectiveMeta.domain === null || effectiveMeta.lane === "control") {
      sendV2ControlFrame(ws, frame);
      return;
    }
    const domain = v2.domains.get(effectiveMeta.domain);
    if (!domain || !domain.subscribed) return;

    const owned = clone(FirehoseFrameSchema, frame);
    owned.deliverySeq = 0n;
    owned.domain = effectiveMeta.domain;
    owned.domainGeneration = domain.generation;
    const estimatedBytes = toBinary(FirehoseFrameSchema, owned).byteLength + 10;
    const exceedsDomain = domain.queue.length + 1 > V2_DOMAIN_MAX_QUEUED_FRAMES
      || domain.queuedBytes + estimatedBytes > V2_DOMAIN_MAX_QUEUED_BYTES;
    const exceedsAggregate = v2.queuedFrames + 1 > V2_AGGREGATE_MAX_QUEUED_FRAMES
      || v2.queuedBytes + estimatedBytes > V2_AGGREGATE_MAX_QUEUED_BYTES;
    if (exceedsDomain || exceedsAggregate) {
      if (effectiveMeta.domain === SyncDomain.TERMINAL) {
        closeForBackpressure(
          ws,
          exceedsDomain ? "frame_limit" : "byte_limit",
          frame.frame.case ?? "terminal",
        );
      } else {
        resetV2Domain(ws, effectiveMeta.domain, exceedsDomain ? "domain_overflow" : "aggregate_overflow");
      }
      return;
    }
    const item: SyncV2QueuedFrame = {
      frame: owned,
      meta: effectiveMeta,
      queuedAtMs: deadlineClock.now(),
      estimatedBytes,
    };
    if (effectiveMeta.beforeBuffered) {
      domain.queue.splice(domain.seedInsertIndex, 0, item);
      domain.seedInsertIndex += 1;
    } else {
      domain.queue.push(item);
    }
    domain.queuedBytes += estimatedBytes;
    v2.queuedFrames += 1;
    v2.queuedBytes += estimatedBytes;
    scheduleV2(ws);
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

  const applyCumulativeAck = (
    ws: ServerWebSocket<SyncWsData>,
    ackDeliverySeq: bigint,
  ): boolean => {
    if (ackDeliverySeq > ws.data.lastSentDeliverySeq) {
      closeForInvalidAck(ws);
      return false;
    }
    if (ackDeliverySeq <= ws.data.ackDeliverySeq) return true;
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
    const v2 = ws.data.v2;
    if (v2) {
      for (const [sessionId, deliverySeq] of v2.pendingSessionAnnouncements) {
        if (deliverySeq <= ackDeliverySeq) v2.pendingSessionAnnouncements.delete(sessionId);
      }
    }
    rearmApplicationDeadline(ws);
    wakeDeliveryWaiters(ws);
    if (v2) scheduleV2(ws);
    return true;
  };

  const handleV2Command = (
    ws: ServerWebSocket<SyncWsData>,
    clientFrame: SyncClientFrame,
  ): void => {
    const v2 = ws.data.v2;
    if (!v2 || clientFrame.socketId !== v2.socketId) return;
    const command = clientFrame.command;
    if (command.case === "domainReady") {
      const domain = v2.domains.get(command.value.domain);
      if (
        !domain
        || !domain.subscribed
        || command.value.generation !== domain.generation
      ) return;
      if (domain.ready) return;
      let terminalSessionIds: ReadonlySet<string> | undefined;
      if (command.value.domain === SyncDomain.TERMINAL) {
        const token = command.value.snapshotToken;
        const sessionIds = token
          ? consumeSyncSessionSnapshot(v2.socketId, token)
          : null;
        if (!sessionIds) {
          resetV2Domain(ws, SyncDomain.TERMINAL, "snapshot_token_invalid");
          return;
        }
        v2.announcedSessions.clear();
        v2.pendingSessionAnnouncements.clear();
        for (const sessionId of sessionIds) v2.announcedSessions.add(sessionId);
        terminalSessionIds = sessionIds;
      }
      domain.ready = true;
      void ws.data.feed?.seedDomain(command.value.domain, terminalSessionIds);
      scheduleV2(ws);
      return;
    }
    if (command.case === "domainSubscribe" || command.case === "domainUnsubscribe") {
      const domainId = command.value.domain;
      if (!isLazyDomain(domainId)) return;
      const domain = v2.domains.get(domainId);
      if (!domain || command.value.generation !== domain.generation) return;
      if (command.case === "domainSubscribe") {
        domain.subscribed = true;
        domain.ready = false;
        return;
      }
      clearV2DomainQueue(ws, domain);
      domain.subscribed = false;
      domain.ready = false;
      domain.generation = allocateDomainGeneration();
      sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
        frame: {
          case: "domainReset",
          value: create(SyncDomainResetFrameSchema, {
            domain: domainId,
            generation: domain.generation,
            reason: "unsubscribed",
            subscribed: false,
          }),
        },
      }));
      return;
    }
    if (command.case !== "viewport" && command.case !== "input") return;
    const viewerKey = ws.data.viewerKey;
    if (viewerKey === null) return;
    const terminal = v2.domains.get(SyncDomain.TERMINAL);
    if (
      !terminal?.ready
      || command.value.domainGeneration !== terminal.generation
    ) return;
    const owned = clone(SyncClientFrameSchema, clientFrame).command;
    if (owned.case !== "viewport" && owned.case !== "input") return;
    options.onV2Command?.({
      caller: ws.data.caller,
      viewerKey,
      remoteAddress: ws.data.remoteAddress ?? undefined,
      socketId: v2.socketId,
      command: owned,
      reply: (control) => {
        if (ws.data.v2 !== v2 || ws.data.pressureClosing) return false;
        return sendV2ControlFrame(ws, create(FirehoseFrameSchema, { frame: control }));
      },
    });
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
          (frame, meta) => enqueueV2Frame(ws, frame, meta),
          ws.data.viewerKey,
          {
            version: 2,
            onRecoveryReset: (reason) => {
              if (ws.data.v2 === v2) resetV2Domain(ws, SyncDomain.TERMINAL, reason);
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
        if (!sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
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
      } else {
        const push = (frame: FirehoseFrame): void => { sendGuarded(ws, frame); };
        feed = startSyncFeed(
          deps,
          ws.data.sinceEventId,
          push,
          ws.data.viewerKey,
          ws.data.flowControl
            ? { pacedSeedPush: (frame) => pushPacedSeed(ws, frame) }
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
        if (ws.data.v2) sendV2ControlFrame(ws, frame);
        else sendGuarded(ws, frame);
      }, keepaliveMs);
      log.info("sync-ws", "open", {
        caller_fp: ws.data.caller.fingerprint,
        since: ws.data.sinceEventId,
        sync_v: v2 ? 2 : 1,
      });
    },
    message(ws: ServerWebSocket<SyncWsData>, message: string | Buffer): void {
      if (!ws.data.flowControl || ws.data.pressureClosing) return;
      let clientFrame: SyncClientFrame;
      try {
        if (typeof message === "string") throw new TypeError("Sync client frame must be binary");
        clientFrame = fromBinary(SyncClientFrameSchema, message, {
          readUnknownFields: false,
        });
        const canonical = toBinary(SyncClientFrameSchema, clientFrame);
        if (
          canonical.byteLength !== message.byteLength
          || canonical.some((byte, index) => byte !== message[index])
        ) throw new TypeError("Sync client frame must be canonical");
      } catch {
        closeForInvalidAck(ws);
        return;
      }

      if (!ws.data.v2) {
        const ack = clientFrame.ackDeliverySeq;
        if (
          ack === undefined
          || ack <= 0n
          || clientFrame.command.case !== undefined
          || clientFrame.socketId !== ""
        ) {
          closeForInvalidAck(ws);
          return;
        }
        applyCumulativeAck(ws, ack);
        return;
      }

      if (clientFrame.socketId !== ws.data.v2.socketId) return;
      const ack = clientFrame.ackDeliverySeq;
      if (ack !== undefined && ack > 0n && !applyCumulativeAck(ws, ack)) return;
      if (
        (ack === undefined || ack === 0n)
        && clientFrame.command.case === undefined
      ) {
        closeForInvalidAck(ws);
        return;
      }
      handleV2Command(ws, clientFrame);
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
          const sent = ws.data.v2
            ? sendV2ControlFrame(ws, clone(FirehoseFrameSchema, frame))
            : sendGuarded(ws, frame);
          if (sent) ws.close();
        } catch { /* close handler cleans up */ }
      }
    },
  };
}
