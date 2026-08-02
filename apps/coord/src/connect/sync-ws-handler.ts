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
// Auth: query-param JWT (`?token=`) — the browser WebSocket API can't set
// custom headers, so the Authorization header isn't available. Verified at
// upgrade. Browser caller (sub="web"): NO fingerprint-in-path match (that
// check is worker-only). Backfill cursor via `?since=<eventId>`.
//
// Lives in apps/coord (Bun-specific: server.upgrade). main.ts wires the
// upgrade + multiplexes the single Bun `websocket` handler with the worker WS.

import type { Server, ServerWebSocket } from "bun";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordinatorRelocationFrameSchema, FirehoseFrameSchema, KeepaliveFrameSchema, type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { startSyncFeed } from "./handlers-streaming.ts";
import type { ConnectDeps } from "./router.ts";

const WS_PATH = "/ws/coord-sync";

const KEEPALIVE_INTERVAL_MS = 30_000;

export interface SyncWsData {
  kind: "sync";
  caller: { fingerprint: string; label?: string };
  sinceEventId: number;
  /** `${fingerprint}:${tabId}` — the same per-tab key sessionsResize claims
   *  use, so this socket can be fanned out only the sessions this tab claimed.
   *  null when the client sent no `tab=` (an older SPA build, the CLI, a test
   *  client): the fanout then FAILS OPEN and ships everything, as before. */
  viewerKey: string | null;
  feed: { dispose: () => void } | null;
  keepaliveTimer: Timer | null;
}

/** Bun fetch-handler hook. Returns:
 *  - null      → not the sync-WS path; caller should continue to coord.fetch.
 *  - undefined → upgrade succeeded (Bun hijacked); return undefined from fetch.
 *  - Response  → reject (401 / 400); return it from fetch. */
export async function handleSyncWsUpgrade(
  req: Request, server: Server<SyncWsData>, deps: ConnectDeps,
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
  const addr = server.requestIP(req)?.address ?? undefined;
  const token = url.searchParams.get("token");
  if (!token) return new Response("unauthorized", { status: 401 });
  let caller: { fingerprint: string; label?: string };
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
    kind: "sync", caller, sinceEventId: since,
    viewerKey: tabId ? `${caller.fingerprint}:${tabId}` : null,
    feed: null, keepaliveTimer: null,
  };
  const ok = server.upgrade(req, { data });
  if (ok) return undefined; // hijacked
  return new Response("upgrade failed", { status: 400 });
}

export function makeSyncWsHandler(deps: ConnectDeps, keepaliveMs: number = KEEPALIVE_INTERVAL_MS) {
  const sockets = new Set<ServerWebSocket<SyncWsData>>();
  return {
    open(ws: ServerWebSocket<SyncWsData>): void {
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
      ws.data.feed?.dispose();
      ws.data.feed = null;
      log.info("sync-ws", "close", { caller_fp: ws.data.caller.fingerprint });
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
