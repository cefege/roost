// Per-session viewport ownership registry for the terminal outbound lane. Split
// out of ws/sync-outbound.ts. Owns the one `viewportSessions` map, its
// sessionStorage watermark persistence, status fan-out to mounted owners, the
// optimistic-spawn preclaim seed, and the bounded diagnostic view of a claim.
//
// The map itself is deliberately NOT exported: every other module reaches it
// through the narrow accessors below, so a late timer or a replaced mount can
// only ever act on the record the registry still recognises
// (isCurrentViewportSession).

import { currentSyncV2TerminalState, type SyncV2TerminalState } from "../store/sync.ts";
import { forgetSmokeViewportSession } from "./sync-outbound-smoke.ts";
import type {
  TerminalViewportClaimSnapshot,
  TerminalViewportOwnerStatus,
  ViewportDesired,
  ViewportOutcome,
  ViewportSession,
} from "./sync-outbound-viewport-types.ts";

const MAX_VIEWPORT_SESSIONS = 256;
const STORAGE_KEY = "roost.sync-v2.viewport-intents";
export const MAX_VIEWPORT_STATUS_LISTENERS = 32;
export const MAX_WIRE_DIMENSION = 0xffff_ffff;
export const MAX_SAFE_CELL_SEQ = BigInt(Number.MAX_SAFE_INTEGER);

const viewportSessions = new Map<string, ViewportSession>();

function safeSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

export function boundedViewportReason(reason: string): string {
  return reason.slice(0, 200);
}

function newViewportSession(sessionId: string, sequence = 0n, updatedAt = Date.now()): ViewportSession {
  return {
    sessionId,
    sequenceFloor: sequence,
    sequenceUpdatedAt: updatedAt,
    ownerToken: null,
    desired: null,
    attempt: null,
    preclaim: null,
    confirmed: null,
    retryTimer: null,
    retryAt: null,
    retryReason: null,
    retrySocketId: null,
    retryDomainGeneration: null,
    processEpoch: currentSyncV2TerminalState()?.processEpoch ?? null,
    fullFrameReceipt: 0,
    fullFrameSeq: 0,
    fullFrameGridEpoch: null,
    status: null,
    listeners: new Set(),
  };
}

export function emitViewportStatus(session: ViewportSession, status: TerminalViewportOwnerStatus): void {
  session.status = status;
  for (const listener of session.listeners) {
    try {
      listener(status);
    } catch {
      // Ownership and retry progress cannot depend on a diagnostic observer.
    }
  }
}

export function clearViewportAttempt(session: ViewportSession): void {
  clearTimeout(session.attempt?.timer ?? undefined);
  session.attempt = null;
}

export function clearViewportRetry(session: ViewportSession): void {
  clearTimeout(session.retryTimer ?? undefined);
  session.retryTimer = null;
  session.retryAt = null;
  session.retryReason = null;
  session.retrySocketId = null;
  session.retryDomainGeneration = null;
}

export function settleViewportDesired(session: ViewportSession, outcome: ViewportOutcome): void {
  const desired = session.desired;
  if (!desired || desired.settled) return;
  desired.settled = true;
  desired.resolve(outcome);
}

export function supersedeViewportDesired(session: ViewportSession, reason: string): void {
  clearViewportAttempt(session);
  clearViewportRetry(session);
  const desired = session.desired;
  if (!desired) return;
  const outcome: ViewportOutcome = {
    status: "superseded",
    sequence: desired.sequence,
    reason: boundedViewportReason(reason),
  };
  emitViewportStatus(session, outcome);
  settleViewportDesired(session, outcome);
  session.desired = null;
  session.retrySocketId = null;
  session.retryDomainGeneration = null;
}

function evictViewportSession(session: ViewportSession, reason: string): void {
  if (viewportSessions.get(session.sessionId) !== session) return;
  clearViewportAttempt(session);
  clearViewportRetry(session);
  const desired = session.desired;
  if (desired) {
    const outcome: ViewportOutcome = {
      status: "rejected",
      sequence: desired.sequence,
      reason: boundedViewportReason(reason),
    };
    emitViewportStatus(session, outcome);
    settleViewportDesired(session, outcome);
  }
  session.listeners.clear();
  viewportSessions.delete(session.sessionId);
  forgetSmokeViewportSession(session.sessionId);
}

function trimViewportSessions(preferredSessionId: string): void {
  while (viewportSessions.size > MAX_VIEWPORT_SESSIONS) {
    let oldest: ViewportSession | null = null;
    for (const candidate of viewportSessions.values()) {
      if (candidate.sessionId === preferredSessionId) continue;
      if (!oldest || candidate.sequenceUpdatedAt < oldest.sequenceUpdatedAt) oldest = candidate;
    }
    if (!oldest) return;
    evictViewportSession(oldest, "viewport registry is full");
  }
}

export function viewportSession(sessionId: string): ViewportSession {
  let session = viewportSessions.get(sessionId);
  if (session) return session;
  session = newViewportSession(sessionId);
  viewportSessions.set(sessionId, session);
  trimViewportSessions(sessionId);
  return session;
}

export function persistViewportIntents(): void {
  const storage = safeSessionStorage();
  if (!storage) return;
  try {
    const records: Array<Record<string, unknown>> = [];
    for (const session of viewportSessions.values()) {
      if (session.sequenceFloor <= 0n) continue;
      records.push({
        sessionId: session.sessionId,
        sequence: session.sequenceFloor.toString(),
        updatedAt: session.sequenceUpdatedAt,
        watermarkOnly: true,
      });
    }
    records.sort((a, b) => Number(b.updatedAt) - Number(a.updatedAt));
    records.length = Math.min(records.length, MAX_VIEWPORT_SESSIONS);
    storage.setItem(STORAGE_KEY, JSON.stringify(records));
  } catch {
    // Storage can be denied or quota-limited; in-memory ownership remains live.
  }
}

function restoreViewportIntents(): void {
  const storage = safeSessionStorage();
  if (!storage) return;
  try {
    const decoded = JSON.parse(storage.getItem(STORAGE_KEY) ?? "[]") as Array<Record<string, unknown>>;
    for (const value of decoded.slice(0, MAX_VIEWPORT_SESSIONS)) {
      if (typeof value.sessionId !== "string"
        || typeof value.sequence !== "string"
        || typeof value.updatedAt !== "number") continue;
      const sequence = BigInt(value.sequence);
      if (sequence <= 0n) continue;
      viewportSessions.set(value.sessionId, newViewportSession(value.sessionId, sequence, value.updatedAt));
    }
  } catch {
    storage.removeItem(STORAGE_KEY);
  }
}

export function updateViewportSequence(
  session: ViewportSession,
  desired: ViewportDesired,
  sequence: bigint,
): void {
  desired.sequence = sequence;
  desired.needsSequenceAdvance = false;
  session.sequenceFloor = sequence;
  session.sequenceUpdatedAt = Date.now();
  trimViewportSessions(session.sessionId);
  persistViewportIntents();
}

/** Move the persisted sequence watermark forward without changing the live
 * desired claim. The caller decides when that claim should be redispatched. */
export function rebaseViewportSequenceFloor(session: ViewportSession, sequenceFloor: bigint): boolean {
  if (sequenceFloor <= session.sequenceFloor) return false;
  session.sequenceFloor = sequenceFloor;
  session.sequenceUpdatedAt = Date.now();
  trimViewportSessions(session.sessionId);
  persistViewportIntents();
  return true;
}

export function advanceViewportSequence(session: ViewportSession, desired: ViewportDesired): void {
  updateViewportSequence(session, desired, session.sequenceFloor + 1n);
}

/** Seed a viewport already committed by optimistic spawn. The next equivalent
 * INITIAL claim adopts it without issuing a redundant wire command. */
export function seedTerminalViewportIntent(
  sessionId: string,
  sequence: bigint,
  cols: number,
  rows: number,
  cause: number,
): void {
  if (sequence <= 0n
    || !Number.isSafeInteger(cols)
    || !Number.isSafeInteger(rows)
    || !Number.isSafeInteger(cause)
    || cols <= 0
    || rows <= 0
    || cols > MAX_WIRE_DIMENSION
    || rows > MAX_WIRE_DIMENSION
    || cause < 0) return;
  const session = viewportSession(sessionId);
  if (sequence < session.sequenceFloor || (session.desired?.sequence ?? 0n) > sequence) return;
  if (session.desired) supersedeViewportDesired(session, "optimistic viewport preclaim replaced the prior intent");
  clearViewportAttempt(session);
  clearViewportRetry(session);
  session.preclaim = {
    sequence,
    cols,
    rows,
    cause,
    updatedAt: Date.now(),
  };
  session.sequenceFloor = sequence;
  session.sequenceUpdatedAt = Date.now();
  session.confirmed = null;
  trimViewportSessions(sessionId);
  persistViewportIntents();
}

/** The registry's record for a session, or undefined if it holds none. Callers
 * that must NOT create one — control results, generation reconciliation, the
 * diagnostic snapshot — use this instead of viewportSession(). */
export function existingViewportSession(sessionId: string): ViewportSession | undefined {
  return viewportSessions.get(sessionId);
}

/** Is this record still the registry's entry for its session? Every deferred
 * callback re-checks it, so a late result deadline or retry timer can never
 * mutate a record the registry has already replaced or evicted. */
export function isCurrentViewportSession(session: ViewportSession): boolean {
  return viewportSessions.get(session.sessionId) === session;
}

/** Live iteration over every registered session. The generation reconciler
 * walks this while dispatching, and dispatching can trim OTHER sessions
 * mid-walk, so the iterator stays live rather than a copy. */
export function allViewportSessions(): Iterable<ViewportSession> {
  return viewportSessions.values();
}

/** Bounded on-demand view of one session's desired/confirmed viewport
 * ownership. Stale-generation confirmation is never reported as current. `sync`
 * is threaded in so this view and its caller read one Sync identity. */
export function viewportClaimSnapshot(
  sessionId: string,
  sync: SyncV2TerminalState | null,
): TerminalViewportClaimSnapshot {
  const session = viewportSessions.get(sessionId);
  const desired = session?.desired;
  const preclaim = session?.preclaim;
  const confirmed = session?.confirmed;
  const confirmedCurrent = confirmed
    && sync?.ready === true
    && confirmed.socketId === sync.socketId
    && confirmed.domainGeneration === sync.domainGeneration
    ? confirmed
    : null;
  const attempt = session?.attempt;
  const attemptCurrent = attempt
    && sync?.ready === true
    && attempt.socketId === sync.socketId
    && attempt.domainGeneration === sync.domainGeneration
    ? attempt
    : null;
  return {
    owner_token: session?.ownerToken?.toString() ?? null,
    sequence_floor: session?.sequenceFloor.toString() ?? "0",
    status: session?.status?.status ?? null,
    desired: desired ? {
      client_seq: desired.sequence.toString(),
      cols: desired.cols,
      rows: desired.rows,
      cause: desired.cause,
      held_cell_seq: desired.heldCellSeq.toString(),
      updated_at_ms: desired.updatedAt,
    } : preclaim ? {
      client_seq: preclaim.sequence.toString(),
      cols: preclaim.cols,
      rows: preclaim.rows,
      cause: preclaim.cause,
      held_cell_seq: "0",
      updated_at_ms: preclaim.updatedAt,
    } : null,
    confirmed: confirmedCurrent ? {
      client_seq: confirmedCurrent.sequence.toString(),
      socket_id: confirmedCurrent.socketId,
      domain_generation: confirmedCurrent.domainGeneration.toString(),
      effective_cols: confirmedCurrent.effectiveCols,
      effective_rows: confirmedCurrent.effectiveRows,
    } : null,
    attempt: attemptCurrent ? {
      client_seq: attemptCurrent.sequence.toString(),
      socket_id: attemptCurrent.socketId,
      domain_generation: attemptCurrent.domainGeneration.toString(),
      phase: attemptCurrent.phase,
      deadline_at_ms: attemptCurrent.deadlineAt,
    } : null,
    retry: session?.retryAt !== null && session?.retryAt !== undefined && session.retryReason
      ? { at_ms: session.retryAt, reason: session.retryReason }
      : null,
  };
}

/** Release a closed session's viewport ownership, settling any live claim. */
export function pruneViewportSession(sessionId: string): void {
  const session = viewportSessions.get(sessionId);
  if (session) evictViewportSession(session, "session closed");
}

/** Viewport half of the outbound test reset. Runs before the input half so the
 * eviction rejections resolve in the same order the single-file version had. */
export function _resetViewportOutboundForTest(): void {
  for (const session of [...viewportSessions.values()]) {
    evictViewportSession(session, "test reset");
  }
  viewportSessions.clear();
  safeSessionStorage()?.removeItem(STORAGE_KEY);
}

restoreViewportIntents();
