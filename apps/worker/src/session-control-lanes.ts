// Two per-channel ordering lanes. They exist for different reasons and must not
// be collapsed into one:
//
//   terminalControlChains — MUTUAL EXCLUSION for whole terminal-control
//     transactions (typed viewport claim, deferred withdraw / freshness reap /
//     SCD recompute, keeper resize, core rebuild, kill). Two transactions can
//     never interleave mid-flight, so there is exactly one owner of the resize
//     sequence, the capture, and the core swap.
//
//   keeperAdmissionLane — RECEIVE-ORDER preservation for keeper WRITES (resize
//     request, status query, PTY input, query reply). A transaction holds it only
//     from before its first keeper operation until the write that can apply the
//     newest geometry has actually been written — never until the ACK. Input
//     therefore waits for the ordering boundary, not for a resize result, which
//     is the whole point: head-of-line blocking of input behind a pending
//     control is the bug.
//
// This module deliberately imports nothing but the manager type: both the
// transaction and the capture/rebuild owner queue work here.

import type { SessionManager } from "./session-manager.ts";
import { monoNowMs } from "./util/mono.ts";

/** Transaction kinds that take the control lane. */
export type TerminalControlKind =
  | "viewport_claim"
  | "viewport_reconcile"
  | "terminal_kill";

/** Write kinds that take the admission lane. */
export type KeeperAdmissionKind =
  | "viewport_resize"
  | "terminal_input"
  | "query_reply";

export interface TerminalControlLane {
  tail: Promise<void>;
  /** Controls queued behind the running one. */
  depth: number;
  running: TerminalControlKind | null;
  runningSinceMonoMs: number;
}

export interface KeeperAdmissionLane {
  tail: Promise<void>;
  depth: number;
  holder: KeeperAdmissionKind | null;
  heldSinceMonoMs: number;
}

/** A held admission slot. `release` is idempotent so a transaction can release
 *  at its ordering boundary and again in `finally` without double-advancing. */
export interface KeeperAdmissionTicket {
  granted: Promise<void>;
  kind: KeeperAdmissionKind;
  release(): void;
}

/** Serialize one whole terminal-control transaction for a channel. The returned
 *  promise carries the caller's own result; a thrown transaction never poisons
 *  the lane for the next one. */
export function enqueueTerminalControl<T>(
  mgr: SessionManager,
  channelId: number,
  kind: TerminalControlKind,
  run: () => Promise<T>,
): Promise<T> {
  const lane: TerminalControlLane = mgr.terminalControlChains.get(channelId)
    ?? { tail: Promise.resolve(), depth: 0, running: null, runningSinceMonoMs: 0 };
  lane.depth++;
  mgr.terminalControlChains.set(channelId, lane);
  const operation = lane.tail.then(() => {
    lane.depth--;
    lane.running = kind;
    lane.runningSinceMonoMs = monoNowMs();
    return run();
  });
  lane.tail = operation.then(
    () => { lane.running = null; },
    () => { lane.running = null; },
  );
  void lane.tail.then(() => {
    // Drop the record only when nothing is queued or running, so the diagnostic
    // snapshot reports "idle" instead of a retained never-cleared lane.
    if (mgr.terminalControlChains.get(channelId) === lane && lane.depth === 0 && lane.running === null) {
      mgr.terminalControlChains.delete(channelId);
    }
  });
  return operation;
}

/** Drain the control lane for a channel. Public because both the diagnostic
 *  paths and the browser-command readers must observe a settled core. */
export function terminalControlSettled(mgr: SessionManager, channelId: number): Promise<void> {
  const lane = mgr.terminalControlChains.get(channelId);
  return lane ? lane.tail : Promise.resolve();
}

/** Take the write-ordering lane. The caller MUST release it at its ordering
 *  boundary; holding it past that point reintroduces head-of-line blocking. */
export function acquireKeeperAdmission(
  mgr: SessionManager,
  channelId: number,
  kind: KeeperAdmissionKind,
): KeeperAdmissionTicket {
  const lane: KeeperAdmissionLane = mgr.keeperAdmissionLane.get(channelId)
    ?? { tail: Promise.resolve(), depth: 0, holder: null, heldSinceMonoMs: 0 };
  lane.depth++;
  mgr.keeperAdmissionLane.set(channelId, lane);
  const prior = lane.tail;
  const { promise: released, resolve: releaseLane } = Promise.withResolvers<void>();
  lane.tail = prior.then(() => released);
  const granted = prior.then(() => {
    lane.depth--;
    lane.holder = kind;
    lane.heldSinceMonoMs = monoNowMs();
  });
  let releasedOnce = false;
  return {
    granted,
    kind,
    release: () => {
      if (releasedOnce) return;
      releasedOnce = true;
      if (lane.holder === kind) lane.holder = null;
      releaseLane();
      void lane.tail.then(() => {
        if (mgr.keeperAdmissionLane.get(channelId) === lane
            && lane.depth === 0 && lane.holder === null) {
          mgr.keeperAdmissionLane.delete(channelId);
        }
      });
    },
  };
}

/** Ordered keeper write that owns the lane for exactly its own write. */
export async function withKeeperAdmission<T>(
  mgr: SessionManager,
  channelId: number,
  kind: KeeperAdmissionKind,
  run: () => T | Promise<T>,
): Promise<T> {
  const ticket = acquireKeeperAdmission(mgr, channelId, kind);
  await ticket.granted;
  try {
    return await run();
  } finally {
    ticket.release();
  }
}
