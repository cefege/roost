// Per-session PTY byte fan-out hub. Worker upstream binary frames
// (2-byte BE channel_id + 1-byte dir + raw bytes per
// `@roost/shared/wire/control.ts`) get demuxed via the channel→session
// map (built from `opened` events) and published into globalBytesBus.
// Sync stream's bytes branch is the SPA's sole byte path post-firehose, and
// publishBytes coalesces it on a 16 ms leading-edge governor (see
// BYTE_COALESCE_MS) so a flooding PTY cannot bury the cell frames that paint.
//
// The per-session BoundedBus<Uint8Array> + reaper that this module used
// to host was retired — no external code subscribes to it. Only the
// channel→session map (with its reverse index for O(1) closed-prune)
// stays.

import { sessionBus, globalBytesBus, globalCellBus } from "./buses.ts";
import type { SessionEvent, SessionId, WorkerFp, ChannelId } from "@roost/shared/wire";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { log } from "@roost/shared/log";
import { signal, diag } from "@roost/shared/diag";

// (workerFp, channelId) → SessionId. Built from `opened` SessionEvents.
// Worker binary frames carry channel_id only; coord needs the session_id
// to fan out. Pruned on `closed`.
const _channelToSession = new Map<string, SessionId>();
// Reverse index: SessionId → Set of (workerFp, channelId) keys
// registered for that session. Lets `closed` prune in O(1) instead of
// walking the entire _channelToSession Map per close — snapshot path
// emits per-ghost synthetic closes inside one SQLite transaction so
// the prior O(n) scan multiplied trx wall-clock by ghost-count × map
// size and blocked the event loop. Same correctness (handles the
// "session registered under multiple keys" case) at much lower cost.
const _sessionToKeys = new Map<SessionId, Set<string>>();

// SessionId → { worker_fp, channel }. Cache for sessionsInput: avoids the
// async DB SELECT on every keystroke POST. Populated on opened/snapshot/
// primeChannelMap; evicted on closed. A stale entry (worker disconnect) is
// safe — getWorkerHubSocket returns null → accepted:false (the existing
// no_worker_sock path). Repopulated on the next hello/primeChannelMap.
const _sessionToWorker = new Map<string, { worker_fp: string; channel: number }>();

export function cacheSessionWorker(sessionId: string, worker_fp: string, channel: number): void {
  _sessionToWorker.set(sessionId, { worker_fp, channel });
}

export function getCachedSessionWorker(sessionId: string): { worker_fp: string; channel: number } | undefined {
  return _sessionToWorker.get(sessionId);
}

export function evictSessionWorker(sessionId: string): void {
  _sessionToWorker.delete(sessionId);
}


function _key(workerFp: WorkerFp, channelId: ChannelId): string {
  return `${workerFp}:${channelId}`;
}

// Unmapped-drop burst detector. A single drop is the benign open-race (first
// PTY byte before the `opened` event binds the channel — primeChannelMap
// pre-binds on coord restart to avoid it), so the per-drop breadcrumb stays on
// the Tier-2 firehose. A SUSTAINED burst on one (workerFp, channelId) key =
// a mapping that never bound = real output/history loss → Tier-1 signal.
const UNMAPPED_DROP_THRESHOLD = 50;
const UNMAPPED_DROP_WINDOW_MS = 5000;
const _unmappedDrops = new Map<string, { count: number; first_ts: number }>();

function _recordUnmappedDrop(workerFp: WorkerFp, channelId: ChannelId): void {
  const key = _key(workerFp, channelId);
  const now = Date.now();
  let rec = _unmappedDrops.get(key);
  if (!rec || now - rec.first_ts > UNMAPPED_DROP_WINDOW_MS) {
    rec = { count: 0, first_ts: now };
    _unmappedDrops.set(key, rec);
  }
  rec.count++;
  if (rec.count > UNMAPPED_DROP_THRESHOLD) {
    signal("bytes.drop_unmapped", {
      worker_fp: workerFp,
      channel_id: channelId,
      drops: rec.count,
      window_ms: UNMAPPED_DROP_WINDOW_MS,
      cooldownKey: String(channelId),
    });
  }
}

// A channel that binds/publishes successfully is no longer dropping — clear its
// burst counter so a later transient drop starts a fresh window.
function _clearUnmappedDrop(workerFp: WorkerFp, channelId: ChannelId): void {
  _unmappedDrops.delete(_key(workerFp, channelId));
}

function _bindKey(key: string, sessionId: SessionId): void {
  // If `key` is already bound to a DIFFERENT session, scrub the old
  // session's reverse-index entry first. Otherwise a future `closed`
  // for the old session would walk its keys set, hit this key, and
  // delete the NEW session's mapping — silently dropping every
  // subsequent PTY chunk for it as drop_unmapped_chunk.
  const prev = _channelToSession.get(key);
  if (prev !== undefined && prev !== sessionId) {
    const prevKeys = _sessionToKeys.get(prev);
    if (prevKeys) {
      prevKeys.delete(key);
      if (prevKeys.size === 0) _sessionToKeys.delete(prev);
    }
  }
  _channelToSession.set(key, sessionId);
  let keys = _sessionToKeys.get(sessionId);
  if (!keys) { keys = new Set(); _sessionToKeys.set(sessionId, keys); }
  keys.add(key);
  _unmappedDrops.delete(key);
}

export function lookupSessionId(workerFp: WorkerFp, channelId: ChannelId): SessionId | undefined {
  return _channelToSession.get(_key(workerFp, channelId));
}

// When coord restarts and the worker stays up, the worker never
// re-emits `opened` for sessions that survived; the in-memory
// channel→session map is empty until the next `snapshot`. browser bytes
// get dropped as `drop_unmapped_chunk`. The worker WS handshake primes the
// map from the DB on worker `hello` to close that gap.
export function primeChannelMap(rows: Array<{ id: string; worker_fp: string; channel: number }>): void {
  for (const r of rows) {
    _bindKey(_key(r.worker_fp as WorkerFp, r.channel as ChannelId), r.id as SessionId);
    cacheSessionWorker(r.id, r.worker_fp, r.channel);
  }
}

// Per-session PTY-byte coalescer. Mirrors the worker's cell governor
// (CELL_EMIT_COALESCE_MS, worker/session-constants.ts): LEADING-edge publish so
// a single keystroke echo ships with zero added latency, then a RE-ARMED fixed
// interval — never a reset deadline — so a continuously-producing PTY is
// bounded at one bytes frame per window instead of one per chunk. Without it a
// flood puts thousands of frames that paint NOTHING (the browser only mines
// these bytes for the OSC-8 link map, asynchronously) ahead of the ~62 cell
// frames/s that actually paint, on the same ordered socket and the same main
// thread. Order and content are preserved: concatenation is append-only in
// publish order and all three consumers (terminal-title-hub, last-activity-hub,
// the SPA's Osc8Tracker) are carry-based stream scanners that cannot observe a
// chunk boundary.
const BYTE_COALESCE_MS = 16;
// Hard flush bound. Keeps one frame well under sync-ws-handler's 8 MiB
// BACKPRESSURE_LIMIT_BYTES even when a session dumps at full speed.
const BYTE_COALESCE_CAP_BYTES = 256 * 1024;
interface PendingBytes {
  parts: Uint8Array[];
  len: number;
  timer: Timer | undefined;
}
const _pendingBytes = new Map<SessionId, PendingBytes>();

function _flushPendingBytes(sessionId: SessionId, pending: PendingBytes): void {
  if (pending.len === 0) return;
  const joined = new Uint8Array(pending.len);
  let at = 0;
  for (const part of pending.parts) {
    joined.set(part, at);
    at += part.length;
  }
  pending.parts = [];
  pending.len = 0;
  globalBytesBus.publish({ session_id: sessionId, bytes: joined });
}

function _armByteCoalesce(sessionId: SessionId, pending: PendingBytes): void {
  const timer = setTimeout(() => {
    // Nothing absorbed this window: the session went quiet, so retire the entry
    // and let its next chunk take the leading edge again.
    if (pending.len === 0) {
      if (_pendingBytes.get(sessionId) === pending) _pendingBytes.delete(sessionId);
      return;
    }
    _flushPendingBytes(sessionId, pending);
    _armByteCoalesce(sessionId, pending);
  }, BYTE_COALESCE_MS);
  // Never hold the process (or a coord test) open on this timer.
  timer.unref?.();
  pending.timer = timer;
}

export function publishBytes(workerFp: WorkerFp, channelId: ChannelId, bytes: Uint8Array): void {
  const sessionId = _channelToSession.get(_key(workerFp, channelId));
  if (!sessionId) {
    diag("byte-hub.drop_unmapped_chunk", {
      worker_fp: workerFp,
      channel_id: channelId,
      bytes: bytes.length,
    });
    _recordUnmappedDrop(workerFp, channelId);
    return;
  }
  _clearUnmappedDrop(workerFp, channelId);
  const pending = _pendingBytes.get(sessionId);
  if (!pending) {
    globalBytesBus.publish({ session_id: sessionId, bytes });
    const fresh: PendingBytes = { parts: [], len: 0, timer: undefined };
    _pendingBytes.set(sessionId, fresh);
    _armByteCoalesce(sessionId, fresh);
    return;
  }
  pending.parts.push(bytes);
  pending.len += bytes.length;
  if (pending.len >= BYTE_COALESCE_CAP_BYTES) {
    clearTimeout(pending.timer);
    _flushPendingBytes(sessionId, pending);
    _armByteCoalesce(sessionId, pending);
  }
}

// R11 cell-shipping. Worker WCellGrid frames carry channel_id only (same
// as binary); map to session_id, stamp it on the proto frame, fan out via
// globalCellBus. Unmapped channel = drop (same as bytes — the viewer gets a
// full frame once the `opened` event binds the channel).
export function publishCellGrid(workerFp: WorkerFp, channelId: ChannelId, frame: PbCellGridFrame): void {
  const sessionId = _channelToSession.get(_key(workerFp, channelId));
  if (!sessionId) {
    diag("byte-hub.drop_unmapped_cell", { worker_fp: workerFp, channel_id: channelId });
    _recordUnmappedDrop(workerFp, channelId);
    return;
  }
  _clearUnmappedDrop(workerFp, channelId);
  frame.sessionId = sessionId;
  frame.coordRecvMs = BigInt(Date.now());
  diag("cell.relay", { sid: sessionId, channel_id: channelId });
  globalCellBus.publish(frame);
}



// Subscribe to sessionBus so `opened` / `closed` / `snapshot` events
// keep the channel→session map current. Coord restart that finds
// running workers primes the map from DB (see primeChannelMap) so the
// first PTY byte never drops while the worker hasn't re-emitted
// `opened`.
let _busHookInstalled = false;
export function installByteHubBusHook(): void {
  if (_busHookInstalled) return;
  _busHookInstalled = true;
  sessionBus.subscribe((ev: SessionEvent) => {
    if (ev.kind === "opened") {
      _bindKey(_key(ev.worker_fp, ev.channel), ev.session_id);
      cacheSessionWorker(ev.session_id, ev.worker_fp, ev.channel);
    } else if (ev.kind === "closed") {
      // O(1) prune via the reverse index — important because the
      // snapshot path emits per-ghost synthetic closes inside one
      // SQLite trx.
      const keys = _sessionToKeys.get(ev.session_id);
      if (keys) {
        for (const k of keys) _channelToSession.delete(k);
        _sessionToKeys.delete(ev.session_id);
      }
      evictSessionWorker(ev.session_id);
      // Undelivered tail bytes are discarded, matching the worker's own
      // post-close drop policy (session-emit.ts).
      const pending = _pendingBytes.get(ev.session_id);
      if (pending) {
        clearTimeout(pending.timer);
        _pendingBytes.delete(ev.session_id);
      }
    } else if (ev.kind === "snapshot") {
      for (const s of ev.sessions) {
        _bindKey(_key(s.worker_fp, s.channel), s.id);
        cacheSessionWorker(s.id, s.worker_fp, s.channel);
      }
    }
  });
}
