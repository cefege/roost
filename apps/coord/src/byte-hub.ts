// Per-session PTY byte fan-out hub. Worker upstream binary frames
// (2-byte BE channel_id + 1-byte dir + raw bytes per
// `@roost/shared/wire/control.ts`) get demuxed via the channel→session
// map (built from `opened` events) and published into globalBytesBus.
// Browser Sync never receives these bytes. Coordinator-only title, activity,
// and OSC-8 consumers share a 16 ms leading-edge coalescer so a flooding PTY
// cannot force redundant parser scans.
//
// The per-session BoundedBus<Uint8Array> + reaper that this module used
// to host was retired — no external code subscribes to it. Only the
// channel→session map (with its reverse index for O(1) closed-prune)
// stays.
//
// The channel→session index is mutated ONLY by appendEvent's post-commit
// durable-publication step (event-log.ts → applyDurableChannelIndex), never
// from a sessionBus subscription: the retired bus hook raced browser Sync
// fan-out, so a tab could observe `opened`/`respawned`/`snapshot` before its
// exact worker/channel binding existed and route the first claim/keystroke
// into a channel no keeper owned.

import type { SessionEvent, SessionId, WorkerFp, ChannelId } from "@roost/shared/wire";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import {
  currentTerminalScreenHub,
  notifyTerminalRouteReconciled,
} from "./connect/terminal-view-hub.ts";
import { log } from "@roost/shared/log";
import { signal, diag } from "@roost/shared/diag";
import {
  dropCoalescedBytes,
  publishCoalescedBytes,
} from "./byte-hub-coalescer.ts";

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

interface LastCellDiagnostic {
  workerFp: string;
  channelId: number;
  gridEpoch: string;
  seq: bigint;
  full: boolean;
  receivedAtMs: number;
}
// One mutable record per live session. Updated in place on the cell hot path;
// diagnostic requests allocate the JSON-safe copy on demand.
const _lastCellBySession = new Map<string, LastCellDiagnostic>();

export function cacheSessionWorker(sessionId: string, worker_fp: string, channel: number): void {
  _sessionToWorker.set(sessionId, { worker_fp, channel });
}

export function getCachedSessionWorker(sessionId: string): { worker_fp: string; channel: number } | undefined {
  return _sessionToWorker.get(sessionId);
}

export function evictSessionWorker(sessionId: string): void {
  _sessionToWorker.delete(sessionId);
  _lastCellBySession.delete(sessionId);
}



// Branded WorkerFp/ChannelId are assignable to these, so the coordinator-local
// repair marks (plain route triples) share one key format with the index.
function _key(workerFp: string, channelId: number): string {
  return `${workerFp}:${channelId}`;
}

// Unmapped-drop burst detector. A single drop is the benign open-race (first
// PTY byte before the `opened` event binds the channel — primeChannelMap
// pre-binds on coord restart to avoid it), so the per-drop breadcrumb stays on
// the Tier-2 firehose. A SUSTAINED burst on one (workerFp, channelId) key =
// a mapping that never bound = real output/history loss → Tier-1 signal.
const UNMAPPED_DROP_THRESHOLD = 50;
const UNMAPPED_DROP_WINDOW_MS = 5000;
// A channel that stops dropping must not pin its breadcrumb forever — the map
// used to grow monotonically over coord's uptime. Swept lazily at most once
// per window, plus a hard cap so a pathological many-channel burst stays O(1).
const UNMAPPED_DROP_MAX_ENTRIES = 1024;
const _unmappedDrops = new Map<string, { count: number; first_ts: number }>();
let _unmappedSweepTs = 0;

function _recordUnmappedDrop(workerFp: WorkerFp, channelId: ChannelId): void {
  const key = _key(workerFp, channelId);
  const now = Date.now();
  if (_unmappedDrops.size > 0 && now - _unmappedSweepTs >= UNMAPPED_DROP_WINDOW_MS) {
    _unmappedSweepTs = now;
    for (const [k, rec] of _unmappedDrops) {
      if (now - rec.first_ts > UNMAPPED_DROP_WINDOW_MS) _unmappedDrops.delete(k);
    }
  }
  while (_unmappedDrops.size >= UNMAPPED_DROP_MAX_ENTRIES) {
    let oldestKey: string | null = null;
    let oldestTs = Infinity;
    for (const [k, rec] of _unmappedDrops) {
      if (rec.first_ts < oldestTs) { oldestTs = rec.first_ts; oldestKey = k; }
    }
    if (oldestKey === null) break;
    _unmappedDrops.delete(oldestKey);
  }
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

// Forget one (workerFp, channelId) route: the forward key, its reverse-index
// entry, and its unmapped-drop window. Used by the exact-snapshot sweep, where
// the removed key's session usually stays live under a different channel.
function _forgetKey(key: string): void {
  const sessionId = _channelToSession.get(key);
  if (sessionId !== undefined) {
    const keys = _sessionToKeys.get(sessionId);
    if (keys) {
      keys.delete(key);
      if (keys.size === 0) _sessionToKeys.delete(sessionId);
    }
  }
  _channelToSession.delete(key);
  _unmappedDrops.delete(key);
}

// Drop every live route a session was bound under (O(1) via the reverse
// index), so a rebind cannot leave the old channel resolving to it.
function _unbindSessionKeys(sessionId: SessionId): void {
  const keys = _sessionToKeys.get(sessionId);
  if (!keys) return;
  for (const key of keys) {
    _channelToSession.delete(key);
    _unmappedDrops.delete(key);
  }
  _sessionToKeys.delete(sessionId);
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
  publishCoalescedBytes(sessionId, bytes);
}

// Worker cell frames carry only channel_id. Stamp the durable session binding
// and deliver them directly to the canonical TerminalScreenHub.
export function publishCellGrid(workerFp: WorkerFp, channelId: ChannelId, frame: PbCellGridFrame): void {
  const sessionId = _channelToSession.get(_key(workerFp, channelId));
  if (!sessionId) {
    diag("byte-hub.drop_unmapped_cell", { worker_fp: workerFp, channel_id: channelId });
    _recordUnmappedDrop(workerFp, channelId);
    return;
  }
  if (frame.sessionId !== "" && frame.sessionId !== sessionId) {
    diag("byte-hub.drop_mismatched_cell_session", {
      worker_fp: workerFp,
      channel_id: channelId,
      expected_session_id: sessionId,
      received_session_id: frame.sessionId,
    });
    currentTerminalScreenHub()?.invalidate(
      sessionId,
      "worker cell frame carried a mismatched session id",
    );
    return;
  }
  _clearUnmappedDrop(workerFp, channelId);
  frame.sessionId = sessionId;
  const receivedAtMs = Date.now();
  frame.coordRecvMs = BigInt(receivedAtMs);
  const prior = _lastCellBySession.get(sessionId);
  if (prior) {
    prior.workerFp = workerFp;
    prior.channelId = channelId;
    prior.gridEpoch = frame.gridEpoch;
    prior.seq = frame.seq;
    prior.full = frame.full;
    prior.receivedAtMs = receivedAtMs;
  } else {
    _lastCellBySession.set(sessionId, {
      workerFp,
      channelId,
      gridEpoch: frame.gridEpoch,
      seq: frame.seq,
      full: frame.full,
      receivedAtMs,
    });
  }
  diag("cell.relay", { sid: sessionId, channel_id: channelId });
  currentTerminalScreenHub()?.publishFrame(sessionId, frame);
}

export function publishCellGridChunk(
  workerFp: WorkerFp,
  channelId: ChannelId,
  chunk: PbCellGridChunk,
): void {
  const sessionId = _channelToSession.get(_key(workerFp, channelId));
  if (!sessionId) {
    diag("byte-hub.drop_unmapped_cell_chunk", {
      worker_fp: workerFp,
      channel_id: channelId,
    });
    _recordUnmappedDrop(workerFp, channelId);
    return;
  }
  if (
    chunk.part
    && chunk.part.sessionId !== ""
    && chunk.part.sessionId !== sessionId
  ) {
    diag("byte-hub.drop_mismatched_cell_chunk_session", {
      worker_fp: workerFp,
      channel_id: channelId,
      expected_session_id: sessionId,
      received_session_id: chunk.part.sessionId,
    });
    currentTerminalScreenHub()?.invalidate(
      sessionId,
      "worker cell chunk carried a mismatched session id",
    );
    return;
  }
  _clearUnmappedDrop(workerFp, channelId);
  if (chunk.part) {
    chunk.part.sessionId = sessionId;
    chunk.part.coordRecvMs = BigInt(Date.now());
  }
  currentTerminalScreenHub()?.publishChunk(sessionId, chunk);
  diag("cell.chunk_relay", {
    sid: sessionId,
    channel_id: channelId,
    chunk_index: chunk.chunkIndex,
    chunk_count: chunk.chunkCount,
  });
}

// ─── durable channel-index publication ────────────────────────────────
//
// appendEvent commits its SQLite transaction, calls applyDurableChannelIndex,
// and only then publishes on sessionBus. Every op below therefore runs with
// the projection already durable and no browser having seen the event yet.

// Workers whose exact boot/reconcile snapshot has been applied. Before that the
// live index is legitimately incomplete (coord restarted under a live worker),
// so resolveSessionRoute may fall back to the open-session DB breadcrumb. After
// it, a session with no live route is offline — repopulating from the breadcrumb
// would resurrect a channel the worker no longer owns.
const _reconciledWorkers = new Set<string>();

export function isWorkerChannelIndexReconciled(workerFp: string): boolean {
  return _reconciledWorkers.has(workerFp);
}

// A new authenticated worker connection reopens the pre-reconcile window: its
// hello re-primes the index from the DB and its snapshot has not landed yet.
export function resetWorkerChannelIndexReconcile(workerFp: string): void {
  _reconciledWorkers.delete(workerFp);
}


/** `respawned`: the keeper handed this session a NEW channel on `workerFp` —
 *  the fingerprint that authenticated the connection the event arrived on,
 *  never a `_sessionToWorker` lookup, which can hold an arbitrarily old route.
 *  Every previous key for the session goes first, so a later `closed` cannot
 *  prune the new binding and the dead channel stops resolving; the route cache
 *  is rebound before the browser sees the event, so the next cell frame,
 *  keystroke, and coordinator stream state reach the new core without redial. */
export function rebindRespawnedChannel(
  workerFp: WorkerFp,
  sessionId: SessionId,
  newChannel: ChannelId,
): void {
  _unbindSessionKeys(sessionId);
  // The prior core is gone; its last-cell record would advertise a dead channel
  // until the new core's first frame lands.
  _lastCellBySession.delete(sessionId);
  _bindKey(_key(workerFp, newChannel), sessionId);
  cacheSessionWorker(sessionId, workerFp, newChannel);
}

/** An authenticated worker `snapshot` is an EXACT replacement of that worker's
 *  live routes, not additive priming: a session the worker no longer runs loses
 *  its channel key and route-cache entry, a rebound session loses its older
 *  keys, and the worker is marked reconciled. DB/sidebar breadcrumbs are
 *  untouched — event-log owns the projection — they simply stop resolving to a
 *  live route once the announcing worker is reconciled. */
export function replaceWorkerChannelIndex(
  workerFp: WorkerFp,
  live: ReadonlyArray<{ sessionId: SessionId; channelId: ChannelId }>,
): void {
  const liveKeys = new Set<string>();
  const liveSessions = new Set<string>();
  for (const entry of live) {
    liveKeys.add(_key(workerFp, entry.channelId));
    liveSessions.add(entry.sessionId);
  }
  const prefix = `${workerFp}:`;
  // Deleting the current entry mid-iteration is well-defined for Map, so this
  // sweeps in place without copying the key set.
  for (const key of _channelToSession.keys()) {
    if (!key.startsWith(prefix) || liveKeys.has(key)) continue;
    _forgetKey(key);
  }
  // A route-cache entry can exist with no channel key of its own
  // (resolveSessionRoute's pre-reconcile DB fallback caches one), so the cache
  // needs its own sweep.
  for (const [sessionId, route] of _sessionToWorker) {
    if (route.worker_fp !== workerFp || liveSessions.has(sessionId)) continue;
    evictSessionWorker(sessionId);
  }
  for (const entry of live) {
    const key = _key(workerFp, entry.channelId);
    // Rebound: the session may still carry keys from an older channel or an
    // older worker; those routes are stale the moment this snapshot lands.
    const priorKeys = _sessionToKeys.get(entry.sessionId);
    if (priorKeys) {
      for (const old of priorKeys) if (old !== key) _forgetKey(old);
    }
    _bindKey(key, entry.sessionId);
    cacheSessionWorker(entry.sessionId, workerFp, entry.channelId);
  }
  _reconciledWorkers.add(workerFp);
}

/** Permanently retire every volatile route owned by a deleted worker while
 * preserving its durable session breadcrumbs. Returns affected session ids so
 * terminal-view cleanup can fail independently of route retirement. */
export function retireWorkerRoutes(workerFp: WorkerFp): SessionId[] {
  const sessionIds = new Set<SessionId>();
  for (const [sessionId, route] of _sessionToWorker) {
    if (route.worker_fp === workerFp) sessionIds.add(sessionId as SessionId);
  }
  const prefix = `${workerFp}:`;
  for (const [key, sessionId] of _channelToSession) {
    if (key.startsWith(prefix)) sessionIds.add(sessionId);
  }
  replaceWorkerChannelIndex(workerFp, []);
  for (const sessionId of sessionIds) dropCoalescedBytes(sessionId);
  return [...sessionIds];
}

/** Post-commit channel-index step of appendEvent's durable publication.
 *  `authenticatedWorkerFp` is the fingerprint of the worker connection that
 *  delivered the event (null for coordinator-produced events). */
export function applyDurableChannelIndex(
  event: SessionEvent,
  authenticatedWorkerFp: WorkerFp | null,
): void {
  // `opened`/`snapshot` name their own worker; a mismatch against the socket
  // that delivered them is a protocol violation, never a binding.
  const announcerFp = event.kind === "opened" || event.kind === "snapshot"
    ? event.worker_fp
    : null;
  if (announcerFp !== null && authenticatedWorkerFp !== null && announcerFp !== authenticatedWorkerFp) {
    signal("worker.protocol_violation", {
      reason: "event_worker_fp_mismatch",
      worker_fp: authenticatedWorkerFp,
      announced_fp: announcerFp,
      cooldownKey: authenticatedWorkerFp,
    });
    return;
  }
  switch (event.kind) {
    case "opened":
      _bindKey(_key(event.worker_fp, event.channel), event.session_id);
      cacheSessionWorker(event.session_id, event.worker_fp, event.channel);
      return;
    case "closed":
      // O(1) prune via the reverse index — important because the snapshot path
      // appends per-ghost synthetic closes back to back.
      _unbindSessionKeys(event.session_id);
      evictSessionWorker(event.session_id);
      dropCoalescedBytes(event.session_id);
      return;
    case "respawned":
      if (authenticatedWorkerFp === null) {
        // The new channel belongs to whichever worker authenticated the event.
        // With no fingerprint there is nothing exact to bind, and inferring one
        // from the route cache could bind a channel on a worker that has since
        // been replaced; the stale route stays until that worker's next exact
        // snapshot reconciles it.
        signal("worker.protocol_violation", {
          reason: "respawned_without_worker_fp",
          session_id: event.session_id,
          cooldownKey: event.session_id,
        });
        return;
      }
      rebindRespawnedChannel(authenticatedWorkerFp, event.session_id, event.new_channel);
      notifyTerminalRouteReconciled(
        authenticatedWorkerFp,
        [event.session_id],
      );
      return;
    case "snapshot": {
      const live: Array<{ sessionId: SessionId; channelId: ChannelId }> = [];
      for (const s of event.sessions) {
        if (s.worker_fp !== event.worker_fp) {
          // A snapshot reconciles only the announcing worker's own routes.
          diag("byte-hub.snapshot_foreign_session", {
            worker_fp: event.worker_fp, session_id: s.id, claimed_fp: s.worker_fp,
          });
          continue;
        }
        live.push({ sessionId: s.id, channelId: s.channel });
      }
      replaceWorkerChannelIndex(event.worker_fp, live);
      notifyTerminalRouteReconciled(
        event.worker_fp,
        live.map((entry) => entry.sessionId),
      );
      return;
    }
    default:
      return;
  }
}
