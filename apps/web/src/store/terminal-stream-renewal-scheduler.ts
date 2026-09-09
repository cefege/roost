// Terminal view renewal has one document-owned deadline queue.
// Views arm their exact active intent after a successful publication.
// The queue gates visibility and never creates one interval per terminal pane.
// terminal-stream-view.ts installs the command callback and owns view semantics.

import { TERMINAL_VIEW_HEARTBEAT_MS } from "@roost/shared/viewport";
import { isPageVisible } from "../lib/pageVisible.ts";
import type { TerminalViewRecord } from "./terminal-stream-types.ts";

type TerminalViewRenewalHandler = (view: TerminalViewRecord) => void;

const scheduledViews = new Set<TerminalViewRecord>();
let renewalHandler: TerminalViewRenewalHandler | null = null;
let renewalTimer: Timer | null = null;
let schedulerEpoch = 0;
let nextDeadlineMs: number | null = null;
let dispatchingDueViews = false;
let renewalBatchDepth = 0;
let schedulePending = false;

export function registerTerminalViewRenewalHandler(
  handler: TerminalViewRenewalHandler,
): void {
  renewalHandler = handler;
  scheduleTerminalViewRenewals();
}
export function beginTerminalViewRenewalBatch(): void {
  renewalBatchDepth++;
}

export function endTerminalViewRenewalBatch(): void {
  renewalBatchDepth--;
  if (renewalBatchDepth !== 0 || !schedulePending) return;
  schedulePending = false;
  scheduleTerminalViewRenewals();
}


export function armTerminalViewRenewal(view: TerminalViewRecord): void {
  if (!view.desired?.active || view.disposed) {
    cancelTerminalViewRenewal(view);
    return;
  }
  view.renewalDueAtMs = performance.now() + TERMINAL_VIEW_HEARTBEAT_MS;
  scheduledViews.add(view);
  scheduleTerminalViewRenewals();
}

export function cancelTerminalViewRenewal(view: TerminalViewRecord): void {
  view.renewalDueAtMs = null;
  scheduledViews.delete(view);
  scheduleTerminalViewRenewals();
}

export function cancelTerminalViewRenewals(
  views: Iterable<TerminalViewRecord>,
): void {
  for (const view of views) {
    view.renewalDueAtMs = null;
    scheduledViews.delete(view);
  }
  scheduleTerminalViewRenewals();
}

/** Retire every outstanding callback before a Sync-generation replay. */
export function invalidateTerminalViewRenewals(): void {
  schedulerEpoch++;
  clearTimeout(renewalTimer ?? undefined);
  renewalTimer = null;
  nextDeadlineMs = null;
  for (const view of scheduledViews) view.renewalDueAtMs = null;
  scheduledViews.clear();
  schedulePending = false;
}

export function scheduleTerminalViewRenewals(): void {
  if (dispatchingDueViews || renewalBatchDepth !== 0) {
    schedulePending = true;
    return;
  }
  schedulePending = false;
  schedulerEpoch++;
  clearTimeout(renewalTimer ?? undefined);
  renewalTimer = null;
  nextDeadlineMs = null;
  if (!renewalHandler || !isPageVisible()) return;

  let earliestDueAtMs: number | null = null;
  for (const view of scheduledViews) {
    if (!eligibleForRenewal(view)) {
      view.renewalDueAtMs = null;
      scheduledViews.delete(view);
      continue;
    }
    const dueAtMs = view.renewalDueAtMs;
    if (dueAtMs === null) {
      scheduledViews.delete(view);
      continue;
    }
    if (earliestDueAtMs === null || dueAtMs < earliestDueAtMs) {
      earliestDueAtMs = dueAtMs;
    }
  }
  if (earliestDueAtMs === null) return;

  const epoch = schedulerEpoch;
  nextDeadlineMs = earliestDueAtMs;
  renewalTimer = setTimeout(() => {
    if (epoch !== schedulerEpoch) return;
    renewalTimer = null;
    nextDeadlineMs = null;
    dispatchingDueViews = true;
    try {
      renewDueTerminalViews(epoch);
    } finally {
      dispatchingDueViews = false;
    }
    if (epoch === schedulerEpoch) scheduleTerminalViewRenewals();
  }, Math.max(0, earliestDueAtMs - performance.now()));
}

export function _terminalViewRenewalSchedulerSnapshotForTest(): {
  armed: boolean;
  scheduledViewCount: number;
  nextDeadlineMs: number | null;
} {
  return {
    armed: renewalTimer !== null,
    scheduledViewCount: scheduledViews.size,
    nextDeadlineMs,
  };
}

function renewDueTerminalViews(epoch: number): void {
  const handler = renewalHandler;
  if (!handler || !isPageVisible()) return;
  const nowMs = performance.now();
  for (const view of scheduledViews) {
    if (epoch !== schedulerEpoch) return;
    if (!eligibleForRenewal(view)) {
      view.renewalDueAtMs = null;
      scheduledViews.delete(view);
      continue;
    }
    if ((view.renewalDueAtMs ?? Number.POSITIVE_INFINITY) > nowMs) continue;
    // Reserve the next slot before the command path runs so a failed write
    // cannot re-enter the same due view in this scheduler turn.
    view.renewalDueAtMs = nowMs + TERMINAL_VIEW_HEARTBEAT_MS;
    handler(view);
  }
}

function eligibleForRenewal(view: TerminalViewRecord): boolean {
  return !view.disposed && view.desired?.active === true && isPageVisible();
}
