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

import { globalBytesBus, globalCellBus } from "./buses.ts";
import type { SessionEvent, SessionId, WorkerFp, ChannelId } from "@roost/shared/wire";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import {
  clearBarrierRepairForFullFrame,
  dropStaleBarrierRepair,
} from "./byte-hub-barrier-repair.ts";
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
export interface CoordinatorLastCellDiagnostic {
  worker_fp: string;
  channel_id: number;
  grid_epoch: string;
  seq: string;
  full: boolean;
  received_at_ms: number;
}

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

export function _sessionRouteSnapshot(): Record<string, { worker_fp: string; channel_id: number }> {
  const out: Record<string, { worker_fp: string; channel_id: number }> = {};
  for (const [sessionId, route] of _sessionToWorker) {
    out[sessionId] = { worker_fp: route.worker_fp, channel_id: route.channel };
  }
  return out;
}

export function _lastCellSnapshot(): Record<string, CoordinatorLastCellDiagnostic> {
  const out: Record<string, CoordinatorLastCellDiagnostic> = {};
  for (const [sessionId, cell] of _lastCellBySession) {
    out[sessionId] = {
      worker_fp: cell.workerFp,
      channel_id: cell.channelId,
      grid_epoch: cell.gridEpoch,
      seq: cell.seq.toString(),
      full: cell.full,
      received_at_ms: cell.receivedAtMs,
    };
  }
  return out;
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

/** The barrier-repair ledger (byte-hub-barrier-repair.ts) sweeps its marks
 *  against the live index after a rebind, and needs exactly one bit of it: has
 *  this route been rebound to some OTHER session? An unbound route answers
 *  false — its durable binding may still be in flight. This stays an accessor
 *  so `_channelToSession` is never handed out for mutation. */
export function _isChannelBoundToOtherSession(
  workerFp: string,
  channelId: number,
  sessionId: string,
): boolean {
  const bound = _channelToSession.get(_key(workerFp, channelId));
  return bound !== undefined && bound !== sessionId;
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

// Per-session coordinator-internal PTY-byte coalescer. Mirrors the worker's
// cell governor (CELL_EMIT_COALESCE_MS, worker/session-constants.ts):
// LEADING-edge publish gives a single keystroke echo zero added latency, then
// a RE-ARMED fixed interval — never a reset deadline — bounds continuous PTY
// output to one parser batch per window instead of one batch per chunk.
// Order and content are preserved: concatenation is append-only in publish
// order, and both consumers (terminal-title-hub and last-activity-hub) are
// carry-based stream scanners that cannot observe a chunk boundary.
const BYTE_COALESCE_MS = 16;
// Bound each pending parser batch and its temporary concatenation allocation.
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
  globalCellBus.publish(frame);
  // Only a full frame for this exact route proves the cells the announcement
  // barrier dropped are back — never a timer, never a delta.
  if (frame.full) clearBarrierRepairForFullFrame(workerFp, sessionId, channelId);
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

// Undelivered tail bytes are discarded, matching the worker's own post-close
// drop policy (session-emit.ts). Clearing the timer is what stops a closed
// session from leaking a re-armed coalescer.
function _dropPendingBytes(sessionId: SessionId): void {
  const pending = _pendingBytes.get(sessionId);
  if (!pending) return;
  clearTimeout(pending.timer);
  _pendingBytes.delete(sessionId);
}

/** `respawned`: the keeper handed this session a NEW channel on `workerFp` —
 *  the fingerprint that authenticated the connection the event arrived on,
 *  never a `_sessionToWorker` lookup, which can hold an arbitrarily old route.
 *  Every previous key for the session goes first, so a later `closed` cannot
 *  prune the new binding and the dead channel stops resolving; the route cache
 *  is rebound before the browser sees the event, so the next cell frame,
 *  keystroke, and viewport claim reach the new core with no Sync reconnect. */
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
  dropStaleBarrierRepair(workerFp);
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
  dropStaleBarrierRepair(workerFp);
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
      _dropPendingBytes(event.session_id);
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
      return;
    }
    default:
      return;
  }
}
