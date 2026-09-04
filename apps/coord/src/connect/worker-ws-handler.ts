// Owns the live Bun worker WebSocket after authenticated upgrade: ordered
// frame admission, announced-channel barriers, and connection teardown.
// The copied decode buffer and queue ordering are required because Bun does
// not await message handlers and recycles each inbound message buffer.

import type { ServerWebSocket } from "bun";
import { fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, CoordWorkerDownSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type {
  CoordWorkerDown,
  CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import { jwtKeyGeneration, type Caller as VerifiedJwtCaller } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { diag, signal } from "@roost/shared/diag";
import { makeWorkerConn, type WorkerConn, type WorkerServiceDeps } from "./worker-service.ts";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { asChannelId, asWorkerFp } from "@roost/shared/wire";
import { lookupSessionId } from "../byte-hub.ts";
import {
  realWsDeadlineClock,
  scheduleWsAuthDeadline,
  type WsAuthDeadlineTimer,
  type WsDeadlineClock,
} from "./ws-auth-deadline.ts";
import { OrderedWorkerFrameQueue } from "./worker-frame-queue.ts";
import { fenceWorkerCredential } from "./worker-registry.ts";
import type { AnnouncedChannelBarrier } from "./announced-channel-barrier.ts";
export {
  createAnnouncedChannelBarrier,
  handleWorkerWsUpgrade,
} from "./worker-ws-upgrade.ts";

export const COORD_WEBSOCKET_MAX_PAYLOAD_BYTES = 4 * 1024 * 1024;
export const WORKER_DURABLE_EVENT_LIMIT = 600;
export const WORKER_DURABLE_EVENT_WINDOW_MS = 60_000;

export interface WorkerEventRateWindow {
  startedAtMs: number | null;
  events: number;
}

interface QueuedWorkerFrame {
  frame: CoordWorkerUp;
  announced: { sessionId: string; channelId: number } | null;
}

/** Fixed per-socket window: the first 600 durable event frames are admitted;
 * the next closes only their authenticated worker socket. */
export function admitWorkerDurableEvent(
  window: WorkerEventRateWindow,
  nowMs: number,
): boolean {
  if (
    window.startedAtMs === null
    || nowMs < window.startedAtMs
    || nowMs - window.startedAtMs >= WORKER_DURABLE_EVENT_WINDOW_MS
  ) {
    window.startedAtMs = nowMs;
    window.events = 0;
  }
  if (window.events >= WORKER_DURABLE_EVENT_LIMIT) return false;
  window.events += 1;
  return true;
}

export interface WorkerWsData {
  kind: "worker";
  caller: VerifiedJwtCaller;
  fp: string;
  /** Resolved from workers.dashboard_id before WS upgrade; never from a
   * URL, JWT claim, hello, or worker event payload. */
  dashboardId: string;
  authDeadlineAtMs: number | null;
  authDeadlineTimer: WsAuthDeadlineTimer | null;
  conn: WorkerConn | null;
  // Bun dispatches messages in order but does not await async handlers. This
  // explicit bounded queue keeps durable/control frames ordered and accounts
  // for the in-flight SQLite append until it settles.
  queue: OrderedWorkerFrameQueue<QueuedWorkerFrame> | null;
  eventRate: WorkerEventRateWindow;
  // Cell grids and PTY chunks may overtake the async durable opened/respawned
  // append, but only after this socket synchronously decoded that exact
  // worker/channel announcement.
  announcedChannels: AnnouncedChannelBarrier;
}

export interface WorkerWsHandlerOptions {
  deadlineClock?: WsDeadlineClock;
}

export function makeWorkerWsHandler(
  deps: WorkerServiceDeps,
  options: WorkerWsHandlerOptions = {},
) {
  const deadlineClock = options.deadlineClock ?? realWsDeadlineClock;
  const sockets = new Set<ServerWebSocket<WorkerWsData>>();
  const clearAuthDeadline = (ws: ServerWebSocket<WorkerWsData>): void => {
    if (ws.data.authDeadlineTimer?.current) {
      deadlineClock.clearTimeout(ws.data.authDeadlineTimer.current);
    }
    ws.data.authDeadlineTimer = null;
  };
  const armAuthDeadline = (
    ws: ServerWebSocket<WorkerWsData>,
    deadlineMs: number | null,
  ): boolean => {
    clearAuthDeadline(ws);
    ws.data.authDeadlineAtMs = deadlineMs;
    if (deadlineMs === null) return true;
    if (deadlineMs <= deadlineClock.now()) {
      ws.close(4003, "reauth required");
      return false;
    }
    ws.data.authDeadlineTimer = scheduleWsAuthDeadline(ws, deadlineMs, deadlineClock);
    return true;
  };
  const ensureQueue = (
    ws: ServerWebSocket<WorkerWsData>,
    conn: WorkerConn,
  ): OrderedWorkerFrameQueue<QueuedWorkerFrame> => {
    if (ws.data.queue) return ws.data.queue;
    const queue = new OrderedWorkerFrameQueue<QueuedWorkerFrame>(
      async ({ frame, announced }) => {
        await conn.handleUpstream(frame);
        if (!announced) return;
        // Drain on this same socket lane, awaiting each buffered frame in
        // order. Arrivals during the drain stay behind the durable route.
        await ws.data.announcedChannels.commit(
          announced.channelId,
          announced.sessionId,
          () => lookupSessionId(
            asWorkerFp(ws.data.fp),
            asChannelId(announced.channelId),
          ) === announced.sessionId,
          (buffered) => conn.handleUpstream(buffered),
        );
      },
      (error, queued) => {
        if (queued.announced) {
          ws.data.announcedChannels.fail(queued.announced.channelId);
        }
        // A throw = fatal (usually a DB durability fault); tear down only this
        // worker socket so it reconnects and replays unacknowledged events.
        log.warn("worker-ws", "handle_failed", {
          worker_fp: ws.data.fp,
          error: String(error),
        });
        signal("event.append_failed", {
          error: String(error),
          phase: "ws_handle",
          cooldownKey: "events",
        });
        try { ws.close(); } catch { /* ignore */ }
      },
      ({ frames, bytes, rejectedBytes }) => {
        log.warn("worker-ws", "queue_overflow", {
          worker_fp: ws.data.fp,
          frames,
          bytes,
          rejected_bytes: rejectedBytes,
        });
        signal("worker.queue_overflow", {
          worker_fp: ws.data.fp,
          frames,
          bytes,
          rejected_bytes: rejectedBytes,
          cooldownKey: ws.data.fp,
        });
        // The queue latches closed before this callback and never retained the
        // offending frame. 1009 is the WebSocket payload-cap close code.
        try { ws.close(1009, "worker queue overflow"); } catch { /* ignore */ }
      },
    );
    ws.data.announcedChannels.bindRetainedWorkBudget(queue.retainedWorkBudget);
    ws.data.queue = queue;
    return queue;
  };
  return {
    maxPayloadLength: COORD_WEBSOCKET_MAX_PAYLOAD_BYTES,
    open(ws: ServerWebSocket<WorkerWsData>): void {
      if (jwtKeyGeneration(deps.jwtCache, ws.data.fp) !== ws.data.caller.keyGeneration) {
        ws.close(4001, "revoked");
        return;
      }
      if (!armAuthDeadline(ws, ws.data.authDeadlineAtMs)) return;
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
      const conn = makeWorkerConn(
        deps,
        ws.data.caller,
        send,
        requestClose,
        () => ws.getBufferedAmount(),
        ws.data.dashboardId,
        (refreshed) => {
          ws.data.caller = refreshed;
          if (deps.cfg.saasMode) {
            armAuthDeadline(ws, refreshed.validUntilMs);
          }
        },
      );
      ws.data.conn = conn;
      ensureQueue(ws, conn);
      log.info("worker-ws", "open", { worker_fp: ws.data.fp });
    },
    message(ws: ServerWebSocket<WorkerWsData>, message: string | Buffer): void {
      const conn = ws.data.conn;
      if (!conn) return;
      // Superseded generations are fenced before queue lookup so a late frame
      // cannot recreate resources detached by fenceForFingerprint.
      if (!conn.isCurrentGeneration()) {
        diag("worker-ws.superseded_frame", { worker_fp: ws.data.fp });
        return;
      }
      const queue = ensureQueue(ws, conn);
      if (!queue.isOpen()) return;
      let frame: CoordWorkerUp;
      let frameBytes = 0;
      try {
        // COPY off Bun's ServerWebSocket message buffer — it is pooled and
        // recycled after this synchronous handler returns. protobuf fromBinary
        // returns a subarray VIEW for `bytes` fields (PtyOut terminal data), and
        // handleUpstream runs later on the explicit async queue, so the view
        // would outlive Bun's buffer and be dereferenced as freed memory:
        // the coord segfault whose fault address is terminal bytes (ESC[, session
        // ids). L11 borrowed-Bun-buffer-view class (feedback_bun_terminal_write_needs_copy).
        // `new Uint8Array(buf)` takes the typed-array→typed-array constructor
        // path (one memcpy). `Uint8Array.from(buf)` would take the ITERATOR
        // path — element-wise, with per-byte iterator-protocol overhead — on
        // every PTY chunk AND every cell frame, on coord's single thread.
        const bytes = typeof message === "string"
          ? new TextEncoder().encode(message)
          : new Uint8Array(message);
        frameBytes = bytes.byteLength;
        frame = fromBinary(CoordWorkerUpSchema, bytes);
      } catch (e) {
        log.warn("worker-ws", "decode_failed", { worker_fp: ws.data.fp, error: String(e) });
        return;
      }
      const fcase = frame.frame.case;
      // The raw socket exists before the worker's exact snapshot barrier. Keep
      // liveness and durable lifecycle replay flowing, but do not let ordinary
      // frames bypass the ordered connection-level readiness gate through the
      // cell/binary fast path.
      if (
        !conn.isReady()
        && fcase !== "hello"
        && fcase !== "event"
        && fcase !== "pong"
        && fcase !== "refreshJwt"
      ) {
        diag("worker-ws.frame_before_snapshot_ready", {
          worker_fp: ws.data.fp,
          frame: fcase,
        });
        return;
      }
      if (
        fcase === "event"
        && !admitWorkerDurableEvent(ws.data.eventRate, deadlineClock.now())
      ) {
        queue.close();
        ws.data.announcedChannels.clear();
        log.warn("worker-ws", "event_rate_exceeded", {
          worker_fp: ws.data.fp,
          limit: WORKER_DURABLE_EVENT_LIMIT,
          window_ms: WORKER_DURABLE_EVENT_WINDOW_MS,
        });
        signal("worker.event_rate_exceeded", {
          worker_fp: ws.data.fp,
          limit: WORKER_DURABLE_EVENT_LIMIT,
          cooldownKey: ws.data.fp,
        });
        try { ws.close(1008, "worker event rate exceeded"); } catch { /* ignore */ }
        return;
      }
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
      if (fcase === "cellGrid" || fcase === "cellGridChunk" || fcase === "binary") {
        const channelId = frame.frame.value.channelId;
        // Reuse the copied wire length; never re-serialize PTY/cell hot-path
        // frames just to account for a short announcement barrier.
        if (ws.data.announcedChannels.isAnnounced(channelId)) {
          const queued = ws.data.announcedChannels.enqueue(
            channelId,
            frame,
            frameBytes,
          );
          if (queued !== "not-announced") return;
        }
        void conn.handleUpstream(frame).catch((e) => {
          log.warn("worker-ws", "handle_failed", { worker_fp: ws.data.fp, error: String(e) });
        });
        return;
      }
      // Slow path: one explicit bounded per-socket queue preserves event and
      // control-frame order. Its accounting includes the in-flight append.
      queue.enqueue({ frame, announced }, frameBytes);
    },
    close(ws: ServerWebSocket<WorkerWsData>): void {
      clearAuthDeadline(ws);
      sockets.delete(ws);
      ws.data.queue?.close();
      ws.data.queue = null;
      ws.data.announcedChannels.clear();
      ws.data.conn?.close();
      ws.data.conn = null;
      log.info("worker-ws", "close", { worker_fp: ws.data.fp });
    },
    /** Synchronous post-commit credential fence. Revoke every admitted socket
     * generation first, then detach all queued/announced inbound work, then
     * unregister the authoritative generation. Socket close is deliberately
     * deferred to the later best-effort cleanup phase. */
    fenceForFingerprint(fingerprint: string): void {
      const matching = [...sockets].filter((ws) => ws.data.fp === fingerprint);
      for (const ws of matching) ws.data.conn?.revoke();
      for (const ws of matching) {
        ws.data.queue?.close();
        ws.data.queue = null;
        ws.data.announcedChannels.clear();
      }
      fenceWorkerCredential(fingerprint);
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
