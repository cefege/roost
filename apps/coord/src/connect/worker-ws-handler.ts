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
import { jwtKeyGeneration, verifyJwt } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { diag, signal } from "@roost/shared/diag";
import { makeWorkerConn, type WorkerConn, type WorkerServiceDeps } from "./worker-service.ts";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { asChannelId, asWorkerFp } from "@roost/shared/wire";
import {
  clearBarrierRepairForWorker,
  lookupSessionId,
  noteBarrierChannelLoss,
  noteBarrierRepairFullFrames,
} from "../byte-hub.ts";
import { requestBarrierRepairFullFrame } from "./session-control.ts";
import {
  AnnouncedChannelBarrier,
  type AnnouncedDrop,
} from "./announced-channel-barrier.ts";

const WS_PATH_RE = /^\/ws\/coord-worker\/([a-f0-9]{64})$/;

export interface WorkerWsData {
  kind: "worker";
  caller: { fingerprint: string; label?: string; keyGeneration: number };
  fp: string;
  conn: WorkerConn | null;
  // Per-socket serialization tail: Bun delivers messages in order but does
  // not await our async handler between them, so chain handleUpstream calls
  // here to keep event appends ordered (the seqno-splice invariant).
  tail: Promise<void>;
  // Cell grids and PTY chunks may overtake the async durable opened/respawned
  // append, but only after this socket synchronously decoded that exact
  // worker/channel announcement.
  announcedChannels: AnnouncedChannelBarrier;
}

/** The barrier abandoned a channel's buffer. Cells lost here return only as a
 *  full frame, so mark the exact route — the browser cannot know its held
 *  sequence went stale — and, when a viewer is actually watching, replay one
 *  refresh claim now instead of waiting for an unrelated delta or a reload. */
function handleAnnouncedDrop(workerFp: string, drop: AnnouncedDrop): void {
  const marked = noteBarrierChannelLoss({
    workerFp,
    sessionId: drop.sessionId,
    channelId: drop.channelId,
    reason: drop.reason,
    phase: drop.phase,
    cellFrames: drop.cellFrames,
    binaryFrames: drop.binaryFrames,
    binaryBytes: drop.binaryBytes,
  });
  if (!marked) return;
  const replay = requestBarrierRepairFullFrame({
    workerFp,
    sessionId: drop.sessionId,
    channelId: drop.channelId,
  });
  noteBarrierRepairFullFrames(
    workerFp,
    drop.sessionId,
    drop.channelId,
    replay.enqueued,
  );
}

/** Every socket's barrier must report its drops into the coordinator-local
 *  repair state, so construction is centralized here: a bare
 *  `new AnnouncedChannelBarrier()` would silently lose the marks that force a
 *  dropped route's next full frame. */
export function createAnnouncedChannelBarrier(workerFp: string): AnnouncedChannelBarrier {
  return new AnnouncedChannelBarrier((drop) => handleAnnouncedDrop(workerFp, drop));
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
  let caller: { fingerprint: string; label?: string; keyGeneration: number };
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
  const worker = await deps.db.selectFrom("workers").select("fp")
    .where("fp", "=", fp).executeTakeFirst();
  if (!worker || jwtKeyGeneration(deps.jwtCache, fp) !== caller.keyGeneration) {
    return new Response("unauthorized", { status: 401 });
  }
  const data: WorkerWsData = {
    kind: "worker",
    caller,
    fp,
    conn: null,
    tail: Promise.resolve(),
    announcedChannels: createAnnouncedChannelBarrier(fp),
  };
  const ok = server.upgrade(req, { data });
  if (ok) return undefined; // hijacked
  return new Response("upgrade failed", { status: 400 });
}

export function makeWorkerWsHandler(deps: WorkerServiceDeps) {
  const sockets = new Set<ServerWebSocket<WorkerWsData>>();
  return {
    open(ws: ServerWebSocket<WorkerWsData>): void {
      if (jwtKeyGeneration(deps.jwtCache, ws.data.fp) !== ws.data.caller.keyGeneration) {
        ws.close(4001, "revoked");
        return;
      }
      sockets.add(ws);
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
      // Superseded generation: a newer authenticated hello for this fingerprint
      // already took over the registry handle, so this socket is closing and its
      // remaining frames — including a late exact snapshot — must not touch
      // coordinator state or its replacement's channel index.
      if (!conn.isCurrentGeneration()) {
        diag("worker-ws.superseded_frame", { worker_fp: ws.data.fp });
        return;
      }
      let frame;
      try {
        // COPY off Bun's ServerWebSocket message buffer — it is pooled and
        // recycled after this synchronous handler returns. protobuf fromBinary
        // returns a subarray VIEW for `bytes` fields (PtyOut terminal data), and
        // handleUpstream runs DEFERRED via the .tail microtask below — so the
        // view would outlive Bun's buffer and be dereferenced as freed memory:
        // the coord segfault whose fault address is terminal bytes (ESC[, session
        // ids). L11 borrowed-Bun-buffer-view class (feedback_bun_terminal_write_needs_copy).
        // `new Uint8Array(buf)` takes the typed-array→typed-array constructor
        // path (one memcpy). `Uint8Array.from(buf)` would take the ITERATOR
        // path — element-wise, with per-byte iterator-protocol overhead — on
        // every PTY chunk AND every cell frame, on coord's single thread.
        const bytes = typeof message === "string"
          ? new TextEncoder().encode(message)
          : new Uint8Array(message);
        frame = fromBinary(CoordWorkerUpSchema, bytes);
      } catch (e) {
        log.warn("worker-ws", "decode_failed", { worker_fp: ws.data.fp, error: String(e) });
        return;
      }
      const fcase = frame.frame.case;
      // `opened` AND `respawned` both bind a NEW (worker, channel) route whose
      // durable append is still queued. Recognizing them synchronously here is
      // what lets their first cell/binary frames wait for that binding instead
      // of racing it — a respawn's first binary frame can carry the only copy of
      // the new PTY's title/OSC mapping.
      let announced: { sessionId: string; channelId: number } | null = null;
      if (fcase === "event") {
        try {
          const event = protoToEvent(frame.frame.value.event as never);
          if (event?.kind === "opened" && event.worker_fp === ws.data.fp) {
            announced = {
              sessionId: event.session_id,
              channelId: Number(event.channel),
            };
          } else if (event?.kind === "respawned") {
            // `respawned` carries no worker_fp of its own. This socket is the
            // current authenticated handle for its fingerprint (guarded above),
            // and the commit below refuses to deliver unless the durable index
            // really bound (fp, new_channel) → session.
            announced = {
              sessionId: event.session_id,
              channelId: Number(event.new_channel),
            };
          }
          if (announced) {
            ws.data.announcedChannels.announce(
              announced.channelId,
              announced.sessionId,
            );
          }
        } catch {
          // The normal worker-conn decoder owns protocol diagnostics.
        }
      }
      // Fast path: in-memory terminal bus publishes — no DB write or ordering
      // constraint. A cell grid or PTY chunk for a synchronously announced
      // channel waits behind only that channel's durable append, and both lanes
      // share one buffer so their original arrival order survives the barrier.
      if (fcase === "cellGrid" || fcase === "binary") {
        const channelId = frame.frame.value.channelId;
        // Measuring a frame means encoding it, so only an actually announced
        // channel pays that: the steady-state PTY/cell lanes must not
        // re-serialize every chunk on the coordinator's single thread.
        if (ws.data.announcedChannels.isAnnounced(channelId)) {
          const queued = ws.data.announcedChannels.enqueue(
            channelId,
            frame,
            toBinary(CoordWorkerUpSchema, frame).byteLength,
          );
          if (queued !== "not-announced") return;
        }
        void conn.handleUpstream(frame).catch((e) => {
          log.warn("worker-ws", "handle_failed", { worker_fp: ws.data.fp, error: String(e) });
        });
        return;
      }
      // Slow path: event frames preserve appendEvent ordering.
      ws.data.tail = ws.data.tail
        .then(async () => {
          await conn.handleUpstream(frame);
          if (!announced) return;
          // Drain on this same socket lane, awaiting each buffered frame in
          // order: arrivals during the drain join the tail, so no later
          // fast-path frame can overtake the channel's first frames.
          await ws.data.announcedChannels.commit(
            announced.channelId,
            announced.sessionId,
            () => lookupSessionId(
              asWorkerFp(ws.data.fp),
              asChannelId(announced!.channelId),
            ) === announced!.sessionId,
            (buffered) => conn.handleUpstream(buffered),
          );
        })
        .catch((e) => {
          if (announced) ws.data.announcedChannels.fail(announced.channelId);
          // A throw = fatal (DB durability fault); tear down so the worker
          // reconnects + replays unacked.
          log.warn("worker-ws", "handle_failed", { worker_fp: ws.data.fp, error: String(e) });
          signal("event.append_failed", { error: String(e), phase: "ws_handle", cooldownKey: "events" });
          try { ws.close(); } catch { /* ignore */ }
        });
    },
    close(ws: ServerWebSocket<WorkerWsData>): void {
      sockets.delete(ws);
      ws.data.announcedChannels.clear();
      // A dead connection's repair marks would only strand overrides on routes
      // no keeper is producing; the returning worker's reconcile snapshot forces
      // a fresh full frame for every active owner instead.
      clearBarrierRepairForWorker(ws.data.fp);
      ws.data.conn?.close();
      ws.data.conn = null;
      log.info("worker-ws", "close", { worker_fp: ws.data.fp });
    },
    closeForFingerprint(fingerprint: string): void {
      for (const ws of sockets) {
        if (ws.data.fp === fingerprint) {
          try { ws.close(4001, "revoked"); } catch { /* close handler cleans up */ }
        }
      }
    },
  };
}
