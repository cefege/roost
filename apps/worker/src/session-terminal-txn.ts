// The viewport transaction: a stage-aware state machine over the keeper.
//
// The phases are observable (session-resize-capture.ts::TerminalTxnPhase) and
// each one carries its own bounded deadline, clamped by the transaction's overall
// ceiling. The phase — not an exception's shape — decides how a failure is
// reported:
//
//   validating     nothing mutated yet         → a failure is a DEFINITE rejection
//   admitted       claim/capture installed     → ambiguous unless the keeper
//   keeper_written resize request written         proves the PTY was never resized
//   pty_resized    keeper ACK proves geometry  → committed, rebuild pending
//   grid_rebuilt   core swapped exactly once
//   settled        gate cleared, result reported
//
// A definite rejection therefore requires PROOF (nothing written, or a typed
// keeper rejection), never merely "we did not hear back".

import { asChannelId } from "@roost/shared/wire";
import type { SessionManager } from "./session-manager.ts";
import type { ViewportClaim } from "./session-record.ts";
import type { WorkerViewportIntent, WorkerViewportResult } from "./session-terminal-control.ts";
import type { KeeperAdmissionTicket } from "./session-control-lanes.ts";
import type { KeeperResizeResult } from "./keeper/multiplexed-client.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import {
  advanceViewportIntentEpoch,
  desiredViewportSize,
  needsClaimSnapshot,
} from "./session-viewport.ts";
import {
  clearResizeCapture,
  enterPhase,
  installResizeCapture,
  markResizeBoundary,
  phaseRemainingMs,
  rebuildTerminalCore,
  type ResizeCapture,
} from "./session-resize-capture.ts";
import {
  FLOOR_REVALIDATE_BUDGET_MS,
  floorValid,
  type ProvenSize,
  resolveResizeFloor,
  revalidateResizeFloor,
  sameViewportSize,
  withinPhase,
} from "./session-resize-floor.ts";

const BACKGROUND_CAUSE = 5;
const SNAPSHOT_CAUSES: Readonly<Record<number, true>> = { 1: true, 3: true, 6: true };
/** Status probes per transaction. The per-command watchdog is 2.5 s and the
 *  reconciliation phase 6 s, so this is a belt on top of a bounded budget. */
const RESIZE_MAX_STATUS_PROBES = 3;

/** What the keeper proved about one logical resize sequence. */
type ResizeOutcome =
  | { kind: "no_resize_needed"; seq: number; size: ProvenSize }
  | { kind: "known_applied"; seq: number; size: ProvenSize }
  | { kind: "known_rejected"; seq: number; reason: string }
  | { kind: "not_admitted"; reason: string }
  | { kind: "admitted_unknown"; seq: number; reason: string };

/** Identity-guarded claim restoration. It may put the claim map back, but the
 *  caller must never use it to relabel an already-resized PTY as rejected. */
function restoreViewerClaim(
  mgr: SessionManager,
  channelId: number,
  viewerId: string,
  installed: ViewportClaim | null,
  prior: ViewportClaim | undefined,
): void {
  const claims = mgr.viewportClaims.get(channelId);
  if (!claims) return;
  if (installed === null) {
    if (claims.has(viewerId)) return;
  } else if (claims.get(viewerId) !== installed) return;
  if (prior) claims.set(viewerId, prior);
  else claims.delete(viewerId);
}

/** Drive one logical resize to a typed outcome inside the reconciliation budget.
 *  The admission ticket is released the moment the request that can apply the
 *  newest geometry has been WRITTEN — never when its ACK returns, and never for
 *  a status query alone. */
async function reconcileResize(
  mgr: SessionManager,
  channelId: number,
  capture: ResizeCapture,
  ticket: KeeperAdmissionTicket,
  desired: ProvenSize,
  forceResize: boolean,
): Promise<ResizeOutcome> {
  const pool = getMultiplexedPool();
  const floor = await resolveResizeFloor(mgr, channelId, capture);
  if (!forceResize && floor.proven
      && floor.proven.cols === desired.cols && floor.proven.rows === desired.rows) {
    // A prior written resize already applies the newest desired geometry.
    return { kind: "no_resize_needed", seq: floor.seq, size: floor.proven };
  }
  const seq = floor.seq + 1;
  const command = pool.beginResize(
    channelId,
    seq,
    desired.cols,
    desired.rows,
    () => markResizeBoundary(mgr, channelId, capture),
  );
  if (!command.admission.written) {
    return { kind: "not_admitted", reason: `keeper did not accept the resize request: ${command.admission.reason}` };
  }
  // Reserved on WRITE, not on ACK: a lost result can never let the next
  // allocation reuse this sequence with conflicting dimensions.
  mgr.channelResizeSeq.set(channelId, seq);
  mgr.resizeFloorInvalid.add(channelId);
  enterPhase(capture, "keeper_written");
  ticket.release();

  const expired: KeeperResizeResult = { kind: "unknown", seq, reason: "timeout" };
  let result = await withinPhase(capture, command.result, expired);
  let probes = 0;
  while (result.kind === "unknown" && probes < RESIZE_MAX_STATUS_PROBES && phaseRemainingMs(capture) > 0) {
    if (!mgr.sessions.has(channelId)) {
      return { kind: "admitted_unknown", seq, reason: "session closed while the resize was unresolved" };
    }
    probes++;
    const probe = pool.beginResizeStatus(channelId, seq, () => markResizeBoundary(mgr, channelId, capture));
    if (!probe.admission.written) break;
    result = await withinPhase(capture, probe.result, expired);
  }
  if (result.kind === "ack") {
    if (result.seq !== seq || result.cols !== desired.cols || result.rows !== desired.rows) {
      return { kind: "admitted_unknown", seq, reason: "keeper returned a conflicting resize status" };
    }
    mgr.resizeFloorInvalid.delete(channelId);
    return { kind: "known_applied", seq, size: { cols: result.cols, rows: result.rows } };
  }
  if (result.kind === "reject") {
    // A typed rejection is the ONLY proof that the PTY was not resized.
    return { kind: "known_rejected", seq, reason: `keeper rejected the resize: ${result.reason}` };
  }
  return { kind: "admitted_unknown", seq, reason: `keeper resize result unresolved: ${result.reason}` };
}

/** The size to rebuild at. A proven outcome names it; otherwise ask the keeper.
 *  When authority is unreachable the last proven size is used, the floor stays
 *  invalid, and a repair marker is left — cells are never left gated. */
async function authoritativeSize(
  mgr: SessionManager,
  channelId: number,
  capture: ResizeCapture,
  outcome: ResizeOutcome,
  fallback: ProvenSize,
): Promise<ProvenSize> {
  if (outcome.kind === "known_applied" || outcome.kind === "no_resize_needed") return outcome.size;
  const state = await withinPhase(capture, getMultiplexedPool().getTerminalState(channelId), null);
  if (state) {
    mgr.channelResizeSeq.set(channelId, Math.max(mgr.channelResizeSeq.get(channelId) ?? 0, state.highestResizeSeq));
    if (state.appliedResizeSeq > 0) {
      mgr.lastAppliedSize.set(channelId, { cols: state.cols, rows: state.rows });
      mgr.resizeFloorInvalid.delete(channelId);
      return { cols: state.cols, rows: state.rows };
    }
    return fallback;
  }
  mgr.resizeFloorInvalid.add(channelId);
  mgr.pendingCellRepairs.add(channelId);
  return fallback;
}

/** Geometry half of the transaction, shared by typed claims and the
 *  withdraw/freshness reconcile. Installs the capture before the first keeper
 *  operation, rebuilds exactly once at the authoritative size, and clears the
 *  capture + gate in `finally` on every path including a thrown one. */
async function applyProvenGeometry(
  mgr: SessionManager,
  channelId: number,
  ticket: KeeperAdmissionTicket,
  reason: string,
  desired: ProvenSize,
  forceResize = false,
): Promise<ResizeOutcome> {
  const capture = installResizeCapture(mgr, channelId, reason);
  let outcome: ResizeOutcome = { kind: "not_admitted", reason: "transaction did not run" };
  try {
    outcome = await reconcileResize(mgr, channelId, capture, ticket, desired, forceResize);
    if (outcome.kind === "known_applied" || outcome.kind === "no_resize_needed") {
      mgr.lastAppliedSize.set(channelId, outcome.size);
      enterPhase(capture, "pty_resized");
    }
    const size = await authoritativeSize(mgr, channelId, capture, outcome, {
      cols: mgr.lastAppliedSize.get(channelId)?.cols
        ?? mgr.sessions.get(channelId)?.wtermCore.getCols() ?? desired.cols,
      rows: mgr.lastAppliedSize.get(channelId)?.rows
        ?? mgr.sessions.get(channelId)?.wtermCore.getRows() ?? desired.rows,
    });
    await rebuildTerminalCore(mgr, channelId, size.cols, size.rows, capture);
    enterPhase(capture, "grid_rebuilt");
    return outcome;
  } catch (error) {
    // Past the capture install every unexpected failure is ambiguous: the
    // request may already have resized the PTY.
    return {
      kind: "admitted_unknown",
      seq: mgr.channelResizeSeq.get(channelId) ?? 0,
      reason: error instanceof Error ? error.message : String(error),
    };
  } finally {
    enterPhase(capture, "settled");
    clearResizeCapture(mgr, channelId);
    ticket.release();
    // One authoritative full frame for the replacement core. A delta is
    // meaningless across a core swap, and this is now the ONLY forced frame in
    // the emitter's synchronized-output story — a hold's own flush ships
    // unforced. ORDER IS LOAD-BEARING: emitCellFrame checks the emission gate
    // BEFORE the force bypass, so a snapshot raised before clearResizeCapture is
    // discarded silently and the browser keeps the pre-resize grid.
    if (mgr.sessions.has(channelId)) mgr.emitCellSnapshot(asChannelId(channelId));
  }
}

export type ResumeRedrawResult =
  | { status: "committed"; resizeSeq: number }
  | { status: "rejected"; resizeSeq: number; reason: string }
  | { status: "ambiguous"; resizeSeq: number; reason: string };

/** Forced resume redraw: restoration must not collapse to a proven-size no-op
 * after an ambiguous nudge; ordinary viewport transactions still may. */
export async function applyResumeRedrawNow(
  mgr: SessionManager,
  channelId: number,
  ticket: KeeperAdmissionTicket,
  desired: ProvenSize,
): Promise<ResumeRedrawResult> {
  if (!mgr.sessions.has(channelId)) {
    ticket.release();
    return { status: "rejected", resizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0, reason: "session is not live" };
  }
  const outcome = await applyProvenGeometry(mgr, channelId, ticket, "resume_eviction_redraw", desired, true);
  const resizeSeq = mgr.channelResizeSeq.get(channelId) ?? 0;
  if (outcome.kind === "known_applied" || outcome.kind === "no_resize_needed")
    return { status: "committed", resizeSeq };
  if (outcome.kind === "known_rejected" || outcome.kind === "not_admitted")
    return { status: "rejected", resizeSeq, reason: outcome.reason };
  return { status: "ambiguous", resizeSeq, reason: outcome.reason };
}

/** Withdraw / freshness-reap / SCD recompute. Same owner, same capture, same
 *  once-only rebuild as a typed claim — the old parallel path sent an
 *  unacknowledged legacy resize and scheduled its own uncorrelated rebuild, so
 *  two sizes could race and two cores could be built for one decision. */
export async function reconcileViewportNow(
  mgr: SessionManager,
  channelId: number,
  ticket: KeeperAdmissionTicket,
): Promise<void> {
  const rec = mgr.sessions.get(channelId);
  if (!rec) {
    // Session gone — drop leftovers so the maps cannot grow across spawn/kill.
    mgr.viewportClaims.delete(channelId);
    mgr.lastAppliedSize.delete(channelId);
    mgr.viewportIntentEpoch.delete(channelId);
    ticket.release();
    return;
  }
  const desired = desiredViewportSize.call(mgr, channelId);
  // The core's own dimensions are proven geometry when no resize has been
  // recorded yet: spawn/resume created the PTY and the core at the same size.
  // Treating "unknown" as "must resize" would install a capture and reconcile a
  // resize that changes nothing on the very first claim.
  const proven = mgr.lastAppliedSize.get(channelId)
    ?? { cols: rec.wtermCore.getCols(), rows: rec.wtermCore.getRows() };
  // No fresh sizing viewer: leave the PTY at its last size rather than resizing
  // a running TUI to a default.
  if (!desired || (floorValid(mgr, channelId) && sameViewportSize(desired, proven))) {
    ticket.release();
    return;
  }
  await applyProvenGeometry(mgr, channelId, ticket, "viewport_reconcile", desired);
}

export async function applyViewportNow(
  mgr: SessionManager,
  channelId: number,
  intent: WorkerViewportIntent,
  ticket: KeeperAdmissionTicket,
): Promise<WorkerViewportResult> {
  // ── validating ── nothing below this block may have mutated state.
  const rec = mgr.sessions.get(channelId);
  if (!rec || rec.sessionId !== intent.sessionId) {
    ticket.release();
    return { status: "rejected", reason: "session is not live" };
  }
  if (intent.budget && !intent.budget.isCurrentConnection()) {
    ticket.release();
    return { status: "rejected", reason: "worker connection superseded before the viewport was applied" };
  }
  if (intent.budget && intent.budget.remainingMs() <= 0) {
    ticket.release();
    return { status: "rejected", reason: "viewport budget expired before any keeper operation" };
  }
  let claims = mgr.viewportClaims.get(channelId);
  if (!claims) {
    claims = new Map();
    mgr.viewportClaims.set(channelId, claims);
  }
  const prior = claims.get(intent.viewerId);
  const priorSeq = prior?.clientSeq ?? -1n;
  const isBackground = intent.cause === BACKGROUND_CAUSE;
  const withdraw = !isBackground && (intent.cols <= 0 || intent.rows <= 0);
  if (intent.clientSeq < priorSeq) {
    ticket.release();
    return { status: "rejected", reason: "stale viewport sequence" };
  }
  if (intent.clientSeq === priorSeq) {
    const equivalent = withdraw ? prior === undefined : prior?.cols === intent.cols && prior.rows === intent.rows;
    if (!equivalent) {
      ticket.release();
      return { status: "rejected", reason: "conflicting viewport sequence" };
    }
    ticket.release();
    if (prior) prior.lastMs = Date.now();
    if (SNAPSHOT_CAUSES[intent.cause]
      && needsClaimSnapshot(mgr, channelId, Number(intent.heldCellSeq), claims.size > 0)) {
      mgr.emitCellSnapshot(asChannelId(channelId));
    }
    const size = mgr.lastAppliedSize.get(channelId)
      ?? { cols: rec.wtermCore.getCols(), rows: rec.wtermCore.getRows() };
    return {
      status: "committed",
      channelResizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0,
      cols: size.cols,
      rows: size.rows,
      resized: false,
    };
  }

  // ── admitted ── the claim is installed; a definite rejection from here on
  // requires proof that the PTY was never resized.
  const wasStreaming = claims.size > 0;
  const shouldSnapshot = !withdraw && needsClaimSnapshot(mgr, channelId, Number(intent.heldCellSeq), wasStreaming);
  const installed: ViewportClaim | null = withdraw ? null : {
    cols: intent.cols,
    rows: intent.rows,
    lastMs: Date.now(),
    clientSeq: intent.clientSeq,
  };
  mgr._cancelPendingWithdraw(channelId, intent.viewerId);
  if (installed) claims.set(intent.viewerId, installed);
  else claims.delete(intent.viewerId);

  const desired = desiredViewportSize.call(mgr, channelId);
  let current = mgr.lastAppliedSize.get(channelId)
    ?? { cols: rec.wtermCore.getCols(), rows: rec.wtermCore.getRows() };
  // A claim that issues no resize consumes no resize sequence, so an invalid
  // floor is a reason to re-READ the keeper — never to freeze the core and
  // rebuild it at a geometry nothing changed (which also mints a gridEpoch and
  // throws the browser's painted history away). The revalidation installs no
  // capture, so the live core keeps parsing at its proven size; if it cannot
  // prove the floor, fall through to the full transaction exactly as before.
  if (!floorValid(mgr, channelId) && sameViewportSize(desired, current)) {
    await revalidateResizeFloor(mgr, channelId, Math.min(
      FLOOR_REVALIDATE_BUDGET_MS,
      intent.budget?.remainingMs() ?? FLOOR_REVALIDATE_BUDGET_MS,
    ));
    current = mgr.lastAppliedSize.get(channelId) ?? current;
  }
  if (floorValid(mgr, channelId) && sameViewportSize(desired, current)) {
    // Locally proven no-resize: no capture, no rebuild, and the write lane is
    // free the moment that decision is final.
    ticket.release();
    if (shouldSnapshot) mgr.emitCellSnapshot(asChannelId(channelId));
    advanceViewportIntentEpoch(mgr, channelId);
    return {
      status: "committed",
      channelResizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0,
      cols: current.cols,
      rows: current.rows,
      resized: false,
    };
  }

  const outcome = await applyProvenGeometry(
    mgr,
    channelId,
    ticket,
    withdraw ? "viewport_withdraw" : "viewport_resize",
    desired ?? current,
  );
  const seq = mgr.channelResizeSeq.get(channelId) ?? 0;
  if (outcome.kind === "known_applied" || outcome.kind === "no_resize_needed") {
    advanceViewportIntentEpoch(mgr, channelId);
    return {
      status: "committed",
      channelResizeSeq: outcome.seq,
      cols: outcome.size.cols,
      rows: outcome.size.rows,
      resized: outcome.size.cols !== current.cols || outcome.size.rows !== current.rows,
    };
  }
  if (outcome.kind === "known_rejected" || outcome.kind === "not_admitted") {
    // Proven no resize: the claim map may be restored, and the coordinator may
    // treat this as definite.
    restoreViewerClaim(mgr, channelId, intent.viewerId, installed, prior);
    return { status: "rejected", reason: outcome.reason };
  }
  advanceViewportIntentEpoch(mgr, channelId);
  return { status: "ambiguous", reason: `resize seq ${seq} unresolved: ${outcome.reason}` };
}
