// CoordWorkerUpstream / Downstream = bidir WSS protocol over a single
// outbound dial from worker to coord. Replaces today's split between
// `workers.*` tRPC mutations (heartbeat/register/emit) AND the inbound
// browser↔worker `/ws/worker/:fp` socket. Browser stops dialing worker
// entirely; coord routes browser commands as `browser-command` frames
// and fans worker upstream bytes/events back via tRPC subscriptions.
//
// Binary frame layout reused from `control.ts`: 2-byte BE channel_id +
// 1-byte direction (0=from-pty, 1=to-pty) + raw bytes. Upstream only
// carries DIR_FROM_PTY; downstream only carries DIR_TO_PTY.
//
// phase-24 design: `phase-24.md` at repo root. Migration order phase-24a
// → 24g, additive, no big-bang.

import { z } from "zod";
import { SessionId, WorkerFp, TraceId } from "./brand.ts";
import { ClientControlFrame } from "./control.ts";
import { SessionEvent } from "./event.ts";

/** The sole protocol marker accepted for worker WebSocket authentication.
 * The JWT follows as the second requested subprotocol and is never put in the
 * request URL; the coordinator echoes only this non-secret marker. */
export const WORKER_AUTH_SUBPROTOCOL = "roost-worker-auth";

const Base = z.object({ trace_id: TraceId.optional() });

// ─── worker → coord (upstream) ────────────────────────────────────────

export const CoordWorkerUpstream = z.discriminatedUnion("kind", [
  // first frame after WS open. Coord replies with `hello-ack`.
  Base.extend({
    kind: z.literal("hello"),
    worker_fp: WorkerFp,
    version: z.string(),
  }),
  // keepalive reply to downstream `ping`. coord uses RTT for liveness.
  Base.extend({
    kind: z.literal("pong"),
    ts: z.number().int().nonnegative(),
  }),
  // append-to-event-log delivery. Replaces the per-event tRPC mutation
  // `sessions.emit` on today's worker→coord HTTPS path. Coord runs
  // `appendEvent` in the SAME tx as today (R0.3).
  Base.extend({
    kind: z.literal("event"),
    event: SessionEvent,
  }),
  // RPC reply correlated by the coord-generated `request_id` from the
  // matching downstream `browser-command`. Coord looks up the origin
  // browser by `request_id` and routes the reply over the tRPC sub.
  Base.extend({
    kind: z.literal("rpc-ok"),
    request_id: z.string(),
    data: z.unknown(),
  }),
  Base.extend({
    kind: z.literal("rpc-error"),
    request_id: z.string(),
    message: z.string(),
  }),
]);
export type CoordWorkerUpstream = z.infer<typeof CoordWorkerUpstream>;

// ─── coord → worker (downstream) ──────────────────────────────────────

export const CoordWorkerDownstream = z.discriminatedUnion("kind", [
  // Immediate reply to `hello` and link-readiness barrier.
  Base.extend({
    kind: z.literal("hello-ack"),
  }),
  Base.extend({
    kind: z.literal("ping"),
    ts: z.number().int().nonnegative(),
  }),
  // wraps a browser-originated `ClientControlFrame` for execution at
  // the worker. `browser_id` + `viewer_id` are opaque to the worker
  // (it does NOT learn browser identity); they are present so future
  // multi-viewer presence carry-through is straightforward. Worker
  // MUST echo `request_id` in any `rpc-ok` / `rpc-error` it generates
  // so coord can route the reply back to the originating browser.
  Base.extend({
    kind: z.literal("browser-command"),
    browser_id: z.string(),
    viewer_id: z.string(),
    request_id: z.string(),
    frame: ClientControlFrame,
  }),
]);
export type CoordWorkerDownstream = z.infer<typeof CoordWorkerDownstream>;

// ─── binary frame header (PTY bytes) ─────────────────────────────────
// Same layout as `control.ts` browser↔worker binary frames so the parser
// migrates unchanged. Direction byte is technically redundant on this
// bidirectional WSS (upstream only ever 0, downstream only ever 1) but
// is kept for byte-format equivalence with the soon-to-be-deleted
// `ws/worker-direct.ts` parser, easing the cutover.
export {
  DIR_FROM_PTY,
  DIR_TO_PTY,
} from "./control.ts";
