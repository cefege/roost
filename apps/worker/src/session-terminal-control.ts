// Typed terminal-control entry points: acknowledged PTY input and the viewport
// transaction. Both take the per-channel keeper-admission lane AT RECEIPT, which
// is what preserves the keeper's receive order — the coordinator guarantees a
// preceding viewport frame is written to the worker socket before a later input
// frame, and this lane carries that order through to the keeper writes.
//
// Input waits for that ordering boundary ONLY. It never waits for a resize ACK,
// a browser result, a core rebuild, or a cell repair: input blocked behind a
// pending control is the stall this file exists to prevent.

import type { SessionManager } from "./session-manager.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import {
  acquireKeeperAdmission,
  enqueueTerminalControl,
  type KeeperAdmissionTicket,
} from "./session-control-lanes.ts";
import {
  applyResumeRedrawNow,
  applyViewportNow,
  reconcileViewportNow,
  type ResumeRedrawResult,
} from "./session-terminal-txn.ts";

/** Outer report bound. Above the transaction ceiling (7 s) so a transaction that
 *  finished inside its own phase budgets always reports its truthful result, and
 *  under the coordinator's 8 s viewport-result timeout so the coordinator hears
 *  the worker rather than its own clock. A transaction still queued behind an
 *  earlier one is what this actually catches. */
const VIEWPORT_APPLICATION_RESULT_BUDGET_MS = 7_500;
const VIEWPORT_APPLICATION_DEADLINE_REASON = "viewport application result deadline exceeded";

export type WorkerInputResult =
  | { status: "accepted"; writtenBytes: number }
  | { status: "rejected"; writtenBytes: 0; reason: string }
  | { status: "ambiguous"; writtenBytes: number; reason: string };

export type WorkerViewportResult =
  | { status: "committed"; channelResizeSeq: number; cols: number; rows: number; resized: boolean }
  | { status: "rejected"; reason: string }
  | { status: "ambiguous"; reason: string };

export interface WorkerViewportIntent {
  sessionId: string;
  viewerId: string;
  clientSeq: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  /** Hop-local monotonic budget from the delivering transport. Absent for the
   *  in-process browser-command path, which has no coordinator waiter. */
  budget?: TerminalRequestBudget;
}

export async function writeTerminalInput(
  this: SessionManager,
  sessionId: string,
  inputSeq: bigint,
  bytes: Uint8Array,
  budget?: TerminalRequestBudget,
): Promise<WorkerInputResult> {
  const rec = this.getBySessionId(sessionId);
  if (!rec) return { status: "rejected", writtenBytes: 0, reason: "session is not live" };
  if (bytes.byteLength === 0) return { status: "accepted", writtenBytes: 0 };
  if (inputSeq <= 0n || inputSeq > BigInt(Number.MAX_SAFE_INTEGER)) {
    return { status: "rejected", writtenBytes: 0, reason: "input sequence exceeds keeper protocol range" };
  }
  const channelId = rec.channelId;
  const ticket = acquireKeeperAdmission(this, channelId, "terminal_input");
  const owned = bytes.slice();
  let command;
  try {
    await ticket.granted;
    // Re-check immediately before the write, never from a value snapshotted at
    // entry: queue time is exactly what these guards are for. All three are
    // pre-write, so their failures are DEFINITE rejections.
    if (!this.sessions.has(channelId)) {
      return { status: "rejected", writtenBytes: 0, reason: "session closed before the keeper write" };
    }
    if (budget && !budget.isCurrentConnection()) {
      return { status: "rejected", writtenBytes: 0, reason: "worker connection superseded before the keeper write" };
    }
    if (budget && budget.remainingMs() <= 0) {
      return { status: "rejected", writtenBytes: 0, reason: "input budget expired before the keeper write" };
    }
    this.markInputSensitive(channelId);
    command = getMultiplexedPool().beginInput(channelId, Number(inputSeq), owned);
    if (!command.admission.written) {
      return { status: "rejected", writtenBytes: 0, reason: `keeper did not accept the input: ${command.admission.reason}` };
    }
  } catch (error) {
    // A throw before/at the write cannot prove the bytes reached the keeper.
    return {
      status: "ambiguous",
      writtenBytes: 0,
      reason: error instanceof Error ? error.message : String(error),
    };
  } finally {
    // The lane orders WRITES. Holding it for the ACK would put every later input
    // behind this one's round trip.
    ticket.release();
  }

  try {
    const result = await command.result;
    if (result.kind === "ack") {
      return result.writtenBytes === owned.byteLength
        ? { status: "accepted", writtenBytes: result.writtenBytes }
        : { status: "ambiguous", writtenBytes: result.writtenBytes, reason: "keeper acknowledged an incomplete input batch" };
    }
    if (result.kind === "reject") {
      // The keeper proves nothing reached the PTY.
      return { status: "rejected", writtenBytes: 0, reason: result.reason };
    }
    return { status: "ambiguous", writtenBytes: result.writtenBytes ?? 0, reason: result.reason };
  } catch (error) {
    return { status: "ambiguous", writtenBytes: 0, reason: error instanceof Error ? error.message : String(error) };
  }
}

function reportViewportResultWithinBudget(
  operation: Promise<WorkerViewportResult>,
): Promise<WorkerViewportResult> {
  const { promise, resolve } = Promise.withResolvers<WorkerViewportResult>();
  let reported = false;
  const finish = (result: WorkerViewportResult): void => {
    if (reported) return;
    reported = true;
    clearTimeout(timer);
    resolve(result);
  };
  const timer = setTimeout(
    () => finish({ status: "ambiguous", reason: VIEWPORT_APPLICATION_DEADLINE_REASON }),
    VIEWPORT_APPLICATION_RESULT_BUDGET_MS,
  );
  void operation.then(
    finish,
    (error) => finish({
      status: "ambiguous",
      reason: error instanceof Error ? error.message : String(error),
    }),
  );
  return promise;
}

export function applyTerminalViewport(
  this: SessionManager,
  intent: WorkerViewportIntent,
): Promise<WorkerViewportResult> {
  const rec = this.getBySessionId(intent.sessionId);
  if (!rec) return Promise.resolve({ status: "rejected", reason: "session is not live" });
  const channelId = rec.channelId;
  // Receipt order, not run order: the ticket is taken before the control lane so
  // an input received after this viewport can never reach the keeper first.
  const ticket = acquireKeeperAdmission(this, channelId, "viewport_resize");
  const operation = enqueueTerminalControl(
    this,
    channelId,
    "viewport_claim",
    () => applyViewportNow(this, channelId, intent, ticket),
  ).finally(() => ticket.release());
  return reportViewportResultWithinBudget(operation);
}

export interface ResumeRedrawRecovery {
  nudge: ResumeRedrawResult;
  restore: ResumeRedrawResult;
}

async function settleResumeRedraw(
  mgr: SessionManager,
  channelId: number,
  ticket: KeeperAdmissionTicket,
  cols: number,
  rows: number,
): Promise<ResumeRedrawResult> {
  try {
    await ticket.granted;
    return await applyResumeRedrawNow(mgr, channelId, ticket, { cols, rows });
  } catch (error) {
    return {
      status: "ambiguous",
      resizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0,
      reason: error instanceof Error ? error.message : String(error),
    };
  } finally {
    ticket.release();
  }
}

/** Redraw an evicted resume in two control transactions. The nudge reserves its
 * write before entering the control lane, then releases admission at the write
 * boundary like every viewport resize. Only after its ACK/rebuild settles do we
 * reserve and queue the restore, so PTY input is never parked behind an unused
 * second ticket. A viewport received during the nudge runs before the restore;
 * its newer resize is already the authoritative restoration and must not be
 * overwritten with the stale pre-resume geometry. */
export function redrawEvictedResume(
  mgr: SessionManager,
  channelId: number,
  cols: number,
  rows: number,
): Promise<ResumeRedrawRecovery> {
  const viewportEpoch = mgr.viewportIntentEpoch.get(channelId) ?? 0;
  const nudgeTicket = acquireKeeperAdmission(mgr, channelId, "resume_resize");
  const nudgeRows = rows > 1 ? rows - 1 : rows + 1;
  const nudgeOperation = enqueueTerminalControl(
    mgr,
    channelId,
    "resume_redraw",
    () => settleResumeRedraw(mgr, channelId, nudgeTicket, cols, nudgeRows),
  ).catch((error) => {
    nudgeTicket.release();
    throw error;
  });
  return nudgeOperation.then(async (nudge) => {
    const restoreTicket = acquireKeeperAdmission(mgr, channelId, "resume_resize");
    const restoreOperation = enqueueTerminalControl(
      mgr,
      channelId,
      "resume_redraw",
      async () => {
        if (!mgr.sessions.has(channelId)) {
          restoreTicket.release();
          return {
            status: "rejected",
            resizeSeq: mgr.channelResizeSeq.get(channelId) ?? 0,
            reason: "session is not live",
          } satisfies ResumeRedrawResult;
        }
        const currentSeq = mgr.channelResizeSeq.get(channelId) ?? 0;
        // Any accepted viewport intent that ran between the nudge and this
        // queued restore owns the handoff, including background and withdrawal
        // states with no dimensions and no resize sequence.
        const viewportChanged =
          (mgr.viewportIntentEpoch.get(channelId) ?? 0) !== viewportEpoch;
        if (currentSeq !== nudge.resizeSeq || viewportChanged) {
          restoreTicket.release();
          return { status: "committed", resizeSeq: currentSeq } satisfies ResumeRedrawResult;
        }
        return settleResumeRedraw(mgr, channelId, restoreTicket, cols, rows);
      },
    ).catch((error) => {
      restoreTicket.release();
      throw error;
    });
    const restore = await restoreOperation;
    return { nudge, restore };
  });
}

/** Deferred withdrawal, freshness reaping, and SCD recompute. Same control lane
 *  and same capture as a typed claim, so a reap and a claim can never build two
 *  cores for one channel. Fire-and-forget: no caller is waiting on a result. */
export function reconcileTerminalViewport(this: SessionManager, channelId: number): void {
  const ticket: KeeperAdmissionTicket = acquireKeeperAdmission(this, channelId, "viewport_resize");
  void enqueueTerminalControl(
    this,
    channelId,
    "viewport_reconcile",
    () => reconcileViewportNow(this, channelId, ticket),
  ).catch(() => undefined).finally(() => ticket.release());
}
