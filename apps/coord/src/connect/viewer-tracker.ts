// Viewer-presence tracker — source of truth for "which browsers are looking
// at which session". Driven by ordered viewport claims: explicit WITHDRAW is
// deferred, while BACKGROUND remains present even at 0×0. Fans out
// {kind:"viewers", fps:[...]} on globalPresenceBus for the SPA projection.
// VIEWER_TTL_MS + VIEWER_WITHDRAW_GRACE_MS are shared with the worker so its
// PTY claims and the coordinator's presence projection converge together.
//
// Module-level singleton state starts the TTL reaper and session-close
// subscription. viewport-control.ts owns transactional mutation; router wiring
// and diagnostics read the projection, while auth/pair handlers invalidate
// labels.

import type { KyselyDB } from "../db/connection.ts";
import { globalPresenceBus, sessionBus } from "../buses.ts";
import { resolveHostname } from "../tailnet-resolver.ts";
import {
  VIEWER_WITHDRAW_GRACE_MS,
  VIEWER_REAP_INTERVAL_MS,
  VIEWER_CLAIM_TTL_MS as VIEWER_TTL_MS,
} from "@roost/shared/viewport";

const MAX_DIAGNOSTIC_CLIENT_SEQ = BigInt(Number.MAX_SAFE_INTEGER);

interface ViewerEntry {
  readonly cols: number;
  readonly rows: number;
  readonly lastMs: number;
  // Diagnostics remain JSON-safe; ordering uses ViewerState.clientSeq.
  readonly clientSeq: number;
  readonly lastIp?: string;
  readonly callerFp?: string;
}

interface ViewerState {
  readonly subscribed: boolean;
  readonly clientSeq: bigint;
  readonly lastMs: number;
  readonly entry: ViewerEntry | null;
  readonly withdrawDeadlineMs: number | null;
  timer: Timer | null;
}

/** One provisional viewer mutation. Rollback is conditional on this mutation
 * still being current, so an older result cannot restore over a successor. */
export interface ViewerMutation {
  readonly effectiveClientSeq: bigint;
  isCurrent(): boolean;
  rollback(): boolean;
}

export const _viewersBySession = new Map<string, Map<string, ViewerEntry>>();
const _viewerStatesBySession = new Map<string, Map<string, ViewerState>>();

// fp → authorized_keys.label cache. Populated lazily on first publish.
// Invalidated whenever a label changes via _invalidateLabel (called from
// authAuthorizeBrowser + pairApprove). _viewerTrackerDb is wired once from
// buildConnectRouter — keeps callsites grep-able.
const _labelByFp = new Map<string, string>();
let _viewerTrackerDb: KyselyDB | null = null;
export function _setViewerTrackerDb(db: KyselyDB): void { _viewerTrackerDb = db; }
export function _invalidateLabel(fp: string): void { _labelByFp.delete(fp); }
async function _labelFor(fp: string): Promise<string | undefined> {
  const cached = _labelByFp.get(fp);
  if (cached !== undefined) return cached;
  if (!_viewerTrackerDb) return undefined;
  try {
    const row = await _viewerTrackerDb.selectFrom("authorized_keys").select("label")
      .where("fingerprint", "=", fp).executeTakeFirst();
    if (row?.label) _labelByFp.set(fp, row.label);
    return row?.label ?? undefined;
  } catch {
    // Presence delivery is operational state; optional labels must not block it
    // during coordinator teardown or a transient database failure.
    return undefined;
  }
}

// Combine browser self-label + tailnet hostname (reverse-resolved from
// the client's tailnet IP via apps/coord/src/tailnet-resolver.ts). User
// chose AUGMENT (not replace): "Chrome — macOS on <tailnet-host>". Either
// piece may be missing → graceful fallback to whichever is present, or
// to the 8-char fp prefix (SPA-side fallback in ViewersChip).
function _composeViewerLabel(self: string | undefined, host: string | null): string | undefined {
  if (self && host) return `${self} on ${host}`;
  return self ?? host ?? undefined;
}

async function _publishViewers(sessionId: string): Promise<void> {
  // Label and hostname lookup is asynchronous. Fence the eventual publish by
  // the exact entry identities captured here, so a slow older snapshot cannot
  // land after a successor mutation or rollback.
  const snapshot = Array.from(
    _viewersBySession.get(sessionId)?.entries() ?? [],
  );
  const entries = await Promise.all(snapshot.map(async ([fp, entry]) => ({
    fp,
    cols: entry.cols,
    rows: entry.rows,
    lastMs: entry.lastMs,
    label: _composeViewerLabel(
      await _labelFor(entry.callerFp ?? fp),
      resolveHostname(entry.lastIp),
    ),
  })));
  const current = _viewersBySession.get(sessionId);
  if (
    (current?.size ?? 0) !== snapshot.length
    || snapshot.some(([fp, entry]) => current?.get(fp) !== entry)
  ) {
    return;
  }
  globalPresenceBus.publish({
    session_id: sessionId,
    data: {
      kind: "viewers",
      fps: entries.map((entry) => entry.fp),
      entries,
    },
  });
}

function viewerStates(sessionId: string): Map<string, ViewerState> {
  let states = _viewerStatesBySession.get(sessionId);
  if (!states) {
    states = new Map();
    _viewerStatesBySession.set(sessionId, states);
  }
  return states;
}

function clearViewerTimer(state: ViewerState): void {
  if (state.timer !== null) clearTimeout(state.timer);
  state.timer = null;
}

function removeProjectedViewer(
  sessionId: string,
  viewerFp: string,
  expected?: ViewerEntry | null,
): boolean {
  const viewers = _viewersBySession.get(sessionId);
  if (!viewers) return false;
  if (expected !== undefined && viewers.get(viewerFp) !== expected) return false;
  if (!viewers.delete(viewerFp)) return false;
  if (viewers.size === 0) _viewersBySession.delete(sessionId);
  return true;
}

function installProjectedViewer(
  sessionId: string,
  viewerFp: string,
  entry: ViewerEntry,
): boolean {
  let viewers = _viewersBySession.get(sessionId);
  if (!viewers) {
    viewers = new Map();
    _viewersBySession.set(sessionId, viewers);
  }
  const changed = viewers.get(viewerFp) !== entry;
  viewers.set(viewerFp, entry);
  return changed;
}

function activateViewerState(
  sessionId: string,
  viewerFp: string,
  state: ViewerState,
): boolean {
  clearViewerTimer(state);
  if (state.subscribed) {
    return state.entry !== null
      && installProjectedViewer(sessionId, viewerFp, state.entry);
  }

  const entry = state.entry;
  const deadline = state.withdrawDeadlineMs ?? Date.now();
  if (entry === null || deadline <= Date.now()) {
    return removeProjectedViewer(sessionId, viewerFp, entry);
  }
  const changed = installProjectedViewer(sessionId, viewerFp, entry);
  state.timer = setTimeout(() => {
    state.timer = null;
    const current = _viewerStatesBySession.get(sessionId)?.get(viewerFp);
    if (current !== state) return;
    if (removeProjectedViewer(sessionId, viewerFp, entry)) {
      void _publishViewers(sessionId);
    }
  }, Math.max(0, deadline - Date.now()));
  return changed;
}

function diagnosticClientSeq(clientSeq: bigint): number {
  if (clientSeq <= 0n) return 0;
  return Number(
    clientSeq > MAX_DIAGNOSTIC_CLIENT_SEQ
      ? MAX_DIAGNOSTIC_CLIENT_SEQ
      : clientSeq,
  );
}

/** Provisionally apply viewer membership at the same ordered watermark as the
 * cell subscription. The mutation remains TTL-backed on an ambiguous worker
 * result; only a definite non-admission or matching typed rejection rolls it
 * back. */
export function mutateViewer(
  sessionId: string,
  viewerFp: string,
  subscribed: boolean,
  cols: number,
  rows: number,
  clientSeq = 0n,
  refreshEqual = false,
  clientIp?: string,
  callerFp?: string,
): ViewerMutation | null {
  const states = viewerStates(sessionId);
  let prior = states.get(viewerFp);
  const projected = _viewersBySession.get(sessionId)?.get(viewerFp);
  if (!prior && projected) {
    prior = {
      subscribed: true,
      clientSeq: BigInt(projected.clientSeq),
      lastMs: projected.lastMs,
      entry: projected,
      withdrawDeadlineMs: null,
      timer: null,
    };
  }

  let equalRefresh = false;
  if (clientSeq > 0n && prior) {
    if (clientSeq < prior.clientSeq) return null;
    if (clientSeq === prior.clientSeq) {
      if (!refreshEqual || !subscribed || !prior.subscribed) return null;
      equalRefresh = true;
    }
  }

  const effectiveClientSeq = clientSeq > 0n
    ? clientSeq
    : subscribed
      ? (prior?.clientSeq ?? -1n) + 1n
      : (prior?.clientSeq ?? 0n);
  const now = Date.now();
  if (prior) clearViewerTimer(prior);

  const priorEntry = prior?.entry ?? projected ?? null;
  const appliedEntry: ViewerEntry | null = subscribed
    ? {
        cols: equalRefresh && priorEntry ? priorEntry.cols : cols,
        rows: equalRefresh && priorEntry ? priorEntry.rows : rows,
        lastMs: now,
        clientSeq: diagnosticClientSeq(effectiveClientSeq),
        lastIp: clientIp ?? priorEntry?.lastIp,
        callerFp: callerFp ?? priorEntry?.callerFp,
      }
    : priorEntry;
  const applied: ViewerState = {
    subscribed,
    clientSeq: effectiveClientSeq,
    lastMs: now,
    entry: appliedEntry,
    withdrawDeadlineMs: subscribed ? null : now + VIEWER_WITHDRAW_GRACE_MS,
    timer: null,
  };
  states.set(viewerFp, applied);
  const projectionChanged = activateViewerState(sessionId, viewerFp, applied);
  if (projectionChanged && (!equalRefresh || !projected)) {
    void _publishViewers(sessionId);
  }

  return {
    effectiveClientSeq,
    isCurrent(): boolean {
      return _viewerStatesBySession.get(sessionId)?.get(viewerFp) === applied;
    },
    rollback(): boolean {
      const currentStates = _viewerStatesBySession.get(sessionId);
      if (!currentStates || currentStates.get(viewerFp) !== applied) return false;
      clearViewerTimer(applied);
      if (prior) {
        currentStates.set(viewerFp, prior);
      } else {
        currentStates.delete(viewerFp);
        if (currentStates.size === 0) _viewerStatesBySession.delete(sessionId);
      }
      const removedApplied = removeProjectedViewer(
        sessionId,
        viewerFp,
        appliedEntry,
      );
      const restoredPrior = prior
        ? activateViewerState(sessionId, viewerFp, prior)
        : false;
      if (removedApplied || restoredPrior) void _publishViewers(sessionId);
      return true;
    },
  };
}

/** Legacy/non-transactional caller compatibility. Session control uses
 * mutateViewer directly so it can conditionally roll back. */
export function _bumpViewer(
  sessionId: string,
  viewerFp: string,
  cols: number,
  rows: number,
  clientSeq?: number | bigint,
  clientIp?: string,
  callerFp?: string,
): void {
  let orderedSeq = 0n;
  if (typeof clientSeq === "bigint") {
    orderedSeq = clientSeq;
  } else if (
    typeof clientSeq === "number"
    && Number.isSafeInteger(clientSeq)
    && clientSeq > 0
  ) {
    orderedSeq = BigInt(clientSeq);
  }
  mutateViewer(
    sessionId,
    viewerFp,
    cols > 0 && rows > 0,
    cols,
    rows,
    orderedSeq,
    true,
    clientIp,
    callerFp,
  );
}

setInterval(() => {
  const now = Date.now();
  for (const [sessionId, states] of _viewerStatesBySession) {
    let changed = false;
    for (const [viewerFp, state] of states) {
      if (now - state.lastMs <= VIEWER_TTL_MS) continue;
      clearViewerTimer(state);
      states.delete(viewerFp);
      changed = removeProjectedViewer(sessionId, viewerFp, state.entry) || changed;
    }
    if (states.size === 0) _viewerStatesBySession.delete(sessionId);
    if (changed) void _publishViewers(sessionId);
  }
}, VIEWER_REAP_INTERVAL_MS).unref?.();

// Session close fans through sessionBus as a `closed` SessionEvent.
// Drop viewer state immediately so a stale heartbeat cannot retain sidebar
// presence until the normal claim TTL.
sessionBus.subscribe((ev) => {
  if (ev.kind !== "closed") return;
  const sessionId = String(ev.session_id);
  const states = _viewerStatesBySession.get(sessionId);
  if (states) {
    for (const state of states.values()) clearViewerTimer(state);
    _viewerStatesBySession.delete(sessionId);
  }
  const hadViewers = _viewersBySession.delete(sessionId);
  if (hadViewers) void _publishViewers(sessionId);
});
