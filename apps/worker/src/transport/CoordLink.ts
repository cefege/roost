// Worker → coord OUTBOUND transport: a long-lived raw Bun WebSocket dial
// to /ws/coord-worker/<fp>?token=<jwt>. (Briefly rewritten as a Connect
// HTTP/2 bidi against WorkerService.Attach during crpc5, then reverted —
// Bun can't hold a Connect bidi; see CLAUDE.md L11.)
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

import { create, toBinary, fromBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, CoordWorkerDownSchema, WHelloSchema,
  WRefreshJwtSchema, WSessionEventSchema, WCellGridSchema, WAgentStatusSchema,
  TerminalInputStatus, TerminalViewportStatus, TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { ClientSeq } from "./client-seq.ts";
import type {
  CoordWorkerUp,
  CoordWorkerDown,
  DInputRequest,
  DViewportRequest,
} from "@roost/shared/proto/worker_transport_pb";
import type { AgentStatusUpdate, ClientControlFrame, SessionEvent } from "@roost/shared/wire";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { log, diag, signal } from "@roost/shared";
import { frameToProto, binaryFrameToProto } from "./CoordLink-codec.ts";
import {
  BACKOFF_INITIAL_MS, BACKOFF_MULTIPLIER,
  PENDING_CAP, PENDING_BYTES_CAP, RAW_METADATA_MAX_AGE_MS,
  WS_BUFFERED_HIGH_WATER_BYTES, WS_DRAIN_RETRY_MS,
  STABLE_SESSION_MS, UNACKED_CAP,
  STALE_LINK_TIMEOUT_MS, STALE_CHECK_INTERVAL_MS,
  AUTH_REJECT_THRESHOLD, AUTH_REJECT_BACKOFF_CAP_MS,
  AUTH_REJECT_THRESHOLD_AFTER_OPEN, backoffCapMs,
  INPUT_REQUEST_INFLIGHT_CAP, VIEWPORT_REQUEST_INFLIGHT_CAP,
  TERMINAL_REQUEST_BUDGET_CAP_MS,
} from "./CoordLink-constants.ts";
import type {
  CoordLinkDeps, CoordLink, UpstreamFrame, CoordLinkState,
  TransportSendResult, TerminalRequestBudget,
} from "./CoordLink-types.ts";
export type {
  CoordLinkDeps, CoordLink, TransportSendResult, TerminalRequestBudget,
} from "./CoordLink-types.ts";

// ─── implementation ──────────────────────────────────────────────────

// Consecutive dial failures since the last successful open. Reset to 0
// in ws.onopen; a worker is a daemon so we NEVER stop reconnecting —
// crossing the ceiling only fires an observability signal (once, then
// cooldown-gated) so a wedged worker is visible in `roost doctor`.
const RECONNECT_GIVE_UP_AFTER = 10;
let _reconnectFailures = 0;
interface EncodedPending {
  bytes: Uint8Array;
  queuedAtMs: number;
  kind: "control" | "raw";
}

export function startCoordLink(deps: CoordLinkDeps): CoordLink {
  const ttlSecs = deps.jwtTtlSecs ?? 300;
  let coordHttpUrl = deps.coordHttpUrl;
  let relocating = false;
  let state: CoordLinkState = { kind: "idle" };
  let backoffMs = BACKOFF_INITIAL_MS;
  // Monotonic dial counter. Stamped onto every `connecting` state
  // transition so consumers of state().attempt (telemetry, health UI)
  // can distinguish a wedged worker (attempt: 47) from a healthy one
  // (attempt: 1). Reset to 0 once a stream successfully opens; we
  // pre-increment so the first attempt reports attempt: 1.
  let dialAttempt = 0;
  // Consecutive dials that failed without ws.onopen firing. Not necessarily
  // auth: a 401 upgrade, a handshake timeout and a proxy 502 look identical
  // from the close event. backoffCapMs() decides when a streak escalates.
  let _authRejectCount = 0;
  let _didOpen = false;
  // Persists across dials so callers can reconcile only after a true reopen,
  // not race normal startup with a duplicate snapshot request.
  let hasOpened = false;
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  // Pending backoff dial. Held so relocate()/dispose() can cancel it — an
  // uncancelled timer means a second concurrent socket.
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  // `writer` accepts already-encoded bytes. Admission/backpressure checks live
  // in tryWriteEncoded(), so every queued byte is counted exactly once.
  let writer: ((bytes: Uint8Array) => void) | null = null;
  let activeWs: WebSocket | null = null;
  let linkReady = false;
  let closeStream: (() => void) | null = null;
  let drainTimer: NodeJS.Timeout | null = null;
  let pendingFrameCount = 0;
  let pendingEncodedBytes = 0;
  const controlPending: EncodedPending[] = [];
  const rawPending: EncodedPending[] = [];
  let writableNotificationPending = false;
  let notifyingWritable = false;
  // Downstream terminal-control admission, counted per kind. A viewport RPC
  // parked on a keeper resize therefore cannot spend the slots PTY input
  // needs, and neither lane can starve the other.
  let inputRequestsInFlight = 0;
  let viewportRequestsInFlight = 0;
  // D-4b at-least-once WITHIN A WORKER PROCESS.
  // clientSeq is fsynced (client-seq.ts) and survives restart so coord's
  // dedup key stays stable across reboots. `unacked` is in-memory only:
  // on reconnect inside the same process, every still-unacked entry
  // replays at the head of the new outbox and coord dedups via UNIQUE
  // INDEX (worker_fp, client_seq). On worker process crash, in-flight
  // unacked events are LOST — clientSeq still advances on the next emit
  // so there's no seq collision, but the worker has no record to replay.
  // Persisting `unacked` would mean an fsync per emit; not worth it
  // until we see real loss in practice.
  const snapshotRequestIds = new Map<string, string>();
  const clientSeq = new ClientSeq();
  const unacked = new Map<number, SessionEvent>();
  // Events stay in `unacked` until the coordinator ACKs them. This separate
  // set tracks which entries have not yet entered the current native socket;
  // it prevents a bufferedAmount stall from replaying every already-sent event.
  const unsentEventSeqs = new Set<number>();

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

  function clearDrainTimer(): void {
    if (drainTimer !== null) { clearTimeout(drainTimer); drainTimer = null; }
  }

  function encodeUpstream(frame: CoordWorkerUp): Uint8Array | null {
    try {
      return toBinary(CoordWorkerUpSchema, frame);
    } catch (error) {
      log.warn("coord-link", "upstream_encode_failed", {
        error: error instanceof Error ? error.message : String(error),
      });
      return null;
    }
  }

  function nativeHasCapacity(byteLength: number): boolean {
    if (
      !writer ||
      !activeWs ||
      activeWs.readyState !== WebSocket.OPEN ||
      byteLength > PENDING_BYTES_CAP
    ) return false;
    const buffered = activeWs.bufferedAmount;
    // Permit one large (but bounded) frame when the native queue is empty.
    // Otherwise stop before crossing the high-water mark.
    return buffered === 0
      ? byteLength <= PENDING_BYTES_CAP
      : buffered + byteLength <= WS_BUFFERED_HIGH_WATER_BYTES;
  }

  function tryWriteEncoded(bytes: Uint8Array): boolean {
    if (!nativeHasCapacity(bytes.byteLength) || !writer) return false;
    try {
      writer(bytes);
      return true;
    } catch (error) {
      diag("transport.frame_dropped", {
        reason: "writer_throw",
        kind: "encoded",
        bytes: bytes.byteLength,
      });
      log.warn("coord-link", "writer_throw", {
        error: error instanceof Error ? error.message : String(error),
      });
      return false;
    }
  }

  function scheduleDrain(): void {
    if (disposed || !writer || drainTimer !== null) return;
    drainTimer = setTimeout(drainQueues, WS_DRAIN_RETRY_MS);
  }

  function enqueueEncoded(kind: EncodedPending["kind"], bytes: Uint8Array): boolean {
    if (
      pendingFrameCount >= PENDING_CAP ||
      pendingEncodedBytes + bytes.byteLength > PENDING_BYTES_CAP
    ) {
      diag("transport.frame_dropped", {
        reason: pendingFrameCount >= PENDING_CAP ? "pending_frame_overflow" : "pending_byte_overflow",
        kind,
        frames: pendingFrameCount,
        bytes: pendingEncodedBytes,
        frame_bytes: bytes.byteLength,
      });
      return false;
    }
    const item: EncodedPending = { bytes, queuedAtMs: Date.now(), kind };
    (kind === "raw" ? rawPending : controlPending).push(item);
    pendingFrameCount += 1;
    pendingEncodedBytes += bytes.byteLength;
    scheduleDrain();
    return true;
  }

  function removePendingHead(queue: EncodedPending[]): EncodedPending | undefined {
    const item = queue.shift();
    if (!item) return undefined;
    pendingFrameCount -= 1;
    pendingEncodedBytes -= item.bytes.byteLength;
    return item;
  }

  function rawMetadataAged(now = Date.now()): boolean {
    const oldest = rawPending[0];
    return oldest !== undefined && now - oldest.queuedAtMs >= RAW_METADATA_MAX_AGE_MS;
  }

  /** Wrap a SessionEvent + client_seq into a WSessionEvent frame.
   * Caller is responsible for adding to unacked beforehand. */
  function encodeEventFrame(event: SessionEvent, seq: number): CoordWorkerUp | null {
    const proto = eventToProto(event, 0);
    if (!proto) {
      log.warn("coord-link", "event_proto_encode_returned_null", { kind: event.kind });
      return null;
    }
    return create(CoordWorkerUpSchema, {
      frame: { case: "event", value: create(WSessionEventSchema, {
        event: proto,
        clientSeq: BigInt(seq),
      })},
    });
  }

  function drainUnsentEvents(): void {
    for (const seq of unsentEventSeqs) {
      const event = unacked.get(seq);
      if (!event) {
        unsentEventSeqs.delete(seq);
        continue;
      }
      const proto = encodeEventFrame(event, seq);
      const bytes = proto ? encodeUpstream(proto) : null;
      if (!bytes) {
        // An unencodable event can never become sendable on reconnect.
        unsentEventSeqs.delete(seq);
        unacked.delete(seq);
        signal("transport.event_drop", { dropped_seq: seq, reason: "encode", cooldownKey: "outbox" });
        continue;
      }
      if (!tryWriteEncoded(bytes)) return;
      unsentEventSeqs.delete(seq);
    }
  }

  function drainControls(): void {
    while (controlPending.length > 0) {
      const item = controlPending[0]!;
      if (!tryWriteEncoded(item.bytes)) return;
      removePendingHead(controlPending);
    }
  }

  function drainOneRaw(): boolean {
    const item = rawPending[0];
    if (!item || !tryWriteEncoded(item.bytes)) return false;
    removePendingHead(rawPending);
    return true;
  }

  function maybeNotifyWritable(): void {
    if (
      !writableNotificationPending ||
      notifyingWritable ||
      !linkReady ||
      unsentEventSeqs.size > 0 ||
      !nativeHasCapacity(0)
    ) return;
    writableNotificationPending = false;
    notifyingWritable = true;
    try {
      deps.onWritable?.();
    } catch (error) {
      log.warn("coord-link", "on_writable_failed", {
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      notifyingWritable = false;
    }
  }

  function drainQueues(): void {
    clearDrainTimer();
    if (disposed || !writer) return;
    // Durable/control chronology always fences cells and raw metadata. This is
    // what preserves opened -> first full when native buffering is saturated.
    drainUnsentEvents();
    if (unsentEventSeqs.size > 0) {
      scheduleDrain();
      return;
    }
    // A cell that was dropped behind an earlier durable event is repaired
    // before later RPC/control replies. This preserves opened -> first full ->
    // spawn result even when the native socket was saturated at opened.
    if (linkReady) {
      maybeNotifyWritable();
      if (writableNotificationPending) {
        scheduleDrain();
        return;
      }
    }
    drainControls();
    if (controlPending.length > 0) {
      scheduleDrain();
      return;
    }
    // Raw frames are held until helloAck. Authoritative repairs above lead the
    // reconnect backlog.
    if (!linkReady) return;
    if (rawMetadataAged()) drainOneRaw();
    while (rawPending.length > 0 && drainOneRaw()) { /* FIFO */ }
    if (rawPending.length > 0 || writableNotificationPending) scheduleDrain();
  }

  function sendControlProto(frame: CoordWorkerUp): TransportSendResult {
    const bytes = encodeUpstream(frame);
    if (!bytes) return "dropped";
    if (
      unsentEventSeqs.size === 0 &&
      controlPending.length === 0 &&
      tryWriteEncoded(bytes)
    ) return "sent";
    return enqueueEncoded("control", bytes) ? "queued" : "dropped";
  }

  function scheduleRefresh(): void {
    if (disposed) return;
    clearRefreshTimer();
    const refreshInMs = Math.max(1_000, (ttlSecs - 30) * 1000);
    refreshTimer = setTimeout(async () => {
      if (!writer) return;
      try {
        const jwt = await deps.mintJwt();
        const result = sendControlProto(create(CoordWorkerUpSchema, {
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

  /** Owns D-4b bookkeeping for SessionEvents. Events live in `unacked`
   * until acked; unsentEventSeqs records native-socket admission. */
  function sendEvent(event: SessionEvent): boolean {
    if (disposed) return false;
    if (unacked.size >= UNACKED_CAP) {
      const oldest = unacked.keys().next().value;
      if (oldest !== undefined) {
        unacked.delete(oldest);
        unsentEventSeqs.delete(oldest);
      }
      log.error("coord-link", "unacked_overflow_drop", { cap: UNACKED_CAP, dropped_seq: oldest });
      signal("transport.event_drop", { dropped_seq: oldest, unacked_size: unacked.size, cooldownKey: "outbox" });
    }
    const seq = clientSeq.next();
    unacked.set(seq, event);
    unsentEventSeqs.add(seq);
    if (writer) drainQueues();
    return !unsentEventSeqs.has(seq);
  }

  function send(frame: UpstreamFrame): boolean {
    if (disposed) return false;
    if (frame.kind === "event") return sendEvent(frame.event);
    const proto = frameToProto(frame);
    return proto ? sendControlProto(proto) === "sent" : false;
  }

  function sendBinary(
    channelId: number,
    direction: number,
    endSeq: number,
    data: Uint8Array,
  ): TransportSendResult {
    if (disposed) return "dropped";
    const bytes = encodeUpstream(binaryFrameToProto(channelId, direction, endSeq, data));
    if (!bytes) return "dropped";
    if (
      linkReady &&
      unsentEventSeqs.size === 0 &&
      controlPending.length === 0 &&
      rawPending.length === 0 &&
      tryWriteEncoded(bytes)
    ) return "sent";
    return enqueueEncoded("raw", bytes) ? "queued" : "dropped";
  }

  function sendCellGrid(channelId: number, frame: PbCellGridFrame): TransportSendResult {
    if (disposed) return "dropped";
    if (
      !linkReady ||
      !writer ||
      unsentEventSeqs.size > 0 ||
      (controlPending.length > 0 && !notifyingWritable)
    ) {
      writableNotificationPending = true;
      scheduleDrain();
      return "dropped";
    }
    // Cells normally lead raw metadata. Once raw has waited 100 ms, admit one
    // metadata frame before an ordinary delta so parser input cannot starve.
    // Full repairs always lead reconnect backlog.
    if (!frame.full && rawMetadataAged() && !drainOneRaw()) {
      writableNotificationPending = true;
      scheduleDrain();
      return "dropped";
    }
    const bytes = encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "cellGrid", value: create(WCellGridSchema, { channelId, frame }) },
    }));
    if (bytes && tryWriteEncoded(bytes)) return "sent";
    writableNotificationPending = true;
    scheduleDrain();
    diag("transport.frame_dropped", {
      reason: bytes ? "native_backpressure" : "encode",
      kind: "cellGrid",
      channel_id: channelId,
    });
    return "dropped";
  }

  function sendAgentStatus(status: AgentStatusUpdate): boolean {
    if (
      disposed ||
      !writer ||
      unsentEventSeqs.size > 0 ||
      controlPending.length > 0
    ) return false;
    const bytes = encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "agentStatus", value: create(WAgentStatusSchema, {
        sessionId: status.session_id,
        agentId: status.agent_id,
        state: status.state,
        message: status.message,
        revision: BigInt(status.revision),
        completedRevision: BigInt(status.completed_revision),
        updatedAt: status.updated_at,
        active: status.active,
      }) },
    }));
    return bytes ? tryWriteEncoded(bytes) : false;
  }






  async function dial(): Promise<void> {
    if (disposed) return;
    dialAttempt += 1;
    _didOpen = false;
    setState({ kind: "connecting", attempt: dialAttempt });
    let jwt: string;
    try { jwt = await deps.mintJwt(); }
    catch (err) {
      log.warn("coord-link", "mint_jwt_failed", { error: (err as Error).message });
      signal("auth.jwt_sign_fail", { stage: "mint", cooldownKey: "jwt" });
      scheduleReconnect();
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
      scheduleReconnect();
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
      clearDrainTimer();
      if (staleTimer !== null) { clearInterval(staleTimer); staleTimer = null; }
      writer = null;
      activeWs = null;
      linkReady = false;
      // Anything still awaiting an application ACK may have been lost with
      // the native socket. Re-admit it on the next dial; coordinator dedup
      // makes replay safe.
      for (const seq of unacked.keys()) unsentEventSeqs.add(seq);
      closeStream = null;
      // A dial that never fired ws.onopen is not necessarily an auth
      // rejection — a 401 upgrade, a handshake timeout and a proxy 502 are
      // indistinguishable here. Just count the streak; scheduleReconnect
      // decides what it means (see backoffCapMs).
      if (!_didOpen) _authRejectCount++;
      if (!disposed) {
        if (relocating) {
          relocating = false;
          void dial();
        } else {
          scheduleReconnect();
        }
      }
    };

    ws.onopen = () => {
      if (disposed) { try { ws.close(); } catch { /* ignore */ } return; }
      setState({ kind: "open", since: openedAt });
      _reconnectFailures = 0;
      _didOpen = true;
      dialReconnected = hasOpened;
      hasOpened = true;
      _authRejectCount = 0;
      linkReady = false;
      activeWs = ws;
      writer = (bytes: Uint8Array): void => {
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
      };
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
        const hello = encodeUpstream(create(CoordWorkerUpSchema, {
          frame: { case: "hello", value: create(WHelloSchema, {
            workerFp: deps.workerFp,
            version: deps.workerVersion,
          }) },
        }));
        if (!hello || !writer) throw new Error("hello encode failed");
        // The socket has just opened, so its native buffer is empty. Hello is
        // the sole forced write; every application frame uses byte admission.
        writer(hello);
        if (unacked.size > 0) {
          for (const seq of unacked.keys()) unsentEventSeqs.add(seq);
          log.info("coord-link", "replaying_unacked", { count: unsentEventSeqs.size });
        }
        // Events and controls may follow hello immediately. Raw metadata stays
        // held until helloAck so authoritative cell repairs can lead it.
        drainQueues();
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
        backoffMs = BACKOFF_INITIAL_MS;
        dialAttempt = 0;
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
      handleDownstream(frame, dialReconnected, ws);
    };

    ws.onerror = () => { log.warn("coord-link", "stream_error", { error: "ws error" }); cleanup(); };
    ws.onclose = () => { cleanup(); };
  }

  /** Bound one downstream terminal request to a monotonic budget derived from
   * the coordinator's RELATIVE `budget_ms`. Frame receipt is the origin, so
   * time spent queueing inside the worker is charged against the same budget
   * the coordinator is still waiting on, and neither host's wall clock — nor
   * any skew between them — participates in the decision. */
  function terminalBudget(socket: WebSocket, budgetMs: number): TerminalRequestBudget {
    const receivedAtMono = performance.now();
    const allowedMs = budgetMs > 0
      ? Math.min(budgetMs, TERMINAL_REQUEST_BUDGET_CAP_MS)
      : TERMINAL_REQUEST_BUDGET_CAP_MS;
    return {
      remainingMs: () => allowedMs - (performance.now() - receivedAtMono),
      isCurrentConnection: () => activeWs === socket,
    };
  }

  function handleDownstream(frame: CoordWorkerDown, reconnected: boolean, socket: WebSocket): void {
    const k = frame.frame?.case;
    if (!k) return;
    const v = frame.frame.value;
    switch (k) {
      case "helloAck": {
        const ha = v as { coordPubkeyB64: string; coordPubkeyKid: string };
        linkReady = true;
        // SessionManager emits reconnect/full repairs synchronously from this
        // callback. drainQueues() then releases raw metadata, preserving the
        // repair-before-backlog boundary.
        deps.onHelloAck?.({
          coord_pubkey_b64: ha.coordPubkeyB64,
          coord_pubkey_kid: ha.coordPubkeyKid,
          reconnected,
        });
        drainQueues();
        log.info("coord-link", "hello_ack", { coord_kid: ha.coordPubkeyKid, reconnected });
        return;
      }
      case "ping": {
        const ts = Number((v as { ts: bigint }).ts);
        send({ kind: "pong", ts });
        return;
      }
      case "browserCommand": {
        const bc = v as { browserId: string; viewerId: string; requestId: string; frameJson: string };
        try {
          deps.onBrowserCommand?.({
            browser_id: bc.browserId, viewer_id: bc.viewerId,
            request_id: bc.requestId, frame: JSON.parse(bc.frameJson) as ClientControlFrame,
          });
        } catch (e) { log.warn("coord-link", "browser_command_parse", { error: String(e) }); diag("transport.cmd_parse_failed", {}); }
        return;
      }
      case "binary": {
        const b = v as { channelId: number; direction: number; data: Uint8Array };
        deps.onBinary?.(b.channelId, b.direction, b.data);
        return;
      }
      case "inputRequest": {
        const request = v as DInputRequest;
        if (inputRequestsInFlight >= INPUT_REQUEST_INFLIGHT_CAP) {
          // Fail closed with proof rather than dropping: nothing was handed to
          // the session manager, so the coordinator may reject definitely and
          // the browser may retry without risking a duplicate write.
          diag("transport.terminal_admission_full", {
            kind: "input",
            in_flight: inputRequestsInFlight,
          });
          send({
            kind: "input-result",
            request_id: request.requestId,
            session_id: request.sessionId,
            input_seq: request.inputSeq,
            status: TerminalInputStatus.REJECTED,
            written_bytes: 0,
            phase: TerminalWritePhase.PRE_WRITE,
            reason: "worker input admission is full",
          });
          return;
        }
        inputRequestsInFlight += 1;
        // The async IIFE keeps the handler call synchronous — receive order
        // into the keeper-admission lane is preserved — while turning a
        // synchronous throw into a rejection this catch can answer, rather
        // than letting it escape through ws.onmessage and strand the request.
        void (async () => deps.onInputRequest?.(request, terminalBudget(socket, request.budgetMs)))()
          .catch((error: unknown) => {
            // A thrown handler cannot say which side of the keeper write it
            // died on, so the batch is reported unknown, never "unsent" —
            // presenting it as unsent is what would license a duplicate.
            const message = error instanceof Error ? error.message : String(error);
            log.warn("coord-link", "input_request_failed", {
              request_id: request.requestId,
              error: message,
            });
            send({
              kind: "input-result",
              request_id: request.requestId,
              session_id: request.sessionId,
              input_seq: request.inputSeq,
              status: TerminalInputStatus.AMBIGUOUS,
              written_bytes: 0,
              phase: TerminalWritePhase.UNKNOWN,
              reason: message,
            });
          })
          .finally(() => { inputRequestsInFlight -= 1; });
        return;
      }
      case "viewportRequest": {
        const request = v as DViewportRequest;
        if (viewportRequestsInFlight >= VIEWPORT_REQUEST_INFLIGHT_CAP) {
          diag("transport.terminal_admission_full", {
            kind: "viewport",
            in_flight: viewportRequestsInFlight,
          });
          send({
            kind: "viewport-result",
            request_id: request.requestId,
            session_id: request.sessionId,
            client_seq: request.clientSeq,
            status: TerminalViewportStatus.REJECTED,
            channel_resize_seq: 0n,
            cols: 0,
            rows: 0,
            resized: false,
            phase: TerminalWritePhase.PRE_WRITE,
            reason: "worker viewport admission is full",
          });
          return;
        }
        viewportRequestsInFlight += 1;
        void (async () => deps.onViewportRequest?.(request, terminalBudget(socket, request.budgetMs)))()
          .catch((error: unknown) => {
            const message = error instanceof Error ? error.message : String(error);
            log.warn("coord-link", "viewport_request_failed", {
              request_id: request.requestId,
              error: message,
            });
            send({
              kind: "viewport-result",
              request_id: request.requestId,
              session_id: request.sessionId,
              client_seq: request.clientSeq,
              status: TerminalViewportStatus.AMBIGUOUS,
              channel_resize_seq: 0n,
              cols: 0,
              rows: 0,
              resized: false,
              phase: TerminalWritePhase.UNKNOWN,
              reason: message,
            });
          })
          .finally(() => { viewportRequestsInFlight -= 1; });
        return;
      }
      case "attachmentChunk": {
        const a = v as { requestId: string; sessionId: string; filename: string; shortPath: boolean; data: Uint8Array; last: boolean; seq: number };
        deps.onAttachmentChunk?.({
          request_id: a.requestId, session_id: a.sessionId, filename: a.filename,
          short_path: a.shortPath, data: a.data, last: a.last, seq: a.seq,
        });
        return;
      }
      case "coordMovePrepare": {
        const move = v as { requestId: string; handoffId: string; sourceUrl: string; targetUrl: string; expectedCoordKid: string; expectedGitSha: string; estimatedDbSize: bigint; action: "CHECK" | "PREPARE" };
        if (!deps.onCoordMovePrepare) {
          send({ kind: "rpc-error", request_id: move.requestId, message: "coordinator move unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onCoordMovePrepare({
          request_id: move.requestId, handoff_id: move.handoffId, source_url: move.sourceUrl, target_url: move.targetUrl,
          expected_coord_kid: move.expectedCoordKid, expected_git_sha: move.expectedGitSha, estimated_db_size: move.estimatedDbSize, action: move.action,
        })).then(() => send({ kind: "rpc-ok", request_id: move.requestId, data: {} }))
          .catch((error) => send({ kind: "rpc-error", request_id: move.requestId, message: (error as Error).message }));
        return;
      }
      case "coordMoveSnapshotStart": {
        const snapshot = v as { requestId: string; handoffId: string; totalSize: bigint; sha256: string; coordKeyPem: Uint8Array; authorizedKeys: Uint8Array; secretSha256: string; expectedWorkerFps: string[] };
        if (!deps.onCoordMoveSnapshotStart) {
          send({ kind: "rpc-error", request_id: snapshot.requestId, message: "coordinator move unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onCoordMoveSnapshotStart({
          request_id: snapshot.requestId, handoff_id: snapshot.handoffId, total_size: snapshot.totalSize, sha256: snapshot.sha256,
          coord_key_pem: snapshot.coordKeyPem, authorized_keys: snapshot.authorizedKeys, secret_sha256: snapshot.secretSha256,
          expected_worker_fps: snapshot.expectedWorkerFps,
        })).then(() => snapshotRequestIds.set(snapshot.handoffId, snapshot.requestId))
          .catch((error) => send({ kind: "rpc-error", request_id: snapshot.requestId, message: (error as Error).message }));
        return;
      }
      case "coordMoveSnapshotChunk": {
        const chunk = v as { handoffId: string; seq: number; data: Uint8Array; last: boolean };
        if (!deps.onCoordMoveSnapshotChunk) return;
        void Promise.resolve(deps.onCoordMoveSnapshotChunk({
          handoff_id: chunk.handoffId, seq: chunk.seq, data: chunk.data, last: chunk.last,
        })).then(() => {
          if (!chunk.last) return;
          const requestId = snapshotRequestIds.get(chunk.handoffId);
          if (!requestId) return;
          snapshotRequestIds.delete(chunk.handoffId);
          send({ kind: "rpc-ok", request_id: requestId, data: {} });
        }).catch((error) => {
          const requestId = snapshotRequestIds.get(chunk.handoffId);
          if (!requestId) return;
          snapshotRequestIds.delete(chunk.handoffId);
          send({ kind: "rpc-error", request_id: requestId, message: (error as Error).message });
        });
        return;
      }
      case "coordRelocate": {
        const relocate = v as { requestId: string; handoffId: string; sourceUrl: string; targetUrl: string; action: "STAGE" | "ACTIVATE" | "COMMIT" | "ABORT" };
        if (!deps.onCoordRelocate) {
          send({ kind: "rpc-error", request_id: relocate.requestId, message: "coordinator move unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onCoordRelocate({
          request_id: relocate.requestId, handoff_id: relocate.handoffId, source_url: relocate.sourceUrl, target_url: relocate.targetUrl, action: relocate.action,
        })).then(() => send({ kind: "rpc-ok", request_id: relocate.requestId, data: {} }))
          .catch((error) => send({ kind: "rpc-error", request_id: relocate.requestId, message: (error as Error).message }));
        return;
      }
      case "updateBroker": {
        const update = v as {
          requestId: string;
          jobId: string;
          action: string;
          manifestUrl: string;
          signatureUrl: string;
          manifestSha256: string;
          publisherSha256: string;
        };
        if (update.action !== "START" && update.action !== "STATUS") {
          send({ kind: "rpc-error", request_id: update.requestId, message: `unsupported updater action: ${update.action}` });
          return;
        }
        if (!deps.onUpdateBroker) {
          send({ kind: "rpc-error", request_id: update.requestId, message: "Windows update broker unsupported by this worker" });
          return;
        }
        void Promise.resolve(deps.onUpdateBroker({
          request_id: update.requestId,
          job_id: update.jobId,
          action: update.action,
          manifest_url: update.manifestUrl,
          signature_url: update.signatureUrl,
          manifest_sha256: update.manifestSha256,
          publisher_sha256: update.publisherSha256,
        })).then((progress) => {
          for (const frame of progress) send({ kind: "update-progress", ...frame });
          const lastSequence = progress.length > 0 ? progress[progress.length - 1]!.sequence : 0;
          send({ kind: "rpc-ok", request_id: update.requestId, data: { last_sequence: lastSequence } });
        }).catch((error) => {
          send({ kind: "rpc-error", request_id: update.requestId, message: (error as Error).message });
        });
        return;
      }
      case "eventAck": {
        // D-4b: drop the acked entry from the unacked outbox so it
        // doesn't replay on the next reconnect.
        const seq = Number((v as { clientSeq: bigint }).clientSeq);
        unacked.delete(seq);
        unsentEventSeqs.delete(seq);
        return;
      }
    }
  }

  function scheduleReconnect(): void {
    if (disposed) return;
    // A worker is a daemon: we retry forever. Crossing the ceiling only
    // makes the wedged-worker anomaly visible (cooldown-gated to once/10s);
    // retry behavior is unchanged.
    _reconnectFailures += 1;
    if (_reconnectFailures >= RECONNECT_GIVE_UP_AFTER) {
      signal("reconnect.give_up", { failures: _reconnectFailures, action: "keep_retrying", cooldownKey: "coordlink" });
    }
    // Escalate backoff for sustained non-open streaks (e.g. stale binary with
    // wrong JWT aud, which never opens at all). A link that HAS opened in this
    // process needs a far longer streak, so a transient stall — a throttled
    // worker, a proxy 502 — costs one 30s cap instead of 5 minutes.
    const _cap = backoffCapMs(_authRejectCount, hasOpened);
    const _threshold = hasOpened ? AUTH_REJECT_THRESHOLD_AFTER_OPEN : AUTH_REJECT_THRESHOLD;
    if (_cap === AUTH_REJECT_BACKOFF_CAP_MS && _authRejectCount === _threshold) {
      log.warn("coord-link", "reconnect_backoff_escalated", {
        count: _authRejectCount, backoffMs: _cap, had_opened: hasOpened,
      });
    }
    const nextDialAtMs = Date.now() + backoffMs;
    setState({ kind: "reconnecting", nextDialAtMs, backoffMs });
    const d = backoffMs;
    backoffMs = Math.min(backoffMs * BACKOFF_MULTIPLIER, _cap);
    reconnectTimer = setTimeout(() => { reconnectTimer = null; if (!disposed) void dial(); }, d);
  }

  function relocate(targetUrl: string, force = false): void {
    if (disposed || (!force && coordHttpUrl === targetUrl)) return;
    // cleanup() nulls closeStream, so during reconnect backoff the else-branch
    // below always fires — while the pending backoff timer fires its OWN dial.
    // Two live sockets both install ws.onmessage → handleDownstream, so every
    // browser command, PTY input byte and coordRelocate frame executes twice
    // (doubled characters in the terminal). Cancel the pending dial first.
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    coordHttpUrl = targetUrl;
    backoffMs = BACKOFF_INITIAL_MS;
    // A worker auth-rejected by the source would otherwise carry a 5-minute
    // backoff cap into the healthy target and sit offline for minutes.
    dialAttempt = 0;
    _authRejectCount = 0;
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
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    clearRefreshTimer();
    clearDrainTimer();
    controlPending.length = 0;
    rawPending.length = 0;
    pendingFrameCount = 0;
    pendingEncodedBytes = 0;
    unsentEventSeqs.clear();
    unacked.clear();
    try { closeStream?.(); } catch { /* ignore */ }
    setState({ kind: "closed" });
  }

  void dial();
  return { send, sendBinary, sendCellGrid, sendAgentStatus, state: () => state, relocate, unackedEventCount: () => unacked.size, dispose };
}
