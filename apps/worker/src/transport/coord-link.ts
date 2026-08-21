// Worker → coord OUTBOUND transport: a long-lived raw Bun WebSocket dial
// to /ws/coord-worker/<fp>?token=<jwt>. (Briefly rewritten as a Connect
// HTTP/2 bidi against WorkerService.Attach during crpc5, then reverted —
// Bun can't hold a Connect bidi; see docs/FAILURE-INDEX.md.)
//
// Lifecycle FSM:
//   idle → connecting → open → reconnecting → connecting → ...
//   any → closed (dispose)
//
// Reconnect: exponential backoff, 500ms → 30s cap, multiplier 2.
// JWT refresh: re-create transport on every dial. If a connection
// survives past TTL we proactively close + reconnect at exp-T-30s.
//
// Frame schemas: `@roost/shared/proto/worker_transport_pb` (proto).
// Binary PTY bytes still flow on the same stream as WBinary frames.
//
// This file is the composer. The three engines it wires together are the
// encoded outbox + backpressure lanes (coord-link-outbox.ts, which owns the
// at-least-once event ledger in coord-link-unacked.ts), the reconnect ladder
// (coord-link-reconnect.ts) and the downstream frame dispatch
// (coord-link-downstream.ts). What stays here is the socket lifecycle: dial,
// hello, the stale-link watchdog, in-band JWT refresh, relocate and dispose.

import { create, fromBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, CoordWorkerDownSchema, WHelloSchema, WRefreshJwtSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { createCoordLinkOutbox } from "./coord-link-outbox.ts";
import { createCoordLinkReconnect } from "./coord-link-reconnect.ts";
import { createCoordLinkDownstream } from "./coord-link-downstream.ts";
import {
  STABLE_SESSION_MS,
  STALE_LINK_TIMEOUT_MS, STALE_CHECK_INTERVAL_MS,
} from "./coord-link-constants.ts";
import type {
  CoordLinkDeps, CoordLink, CoordLinkState,
} from "./coord-link-types.ts";
export type {
  CoordLinkDeps, CoordLink, TransportSendResult, TerminalRequestBudget,
} from "./coord-link-types.ts";

// ─── implementation ──────────────────────────────────────────────────

export function startCoordLink(deps: CoordLinkDeps): CoordLink {
  const ttlSecs = deps.jwtTtlSecs ?? 300;
  let coordHttpUrl = deps.coordHttpUrl;
  let relocating = false;
  let state: CoordLinkState = { kind: "idle" };
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  let closeStream: (() => void) | null = null;

  const outbox = createCoordLinkOutbox(deps, () => disposed);
  const downstream = createCoordLinkDownstream(deps, outbox);
  const reconnect = createCoordLinkReconnect({
    isDisposed: () => disposed,
    setState: (next) => { setState(next); },
    dial: () => { void dial(); },
  });

  function setState(next: CoordLinkState): void {
    const from = state.kind;
    const to = next.kind;
    state = next;
    log.debug("coord-link", "state", { kind: to });
    diag("transport.state", { from, to });
  }

  function clearRefreshTimer(): void {
    if (refreshTimer !== null) { clearTimeout(refreshTimer); refreshTimer = null; }
  }

  function scheduleRefresh(): void {
    if (disposed) return;
    clearRefreshTimer();
    const refreshInMs = Math.max(1_000, (ttlSecs - 30) * 1000);
    refreshTimer = setTimeout(async () => {
      if (!outbox.isAttached()) return;
      try {
        const jwt = await deps.mintJwt();
        const result = outbox.sendControlProto(create(CoordWorkerUpSchema, {
          frame: { case: "refreshJwt", value: create(WRefreshJwtSchema, { jwt }) },
        }));
        if (result === "dropped") throw new Error("refresh frame outbox full");
        log.debug("coord-link", "jwt_refreshed_inband", { result });
        scheduleRefresh();
      } catch (error) {
        log.warn("coord-link", "jwt_refresh_inband_failed", {
          error: error instanceof Error ? error.message : String(error),
        });
        try { closeStream?.(); } catch { /* ignore */ }
      }
    }, refreshInMs);
  }

  async function dial(): Promise<void> {
    if (disposed) return;
    setState({ kind: "connecting", attempt: reconnect.beginDial() });
    let jwt: string;
    try { jwt = await deps.mintJwt(); }
    catch (err) {
      log.warn("coord-link", "mint_jwt_failed", { error: (err as Error).message });
      signal("auth.jwt_sign_fail", { stage: "mint", cooldownKey: "jwt" });
      reconnect.scheduleReconnect();
      return;
    }

    // Raw WebSocket transport (Bun-native, full-duplex). The crpc5
    // Connect-bidi client can't hold a stable stream under Bun: h2 is
    // unsupported (Bun's node:http2 is incomplete) and h1.1 buffers the
    // upstream so the worker's rpc-ok replies stalled → sessionsSpawn hung.
    // Same CoordWorkerUp/Down proto frames, carried as BINARY WS messages.
    // Auth: query-param JWT (Bun's client WebSocket has no custom-header API).
    const wsBase = coordHttpUrl.replace(/^http/, "ws");
    const url = `${wsBase}/ws/coord-worker/${deps.workerFp}?token=${encodeURIComponent(jwt)}`;
    let ws: WebSocket;
    try {
      ws = deps.webSocketFactory?.(url) ?? new WebSocket(url);
    } catch (err) {
      log.warn("coord-link", "ws_construct_failed", { error: (err as Error).message });
      signal("auth.jwt_sign_fail", { stage: "ws_construct", cooldownKey: "jwt" });
      reconnect.scheduleReconnect();
      return;
    }
    ws.binaryType = "arraybuffer";

    const openedAt = Date.now();
    let countersReset = false;
    let cleanedUp = false;
    let lastDownstreamAtMs = Date.now();
    let staleTimer: NodeJS.Timeout | null = null;
    let dialReconnected = false;

    // `writer` stays null until OPEN so sends enter the bounded encoded
    // outbox/unacked lanes rather than touching a connecting socket.
    // dispose() / jwt-refresh-failure can tear down a connecting socket.
    closeStream = () => { try { ws.close(); } catch { /* ignore */ } };

    // Fires exactly once per dial (onerror→onclose both call it).
    const cleanup = (): void => {
      if (cleanedUp) return;
      cleanedUp = true;
      clearRefreshTimer();
      outbox.clearDrainTimer();
      if (staleTimer !== null) { clearInterval(staleTimer); staleTimer = null; }
      outbox.detachSocket();
      closeStream = null;
      reconnect.noteDialClosed();
      if (!disposed) {
        if (relocating) {
          relocating = false;
          void dial();
        } else {
          reconnect.scheduleReconnect();
        }
      }
    };

    ws.onopen = () => {
      if (disposed) { try { ws.close(); } catch { /* ignore */ } return; }
      setState({ kind: "open", since: openedAt });
      dialReconnected = reconnect.noteOpen();
      outbox.attachSocket(ws, (bytes: Uint8Array): void => {
        const buffer = bytes.buffer;
        if (buffer instanceof ArrayBuffer) {
          ws.send(
            bytes.byteOffset === 0 && bytes.byteLength === buffer.byteLength
              ? buffer
              : new Uint8Array(buffer, bytes.byteOffset, bytes.byteLength),
          );
          return;
        }
        // BufferSource excludes SharedArrayBuffer-backed views in lib.dom.
        // Copy only at that boundary; protobuf's normal ArrayBuffer stays zero-copy.
        ws.send(Uint8Array.from(bytes));
      });
      scheduleRefresh();
      // Stale-link watchdog: coord pings every 30s; if the socket goes silent
      // past the timeout the backend is gone even though TCP looks alive
      // (tailscale-serve zombie). Force-close → cleanup → scheduleReconnect.
      lastDownstreamAtMs = Date.now();
      const staleTimeoutMs = deps.staleLinkTimeoutMs ?? STALE_LINK_TIMEOUT_MS;
      staleTimer = setInterval(() => {
        if (cleanedUp) return;
        const silentMs = Date.now() - lastDownstreamAtMs;
        if (silentMs < staleTimeoutMs) return;
        log.warn("coord-link", "link_stale_no_downstream", { silent_ms: silentMs });
        try { ws.close(); } catch { /* ignore */ }
        cleanup();
      }, deps.staleCheckIntervalMs ?? STALE_CHECK_INTERVAL_MS);
      try {
        const hello = outbox.encodeUpstream(create(CoordWorkerUpSchema, {
          frame: { case: "hello", value: create(WHelloSchema, {
            workerFp: deps.workerFp,
            version: deps.workerVersion,
          }) },
        }));
        if (!hello) throw new Error("hello encode failed");
        // The socket has just opened, so its native buffer is empty. Hello is
        // the sole forced write; every application frame uses byte admission.
        if (!outbox.forceWrite(hello)) throw new Error("hello encode failed");
        outbox.replayUnacked();
        // Events and controls may follow hello immediately. Raw metadata stays
        // held until helloAck so authoritative cell repairs can lead it.
        outbox.drainQueues();
        try {
          deps.onOpen?.(dialReconnected);
        } catch (error) {
          log.warn("coord-link", "on_open_failed", {
            error: error instanceof Error ? error.message : String(error),
          });
        }
      } catch (error) {
        log.warn("coord-link", "flush_failed", {
          error: error instanceof Error ? error.message : String(error),
        });
        try { ws.close(); } catch { /* ignore */ }
      }
    };

    ws.onmessage = (ev: MessageEvent) => {
      lastDownstreamAtMs = Date.now();
      // Reset dial counters only once the session is demonstrably useful:
      // ≥1 frame received AND ≥STABLE_SESSION_MS uptime — distinguishes a
      // healthy long session from a flap.
      if (!countersReset && Date.now() - openedAt >= STABLE_SESSION_MS) {
        countersReset = true;
        reconnect.noteStableSession();
      }
      let frame: CoordWorkerDown;
      try {
        const d = ev.data;
        const bytes = d instanceof ArrayBuffer ? new Uint8Array(d)
          : d instanceof Uint8Array ? d : null;
        if (!bytes) { log.warn("coord-link", "downstream_non_binary", {}); return; }
        frame = fromBinary(CoordWorkerDownSchema, bytes);
      } catch (err) {
        log.warn("coord-link", "downstream_decode_failed", { error: (err as Error).message });
        return;
      }
      downstream.handleDownstream(frame, dialReconnected, ws);
    };

    ws.onerror = () => { log.warn("coord-link", "stream_error", { error: "ws error" }); cleanup(); };
    ws.onclose = () => { cleanup(); };
  }

  function relocate(targetUrl: string, force = false): void {
    if (disposed || (!force && coordHttpUrl === targetUrl)) return;
    // cleanup() nulls closeStream, so during reconnect backoff the else-branch
    // below always fires — while the pending backoff timer fires its OWN dial.
    // Two live sockets both install ws.onmessage → handleDownstream, so every
    // browser command, PTY input byte and coordRelocate frame executes twice
    // (doubled characters in the terminal). Cancel the pending dial first.
    reconnect.cancelPendingDial();
    coordHttpUrl = targetUrl;
    reconnect.resetForRedial();
    relocating = true;
    if (closeStream) {
      closeStream();
    } else {
      relocating = false;
      void dial();
    }
  }

  function dispose(): void {
    disposed = true;
    reconnect.cancelPendingDial();
    clearRefreshTimer();
    outbox.clearDrainTimer();
    outbox.reset();
    try { closeStream?.(); } catch { /* ignore */ }
    setState({ kind: "closed" });
  }

  void dial();
  return {
    send: outbox.send,
    sendBinary: outbox.sendBinary,
    sendCellGrid: outbox.sendCellGrid,
    sendCellGridChunk: outbox.sendCellGridChunk,
    sendAgentStatus: outbox.sendAgentStatus,
    state: () => state,
    relocate,
    unackedEventCount: outbox.unackedCount,
    dispose,
  };
}
