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
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordinatorRelocationFrameSchema, FirehoseFrameSchema, KeepaliveFrameSchema, type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { jwtKeyGeneration, verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { startSyncFeed } from "./handlers-streaming.ts";
import type { ConnectDeps } from "./router.ts";

const WS_PATH = "/ws/coord-sync";

const KEEPALIVE_INTERVAL_MS = 30_000;
const MAX_TIMER_DELAY_MS = 2_147_483_647;

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

export interface SyncWsData {
  kind: "sync";
  caller: { fingerprint: string; label?: string; keyGeneration: number };
  sinceEventId: number;
  /** `${fingerprint}:${tabId}` — the same per-tab key sessionsResize claims
   *  use, so this socket can be fanned out only the sessions this tab claimed.
   *  null when the client sent no `tab=` (an older SPA build, the CLI, a test
   *  client): the fanout then FAILS OPEN and ships everything, as before. */
  viewerKey: string | null;
  feed: { dispose: () => void } | null;
  keepaliveTimer: Timer | null;
  reauthAtMs: number | null;
  reauthTimer: SyncDeadlineTimer | null;
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
  };
  const ok = server.upgrade(req, {
    data,
    headers: { "Sec-WebSocket-Protocol": "roost-auth" },
  });
  if (ok) return undefined; // hijacked
  return new Response("upgrade failed", { status: 400 });
}

export function makeSyncWsHandler(
  deps: ConnectDeps,
  keepaliveMs: number = KEEPALIVE_INTERVAL_MS,
  deadlineClock: SyncDeadlineClock = realDeadlineClock,
) {
  const sockets = new Set<ServerWebSocket<SyncWsData>>();
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
      const push = (f: FirehoseFrame): void => {
        try {
          const bin = toBinary(FirehoseFrameSchema, f);
          // Bun's contract: 0 = DROPPED, -1 = enqueued under backpressure,
          // >0 = bytes sent. Only 0 loses the frame, and a lost cell frame is
          // otherwise invisible — the SPA's seq-gap detector recovers the pane,
          // but nothing would ever say the coord side was the cause.
          if (ws.send(bin) === 0) {
            signal("sync.ws_frame_dropped", {
              caller_fp: ws.data.caller.fingerprint,
              frame: f.frame.case ?? "unknown",
              bytes: bin.byteLength,
              cooldownKey: "sync-ws",
            });
          }
        }
        catch (e) { log.warn("sync-ws", "send_failed", { error: String(e) }); }
      };
      const feed = startSyncFeed(deps, ws.data.sinceEventId, push, ws.data.viewerKey);
      ws.data.feed = feed;
      // Subscribe-first is already done inside startSyncFeed; kick backfill
      // after so an event firing during getEventsSince isn't lost.
      void feed.backfill();
      // Keepalive: coord sends a FirehoseFrame{keepalive} every 30s so the
      // browser stale-link watchdog (apps/web/src/store/sync-watchdog.ts)
      // can tell a half-open connection (silence past 90s) from a merely
      // idle session. Mirrors worker-conn.ts. Stored on ws.data (per-conn),
      // NOT a closure — makeSyncWsHandler is called once; a closure var
      // would be shared across all connections (leak/cross-fire).
      ws.data.keepaliveTimer = setInterval(() => {
        try {
          ws.send(toBinary(FirehoseFrameSchema, create(FirehoseFrameSchema, {
            frame: { case: "keepalive", value: create(KeepaliveFrameSchema, { ts: BigInt(Date.now()) }) },
          })));
        } catch { /* socket gone — close() will dispose */ }
      }, keepaliveMs);
      log.info("sync-ws", "open", { caller_fp: ws.data.caller.fingerprint, since: ws.data.sinceEventId });
    },
    message(_ws: ServerWebSocket<SyncWsData>, _message: string | Buffer): void {
      // Server→client only. The client sends nothing on this socket.
    },
    close(ws: ServerWebSocket<SyncWsData>): void {
      sockets.delete(ws);
      if (ws.data.keepaliveTimer) { clearInterval(ws.data.keepaliveTimer); ws.data.keepaliveTimer = null; }
      if (ws.data.reauthTimer?.current) {
        deadlineClock.clearTimeout(ws.data.reauthTimer.current);
      }
      ws.data.reauthTimer = null;
      ws.data.feed?.dispose();
      ws.data.feed = null;
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
      const frame = toBinary(FirehoseFrameSchema, create(FirehoseFrameSchema, {
        frame: { case: "coordinatorRelocation", value: create(CoordinatorRelocationFrameSchema, { handoffId, sourceUrl, targetUrl }) },
      }));
      for (const ws of sockets) {
        try { ws.send(frame); ws.close(); } catch { /* close handler cleans up */ }
      }
    },
  };
}
