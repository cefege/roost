// Per-session PTY byte fan-out hub. Worker upstream binary frames
// (2-byte BE channel_id + 1-byte dir + raw bytes per
// `@roost/shared/wire/control.ts`) get demuxed via the channel→session
// map (built from `opened` events) and published into globalBytesBus.
// Sync stream's bytes branch is the SPA's sole byte path post-firehose.
//
// The per-session BoundedBus<Uint8Array> + reaper that this module used
// to host was retired — no external code subscribes to it. Only the
// channel→session map (with its reverse index for O(1) closed-prune)
// stays.

import { sessionBus, globalBytesBus, globalCellBus, claudeStatusBus, globalChatBus } from "./buses.ts";
import type { SessionEvent, SessionId, WorkerFp, ChannelId } from "@roost/shared/wire";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { ChatFrame } from "@roost/shared/proto/sync_pb";
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

// SessionId → last claude_status the worker reported. claudeStatusBus is
// fire-and-forget live-delta (B10 in handlers-streaming), and claude_status
// is NOT in the sessions projection, so a browser connecting AFTER a claude
// session's last transition never learns it's a claude → renders it as a plain
// terminal. This cache lets the Sync handler replay current status on connect
// (snapshotClaudeStatuses). Pruned on `closed` alongside the channel map.
// ponytail: coord-local; empties on coord restart, self-heals on the session's
// next transition. Worker-side re-emit would close that too, if it ever matters.
const _lastClaudeStatus = new Map<SessionId, string>();

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
  globalBytesBus.publish({ session_id: sessionId, bytes });
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

// Omp chat. Worker WChat frames carry channel_id only; map to session_id,
// stamp it, fan out via globalChatBus. Unmapped channel = drop (same as cells).
export function publishChat(workerFp: WorkerFp, channelId: ChannelId, frame: ChatFrame): void {
  const sessionId = _channelToSession.get(_key(workerFp, channelId));
  if (!sessionId) {
    diag("byte-hub.drop_unmapped_chat", { worker_fp: workerFp, channel_id: channelId });
    // Unlike PTY bytes there is no re-send and no replay: a dropped chat frame
    // is permanent data loss, so it gets a line even without ROOST_DIAG=1.
    log.warn("byte-hub", "drop_unmapped_chat", { worker_fp: workerFp, channel_id: channelId, seq: Number(frame.seq) });
    _recordUnmappedDrop(workerFp, channelId);
    return;
  }
  _clearUnmappedDrop(workerFp, channelId);
  frame.sessionId = sessionId;
  diag("chat.relay", { sid: sessionId, channel_id: channelId, seq: Number(frame.seq) });
  globalChatBus.publish(frame);
}

// herdr agent status. Worker WClaudeStatus frames carry channel_id only; map to
// session_id and republish to claudeStatusBus → Sync firehose → SPA chip. Volatile
// (never durable); unmapped channel = drop (the worker re-sends on `opened` bind).
export function publishClaudeStatus(workerFp: WorkerFp, channelId: ChannelId, status: string): void {
  const sessionId = _channelToSession.get(_key(workerFp, channelId));
  if (!sessionId) {
    diag("byte-hub.drop_unmapped_status", { worker_fp: workerFp, channel_id: channelId });
    _recordUnmappedDrop(workerFp, channelId);
    return;
  }
  _clearUnmappedDrop(workerFp, channelId);
  _lastClaudeStatus.set(sessionId, status);
  claudeStatusBus.publish({ session_id: sessionId, status });
}

// Snapshot of current per-session claude_status. The Sync handler replays this
// on connect so a freshly-loaded SPA immediately knows which sessions are claude
// (fixing "claude session shows as a plain terminal after reload").
export function snapshotClaudeStatuses(): Array<{ session_id: SessionId; status: string }> {
  return [..._lastClaudeStatus].map(([session_id, status]) => ({ session_id, status }));
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
      _lastClaudeStatus.delete(ev.session_id);
      evictSessionWorker(ev.session_id);
    } else if (ev.kind === "snapshot") {
      for (const s of ev.sessions) {
        _bindKey(_key(s.worker_fp, s.channel), s.id);
        cacheSessionWorker(s.id, s.worker_fp, s.channel);
      }
    }
  });
}
