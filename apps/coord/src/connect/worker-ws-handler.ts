// Raw-WebSocket coord↔worker transport (Bun-native, full-duplex). Replaces
// the Connect-bidi WorkerService.Attach, which can't hold a stable
// full-duplex stream under Bun: connect-node has no working h2 (Bun's
// node:http2 is incomplete → "[internal] h2 is not supported"), and over
// h1.1 Bun.serve buffers the long-lived request body so the worker's
// upstream rpc-ok replies never reached coord → every sessionsSpawn hung
// ("[internal] internal error").
//
// Carries the SAME CoordWorkerUp/CoordWorkerDown proto frames as BINARY WS
// messages (toBinary/fromBinary) — only the transport tube changed. All
// frame-handling lives in makeWorkerConn (worker-service.ts), shared with
// the Connect handler so the reader can't diverge. pending-rpcs, byte-hub,
// connectWorkers registry, keepalive, respawn-if-missing: all unchanged.
//
// Auth: query-param JWT (`?token=`) — Bun's CLIENT WebSocket has no
// custom-header API, so the Authorization header isn't available; this is
// the proven pre-crpc5 pattern. Verified at upgrade; the URL fp must equal
// the JWT caller fingerprint.
//
// Lives in apps/coord (Bun-specific: server.upgrade). coord-factory.ts stays
// fetch-only/portable — main.ts wires the upgrade + websocket handler.

import type { Server, ServerWebSocket } from "bun";
import { fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, CoordWorkerDownSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { makeWorkerConn, type WorkerConn, type WorkerServiceDeps } from "./worker-service.ts";

const WS_PATH_RE = /^\/ws\/coord-worker\/([a-f0-9]{64})$/;

export interface WorkerWsData {
  kind: "worker";
  caller: { fingerprint: string; label?: string };
  fp: string;
  conn: WorkerConn | null;
  // Per-socket serialization tail: Bun delivers messages in order but does
  // not await our async handler between them, so chain handleUpstream calls
  // here to keep event appends ordered (the seqno-splice invariant).
  tail: Promise<void>;
}

/** Bun fetch-handler hook. Returns:
 *  - null      → not a worker-WS path; caller should continue to coord.fetch.
 *  - undefined → upgrade succeeded (Bun hijacked); return undefined from fetch.
 *  - Response  → reject (401 / 400); return it from fetch. */
export async function handleWorkerWsUpgrade(
  req: Request, server: Server<WorkerWsData>, deps: WorkerServiceDeps,
): Promise<Response | undefined | null> {
  const url = new URL(req.url);
  const m = WS_PATH_RE.exec(url.pathname);
  if (!m) return null;
  // WS handshakes are GET, so main.ts's retired gate cannot see them. Reject
  // ONLY on `retired`: the link must stay open through `source_draining` to
  // buffer unacked events and receive ACTIVATE. Rejecting once retired is what
  // makes coord-relocation-recovery's link-closed guard engage.
  if (deps.move?.gate.mode === "retired") return new Response("coordinator relocated", { status: 410 });
  const fp = m[1]!;
  const addr = server.requestIP?.(req)?.address ?? undefined;
  const token = url.searchParams.get("token");
  if (!token) return new Response("unauthorized", { status: 401 });
  let caller: { fingerprint: string; label?: string };
  try {
    caller = await verifyJwt(token, {
      db: deps.db, cache: deps.jwtCache, jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
    });
  } catch (e) {
    log.warn("worker-ws", "upgrade_jwt_failed", { error: String(e), url_fp: fp, forwarded_for: req.headers.get("x-forwarded-for") });
    signal("worker.auth_rejected", { reason: "jwt_invalid", addr, cooldownKey: addr ?? "worker-auth" });
    return new Response("unauthorized", { status: 401 });
  }
  if (caller.fingerprint !== fp) {
    log.warn("worker-ws", "upgrade_fp_mismatch", { url_fp: fp, jwt_fp: caller.fingerprint });
    signal("worker.auth_rejected", { reason: "fp_mismatch", addr, cooldownKey: addr ?? "worker-auth" });
    return new Response("unauthorized", { status: 401 });
  }
  const data: WorkerWsData = { kind: "worker", caller, fp, conn: null, tail: Promise.resolve() };
  const ok = server.upgrade(req, { data });
  if (ok) return undefined; // hijacked
  return new Response("upgrade failed", { status: 400 });
}

export function makeWorkerWsHandler(deps: WorkerServiceDeps) {
  return {
    open(ws: ServerWebSocket<WorkerWsData>): void {
      // Return Bun's send result (0 = dropped, -1 = backpressure, >0 = bytes)
      // and re-throw: the snapshot pump must learn a chunk was lost instead of
      // sending `last: true` over a hole. The PTY/browser-command hot path is
      // contained by getWorkerHubSocket's own try/catch.
      const send = (f: CoordWorkerDown): number => {
        try { return ws.send(toBinary(CoordWorkerDownSchema, f)); }
        catch (e) {
          log.warn("worker-ws", "send_failed", { worker_fp: ws.data.fp, error: String(e) });
          throw e;
        }
      };
      const requestClose = (): void => { try { ws.close(); } catch { /* ignore */ } };
      ws.data.conn = makeWorkerConn(deps, ws.data.caller, send, requestClose, () => ws.getBufferedAmount());
      log.info("worker-ws", "open", { worker_fp: ws.data.fp });
    },
    message(ws: ServerWebSocket<WorkerWsData>, message: string | Buffer): void {
      const conn = ws.data.conn;
      if (!conn) return;
      let frame;
      try {
        // COPY off Bun's ServerWebSocket message buffer — it is pooled and
        // recycled after this synchronous handler returns. protobuf fromBinary
        // returns a subarray VIEW for `bytes` fields (PtyOut terminal data), and
        // handleUpstream runs DEFERRED via the .tail microtask below — so the
        // view would outlive Bun's buffer and be dereferenced as freed memory:
        // the coord segfault whose fault address is terminal bytes (ESC[, session
        // ids). L11 borrowed-Bun-buffer-view class (feedback_bun_terminal_write_needs_copy).
        const bytes = typeof message === "string"
          ? new TextEncoder().encode(message)
          : Uint8Array.from(message);
        frame = fromBinary(CoordWorkerUpSchema, bytes);
      } catch (e) {
        log.warn("worker-ws", "decode_failed", { worker_fp: ws.data.fp, error: String(e) });
        return;
      }
      // Fast path: in-memory bus publishes — no DB write, no ordering
      // constraint. Process immediately so an echo cell frame never waits
      // behind a DB-writing event frame (the .tail variance amplifier).
      //
      const fcase = frame.frame.case;
      if (fcase === "binary" || fcase === "cellGrid") {
        void conn.handleUpstream(frame).catch((e) => {
          log.warn("worker-ws", "handle_failed", { worker_fp: ws.data.fp, error: String(e) });
        });
        return;
      }
      // Slow path: event frames need serialization to preserve appendEvent
      // ordering (the seqno-splice invariant).
      ws.data.tail = ws.data.tail
        .then(() => conn.handleUpstream(frame))
        .catch((e) => {
          // A throw = fatal (DB durability fault); tear down so the worker
          // reconnects + replays unacked.
          log.warn("worker-ws", "handle_failed", { worker_fp: ws.data.fp, error: String(e) });
          signal("event.append_failed", { error: String(e), phase: "ws_handle", cooldownKey: "events" });
          try { ws.close(); } catch { /* ignore */ }
        });
    },
    close(ws: ServerWebSocket<WorkerWsData>): void {
      ws.data.conn?.close();
      ws.data.conn = null;
      log.info("worker-ws", "close", { worker_fp: ws.data.fp });
    },
  };
}
