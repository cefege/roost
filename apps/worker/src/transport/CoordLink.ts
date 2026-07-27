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
  WRefreshJwtSchema, WSessionEventSchema, WCellGridSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { ClientSeq } from "./client-seq.ts";
import type {
  CoordWorkerUp, CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import type { ClientControlFrame, SessionEvent } from "@roost/shared/wire";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { log, diag, signal } from "@roost/shared";
import { frameToProto, decodeBinaryFrame, binaryFrameToProto } from "./CoordLink-codec.ts";
import {
  BACKOFF_INITIAL_MS, BACKOFF_MAX_MS, BACKOFF_MULTIPLIER,
  PENDING_CAP, STABLE_SESSION_MS, UNACKED_CAP,
  STALE_LINK_TIMEOUT_MS, STALE_CHECK_INTERVAL_MS,
  AUTH_REJECT_THRESHOLD, AUTH_REJECT_BACKOFF_CAP_MS,
} from "./CoordLink-constants.ts";
import type { CoordLinkDeps, CoordLink, UpstreamFrame, CoordLinkState } from "./CoordLink-types.ts";
export type { CoordLinkDeps, CoordLink } from "./CoordLink-types.ts";

// ─── implementation ──────────────────────────────────────────────────

// Consecutive dial failures since the last successful open. Reset to 0
// in ws.onopen; a worker is a daemon so we NEVER stop reconnecting —
// crossing the ceiling only fires an observability signal (once, then
// cooldown-gated) so a wedged worker is visible in `roost doctor`.
const RECONNECT_GIVE_UP_AFTER = 10;
let _reconnectFailures = 0;

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
  // Consecutive dials that failed without ws.onopen firing (upgrade
  // rejected — auth failure). After AUTH_REJECT_THRESHOLD, escalate the
  // backoff cap to AUTH_REJECT_BACKOFF_CAP_MS. Reset on successful open.
  let _authRejectCount = 0;
  let _didOpen = false;
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  // Pending backoff dial. Held so relocate()/dispose() can cancel it — an
  // uncancelled timer means a second concurrent socket.
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let writer: ((f: CoordWorkerUp) => void) | null = null;
  let closeStream: (() => void) | null = null;
  const pending: Array<UpstreamFrame | { binary: Uint8Array }> = [];
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
      // T2.2 — in-band JWT rotation. Mint a new token and push it as a
      // WRefreshJwt frame; stream stays open. If the writer is gone or
      // mint fails, fall back to closing the stream so the reconnect
      // path mints fresh on the next dial.
      if (!writer) return;
      try {
        const jwt = await deps.mintJwt();
        writer(create(CoordWorkerUpSchema, {
          frame: { case: "refreshJwt", value: create(WRefreshJwtSchema, { jwt }) },
        }));
        log.debug("coord-link", "jwt_refreshed_inband");
        scheduleRefresh();  // chain
      } catch (e) {
        log.warn("coord-link", "jwt_refresh_inband_failed", { error: (e as Error).message });
        try { closeStream?.(); } catch { /* ignore */ }
      }
    }, refreshInMs);
  }

  /** Wrap a SessionEvent + client_seq into a WSessionEvent frame.
   *  Caller is responsible for adding to unacked beforehand. */
  function encodeEventFrame(event: SessionEvent, seq: number): CoordWorkerUp | null {
    const proto = eventToProto(event, 0);
    if (!proto) {
      log.warn("coord-link", "event_proto_encode_returned_null", { kind: (event as { kind: string }).kind });
      return null;
    }
    return create(CoordWorkerUpSchema, {
      frame: { case: "event", value: create(WSessionEventSchema, {
        event: proto,
        clientSeq: BigInt(seq),
      })},
    });
  }

  /** Owns D-4b bookkeeping for SessionEvents. Stamps client_seq AFTER
   *  cap-check, registers in `unacked` BEFORE writer (so writer-throw
   *  leaves the entry registered for replay on reconnect — coord
   *  dedups via UNIQUE INDEX). Events NEVER enter `pending` — they
   *  live in `unacked` until acked, which dedups across reconnect for
   *  free without `pending`-cap eviction risk.  */
  function sendEvent(event: SessionEvent): boolean {
    if (unacked.size >= UNACKED_CAP) {
      const oldest = unacked.keys().next().value;
      if (oldest !== undefined) unacked.delete(oldest);
      log.error("coord-link", "unacked_overflow_drop", { cap: UNACKED_CAP, dropped_seq: oldest });
      signal("transport.event_drop", { dropped_seq: oldest, unacked_size: unacked.size, cooldownKey: "outbox" });
    }
    const seq = clientSeq.next();
    unacked.set(seq, event);
    if (writer) {
      const p = encodeEventFrame(event, seq);
      if (p) { try { writer(p); return true; } catch { /* writer threw — entry stays in unacked, will replay on reconnect */ } }
    }
    return false;
  }

  function send(frame: UpstreamFrame): boolean {
    if (disposed) return false;
    if (frame.kind === "event") return sendEvent(frame.event);
    if (writer) {
      const p = frameToProto(frame);
      if (p) { try { writer(p); return true; } catch { /* fall through */ } }
    }
    if (pending.length >= PENDING_CAP) { pending.shift(); diag("transport.frame_dropped", { reason: "pending_overflow", kind: frame.kind }); }
    pending.push(frame);
    return false;
  }

  function sendBinary(bytes: Uint8Array | ArrayBufferView): boolean {
    if (disposed) return false;
    const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const f = decodeBinaryFrame(arr);
    if (!f) { diag("transport.frame_dropped", { reason: "binary_decode_null", kind: "binary" }); return false; }
    if (writer) {
      try { writer(binaryFrameToProto(f)); return true; }
      catch { /* fall through */ }
    }
    if (pending.length >= PENDING_CAP) { pending.shift(); diag("transport.frame_dropped", { reason: "pending_overflow", kind: "binary" }); }
    pending.push({ binary: arr });
    return false;
  }

  function sendCellGrid(channelId: number, frame: PbCellGridFrame): boolean {
    if (disposed || !writer) return false;
    const up = create(CoordWorkerUpSchema, {
      frame: { case: "cellGrid", value: create(WCellGridSchema, { channelId, frame }) },
    });
    try { writer(up); return true; } catch { diag("transport.frame_dropped", { reason: "writer_throw", kind: "cellGrid" }); return false; }
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
      ws = new WebSocket(url);
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
    let staleTimer: ReturnType<typeof setInterval> | null = null;

    // `writer` stays null until OPEN so send()/sendBinary() take their
    // queue-to-pending/unacked fallback rather than throwing into a
    // not-yet-writable socket. `closeStream` is wired immediately so
    // dispose() / jwt-refresh-failure can tear down a connecting socket.
    closeStream = () => { try { ws.close(); } catch { /* ignore */ } };

    // Fires exactly once per dial (onerror→onclose both call it).
    const cleanup = (): void => {
      if (cleanedUp) return;
      cleanedUp = true;
      clearRefreshTimer();
      if (staleTimer !== null) { clearInterval(staleTimer); staleTimer = null; }
      writer = null;
      closeStream = null;
      // If ws.onopen never fired, the upgrade was rejected (auth failure).
      // Track consecutive non-open dials for backoff escalation.
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
      _authRejectCount = 0;
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
        cleanup(); // idempotent (cleanedUp) — a late onclose no-ops
      }, deps.staleCheckIntervalMs ?? STALE_CHECK_INTERVAL_MS);
      // Throwing writer (no swallow): if the socket isn't writable, the
      // throw propagates to send()/sendBinary()'s catch, which keeps the
      // frame queued for the next reconnect.
      const w = (f: CoordWorkerUp): void => { ws.send(toBinary(CoordWorkerUpSchema, f)); };
      writer = w;
      // Flush in canonical order: hello → unacked replay (seq order) →
      // pending drain. ws is OPEN here so these won't throw. D-4b: coord
      // dedups replayed events via UNIQUE INDEX.
      try {
        w(create(CoordWorkerUpSchema, {
          frame: { case: "hello", value: create(WHelloSchema, { workerFp: deps.workerFp, version: deps.workerVersion }) },
        }));
        if (unacked.size > 0) {
          const replaySeqs = Array.from(unacked.keys()).sort((a, b) => a - b);
          log.info("coord-link", "replaying_unacked", { count: replaySeqs.length });
          for (const seq of replaySeqs) {
            const ev = unacked.get(seq)!;
            const p = encodeEventFrame(ev, seq);
            if (p) w(p);
          }
        }
        while (pending.length > 0) {
          const item = pending.shift()!;
          if ("binary" in item) {
            const arr = (item as { binary: Uint8Array }).binary;
            const f = decodeBinaryFrame(arr);
            if (f) w(binaryFrameToProto(f));
            // Malformed binary in the drain — log so the seqno-splice
            // "unexplained gap" is at least diagnosable.
            else log.warn("coord-link", "drain_drop_malformed_binary", { len: arr.length });
          } else {
            const p = frameToProto(item);
            if (p) w(p);
          }
        }
      } catch (err) {
        log.warn("coord-link", "flush_failed", { error: (err as Error).message });
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
      handleDownstream(frame);
    };

    ws.onerror = () => { log.warn("coord-link", "stream_error", { error: "ws error" }); cleanup(); };
    ws.onclose = () => { cleanup(); };
  }

  function handleDownstream(frame: CoordWorkerDown): void {
    const k = frame.frame?.case;
    if (!k) return;
    const v = frame.frame.value;
    switch (k) {
      case "helloAck": {
        const ha = v as { coordPubkeyB64: string; coordPubkeyKid: string };
        deps.onHelloAck?.({ coord_pubkey_b64: ha.coordPubkeyB64, coord_pubkey_kid: ha.coordPubkeyKid });
        log.info("coord-link", "hello_ack", { coord_kid: ha.coordPubkeyKid });
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
      case "eventAck": {
        // D-4b: drop the acked entry from the unacked outbox so it
        // doesn't replay on the next reconnect.
        const seq = Number((v as { clientSeq: bigint }).clientSeq);
        unacked.delete(seq);
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
    // Escalate backoff for sustained auth-rejection streaks (e.g. stale
    // binary with wrong JWT aud). After AUTH_REJECT_THRESHOLD consecutive
    // non-open dials, cap at AUTH_REJECT_BACKOFF_CAP_MS instead of the
    // normal 30s — cuts auth-retry noise 10× (1,849/day → ~288/day).
    const _escalated = _authRejectCount >= AUTH_REJECT_THRESHOLD;
    const _cap = _escalated ? AUTH_REJECT_BACKOFF_CAP_MS : BACKOFF_MAX_MS;
    if (_escalated && _authRejectCount === AUTH_REJECT_THRESHOLD) {
      log.warn("coord-link", "auth_rejection_escalated", {
        count: _authRejectCount, backoffMs: _cap,
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
    try { closeStream?.(); } catch { /* ignore */ }
    setState({ kind: "closed" });
  }

  void dial();
  return { send, sendBinary, sendCellGrid, state: () => state, relocate, unackedEventCount: () => unacked.size, dispose };
}
