// Per-session terminal input lane bookkeeping, shared by both input transports:
// Sync (ws/sync-outbound.ts) and the local worker socket (ws/local-terminal.ts).
// This module owns the queue caps, the local correlation sequence, the result
// timeout and the admission/outcome vocabulary; each transport owns only its own
// fence comparison and wire send, so neither forks a second queue.

import { currentSmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";

export const MAX_INPUT_BYTES = 64 * 1024;
export const MAX_PENDING_INPUTS_PER_SESSION = 200;
export const MAX_PENDING_INPUT_BYTES_PER_SESSION = 256 * 1024;
export const INPUT_RESULT_TIMEOUT_MS = 10_000;

export type InputOutcome =
  | { status: "accepted"; inputSeq: bigint; writtenBytes: number }
  | { status: "rejected"; inputSeq: bigint; writtenBytes: 0; reason: string }
  | { status: "ambiguous"; inputSeq: bigint; writtenBytes: number; reason: string };

export type InputAdmission =
  | { accepted: false; reason: string }
  | { accepted: true; inputSeq: bigint; result: Promise<InputOutcome> };

/** One admitted batch. `fence` is the transport's own generation identity; the
 * lane never interprets it, it only carries it back to the transport. */
export interface PendingTerminalInput<Fence> {
  readonly sessionId: string;
  readonly viewId: string | undefined;
  readonly inputSeq: bigint;
  readonly bytes: Uint8Array;
  readonly fence: Fence;
  started: boolean;
  timer: ReturnType<typeof setTimeout> | null;
  resolve(outcome: InputOutcome): void;
}

export interface EnqueuedTerminalInput<Fence> {
  pending: PendingTerminalInput<Fence>;
  inputSeq: bigint;
  result: Promise<InputOutcome>;
}

interface TerminalInputLaneControl {
  prune(sessionId: string, reason: string): void;
  clear(reason: string): void;
  laneCount(): number;
}

export interface TerminalInputLanes<Fence> extends TerminalInputLaneControl {
  /** The refusal reason for a batch that cannot be queued, else null. Pure: a
   * refused batch leaves no lane behind. */
  refuse(sessionId: string, byteLength: number): string | null;
  enqueue(
    sessionId: string,
    bytes: Uint8Array,
    viewId: string | undefined,
    fence: Fence,
  ): EnqueuedTerminalInput<Fence>;
  find(sessionId: string, inputSeq: bigint): PendingTerminalInput<Fence> | null;
  /** Mark a batch as on the wire and arm its result deadline. */
  markStarted(pending: PendingTerminalInput<Fence>): void;
  finish(pending: PendingTerminalInput<Fence>, outcome: InputOutcome): void;
  pending(): PendingTerminalInput<Fence>[];
  resetSequence(): void;
}

interface InputLane<Fence> {
  pending: PendingTerminalInput<Fence>[];
  bytes: number;
}

const lastInputSendTs = new Map<string, number>();
const laneSets = new Set<TerminalInputLaneControl>();

export function createTerminalInputLanes<Fence>(): TerminalInputLanes<Fence> {
  const lanes = new Map<string, InputLane<Fence>>();
  let nextInputSeq = 0n;

  function finish(pending: PendingTerminalInput<Fence>, outcome: InputOutcome): void {
    const lane = lanes.get(pending.sessionId);
    if (!lane) return;
    const index = lane.pending.indexOf(pending);
    if (index < 0) return;
    lane.pending.splice(index, 1);
    lane.bytes -= pending.bytes.byteLength;
    clearTimeout(pending.timer ?? undefined);
    if (lane.pending.length === 0) lanes.delete(pending.sessionId);
    pending.resolve(outcome);
  }

  const owned: TerminalInputLanes<Fence> = {
    refuse(sessionId, byteLength): string | null {
      if (byteLength > MAX_INPUT_BYTES) return "input exceeds 64 KiB";
      const lane = lanes.get(sessionId);
      if (!lane) return null;
      if (
        lane.pending.length >= MAX_PENDING_INPUTS_PER_SESSION
        || lane.bytes + byteLength > MAX_PENDING_INPUT_BYTES_PER_SESSION
      ) return "terminal input queue is full";
      return null;
    },
    enqueue(sessionId, bytes, viewId, fence): EnqueuedTerminalInput<Fence> {
      let lane = lanes.get(sessionId);
      if (!lane) {
        lane = { pending: [], bytes: 0 };
        lanes.set(sessionId, lane);
      }
      const inputSeq = ++nextInputSeq;
      const ownedBytes = bytes.slice();
      const { promise, resolve } = Promise.withResolvers<InputOutcome>();
      const pending: PendingTerminalInput<Fence> = {
        sessionId,
        viewId,
        inputSeq,
        bytes: ownedBytes,
        fence,
        started: false,
        timer: null,
        resolve,
      };
      lane.pending.push(pending);
      lane.bytes += ownedBytes.byteLength;
      lastInputSendTs.set(sessionId, performance.now());
      try {
        currentSmokeTerminalInputObserver()?.(sessionId, ownedBytes.slice());
      } catch {
        // Smoke instrumentation must never perturb delivery.
      }
      return { pending, inputSeq, result: promise };
    },
    find(sessionId, inputSeq): PendingTerminalInput<Fence> | null {
      return lanes.get(sessionId)?.pending.find(
        (entry) => entry.inputSeq === inputSeq,
      ) ?? null;
    },
    markStarted(pending): void {
      pending.started = true;
      pending.timer = setTimeout(() => {
        finish(pending, {
          status: "ambiguous",
          inputSeq: pending.inputSeq,
          writtenBytes: 0,
          reason: "input result timed out; the batch will not be retried",
        });
      }, INPUT_RESULT_TIMEOUT_MS);
    },
    finish,
    pending(): PendingTerminalInput<Fence>[] {
      const all: PendingTerminalInput<Fence>[] = [];
      for (const lane of lanes.values()) all.push(...lane.pending);
      return all;
    },
    prune(sessionId, reason): void {
      const lane = lanes.get(sessionId);
      if (!lane) return;
      for (const pending of [...lane.pending]) {
        finish(pending, {
          status: pending.started ? "ambiguous" : "rejected",
          inputSeq: pending.inputSeq,
          writtenBytes: 0,
          reason,
        });
      }
    },
    clear(reason): void {
      for (const lane of lanes.values()) {
        for (const pending of lane.pending) {
          clearTimeout(pending.timer ?? undefined);
          pending.resolve({
            status: pending.started ? "ambiguous" : "rejected",
            inputSeq: pending.inputSeq,
            writtenBytes: 0,
            reason,
          });
        }
      }
      lanes.clear();
      nextInputSeq = 0n;
    },
    laneCount(): number {
      return lanes.size;
    },
    resetSequence(): void {
      nextInputSeq = 0n;
    },
  };
  laneSets.add(owned);
  return owned;
}

/** A closed session owns no input on any transport. */
export function pruneTerminalInput(sessionId: string): void {
  for (const set of laneSets) set.prune(sessionId, "session closed");
  lastInputSendTs.delete(sessionId);
}

/** Reject every queued batch on every transport. Queued bytes must never be
 * replayed onto a newly accepted socket. */
export function resetTerminalInputLanes(reason: string): void {
  for (const set of laneSets) set.clear(reason);
  lastInputSendTs.clear();
}

export function consumeLastInputSendTs(sessionId: string): number | undefined {
  const value = lastInputSendTs.get(sessionId);
  if (value !== undefined) lastInputSendTs.delete(sessionId);
  return value;
}

export function inputMapSizes(): number {
  let lanes = 0;
  for (const set of laneSets) lanes += set.laneCount();
  return lastInputSendTs.size + lanes;
}
