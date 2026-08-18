// Terminal outbound lane: the PTY input queue, the control/generation fan-out
// that drives both outbound lanes, and the diagnostic snapshot.
//
// The viewport half lives in the sync-outbound-viewport*.ts siblings and is
// re-exported below, so callers keep one import site (the same shape store/sync.ts
// uses for its own leaf modules). handleGeneration stays HERE because it
// reconciles both lanes: an input batch and a viewport heartbeat can each observe
// a newer Sync generation before the store's notification arrives, and both must
// see the same reconciliation.

import { diag, signal } from "@roost/shared/diag";
import { getSessionTraceId } from "../lib/diag.ts";
import {
  currentSyncV2TerminalState,
  registerSyncV2ControlHandler,
  registerSyncV2GenerationHandler,
  sendSyncV2Command,
  type SyncV2Control,
  type SyncV2TerminalState,
} from "../store/sync.ts";
import {
  currentSmokeTerminalInputObserver,
  forgetSmokeViewportSession,
  _resetSmokeOutboundForTest,
} from "./sync-outbound-smoke.ts";
import {
  command,
  handleViewportControl,
  reconcileViewportGeneration,
} from "./sync-outbound-viewport-dispatch.ts";
import {
  persistViewportIntents,
  pruneViewportSession,
  viewportClaimSnapshot,
  _resetViewportOutboundForTest,
} from "./sync-outbound-viewport-registry.ts";
import type { TerminalViewportClaimSnapshot } from "./sync-outbound-viewport-types.ts";

// Public viewport surface, fronted here so the pane, the projector and the smoke
// harness keep importing terminal outbound state from one module.
export { acquireTerminalViewportOwner } from "./sync-outbound-viewport.ts";
export { seedTerminalViewportIntent } from "./sync-outbound-viewport-registry.ts";
export {
  noteTerminalProducerGeneration,
  rejectNextViewportClaim,
} from "./sync-outbound-viewport-dispatch.ts";
export { rejectedViewportClaimCount, setSmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";
export type { SmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";
export type {
  TerminalViewportClaim,
  TerminalViewportFullFrame,
  TerminalViewportOwner,
  TerminalViewportOwnerStatus,
  TerminalViewportStatusListener,
  ViewportAdmission,
  ViewportOutcome,
} from "./sync-outbound-viewport-types.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const MAX_PENDING_INPUTS_PER_SESSION = 200;
const MAX_PENDING_INPUT_BYTES_PER_SESSION = 256 * 1024;
const INPUT_RESULT_TIMEOUT_MS = 10_000;

type TerminalState = SyncV2TerminalState;
type ResultControl = SyncV2Control;

export type InputOutcome =
  | { status: "accepted"; inputSeq: bigint; writtenBytes: number }
  | { status: "rejected"; inputSeq: bigint; writtenBytes: 0; reason: string }
  | { status: "ambiguous"; inputSeq: bigint; writtenBytes: number; reason: string };

export type InputAdmission =
  | { accepted: false; reason: string }
  | { accepted: true; inputSeq: bigint; result: Promise<InputOutcome> };

interface PendingInput {
  sessionId: string;
  inputSeq: bigint;
  bytes: Uint8Array;
  socketId: string;
  domainGeneration: bigint;
  started: boolean;
  resolve: (result: InputOutcome) => void;
  timer: ReturnType<typeof setTimeout> | null;
}

interface InputLane {
  pending: PendingInput[];
  bytes: number;
}

const inputLanes = new Map<string, InputLane>();
const lastInputSendTs = new Map<string, number>();
let observedSocketId: string | null = null;
let nextInputSeq = 0n;

function finishInput(pending: PendingInput, outcome: InputOutcome): void {
  const lane = inputLanes.get(pending.sessionId);
  if (!lane) return;
  const index = lane.pending.indexOf(pending);
  if (index < 0) return;
  lane.pending.splice(index, 1);
  lane.bytes -= pending.bytes.byteLength;
  clearTimeout(pending.timer ?? undefined);
  if (lane.pending.length === 0) inputLanes.delete(pending.sessionId);
  pending.resolve(outcome);
}

function trySendInput(pending: PendingInput, state = currentSyncV2TerminalState()): void {
  if (pending.started || !state?.ready) return;
  if (state.socketId !== pending.socketId || state.domainGeneration !== pending.domainGeneration) {
    finishInput(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: "Sync generation closed before input was sent",
    });
    return;
  }
  const sent = sendSyncV2Command(command({
    case: "input",
    value: {
      sessionId: pending.sessionId,
      inputSeq: pending.inputSeq,
      data: pending.bytes,
      domainGeneration: pending.domainGeneration,
    },
  }));
  if (!sent) {
    finishInput(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: "Sync input command was not admitted",
    });
    return;
  }
  pending.started = true;
  pending.timer = setTimeout(() => {
    finishInput(pending, {
      status: "ambiguous",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: "input result timed out; the batch will not be retried",
    });
  }, INPUT_RESULT_TIMEOUT_MS);
  diag("bytes.up_send", {
    sid: pending.sessionId,
    session_trace_id: getSessionTraceId(pending.sessionId),
    dir: "up",
    len: pending.bytes.byteLength,
    input_seq: pending.inputSeq,
  });
}

function findInput(sessionId: string, inputSeq: bigint): PendingInput | null {
  return inputLanes.get(sessionId)?.pending.find((entry) => entry.inputSeq === inputSeq) ?? null;
}

function handleInputControl(control: ResultControl, state: TerminalState): boolean {
  if (control.case !== "inputAccepted"
    && control.case !== "inputRejected"
    && control.case !== "inputAmbiguous") return false;
  const value = control.value;
  const pending = findInput(value.sessionId, value.inputSeq);
  if (!pending || !pending.started
    || pending.socketId !== state.socketId
    || pending.domainGeneration !== value.domainGeneration
    || pending.domainGeneration !== state.domainGeneration) return true;
  if (control.case === "inputAccepted") {
    const accepted = control.value;
    if (accepted.writtenBytes === pending.bytes.byteLength) {
      finishInput(pending, {
        status: "accepted",
        inputSeq: pending.inputSeq,
        writtenBytes: accepted.writtenBytes,
      });
    } else {
      finishInput(pending, {
        status: "ambiguous",
        inputSeq: pending.inputSeq,
        writtenBytes: accepted.writtenBytes,
        reason: "coordinator accepted an incomplete input batch",
      });
    }
  } else if (control.case === "inputRejected") {
    const rejected = control.value;
    finishInput(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: rejected.reason,
    });
  } else {
    const ambiguous = control.value;
    finishInput(pending, {
      status: "ambiguous",
      inputSeq: pending.inputSeq,
      writtenBytes: ambiguous.writtenBytes,
      reason: ambiguous.reason,
    });
  }
  return true;
}

function handleControl(control: ResultControl, state: TerminalState): void {
  if (handleInputControl(control, state)) return;
  handleViewportControl(control, state);
}

/** Reconcile BOTH outbound lanes against the live Sync generation. Exported for
 * the viewport owner heartbeat, which can observe a newer generation before the
 * store notification reaches this module. */
export function handleGeneration(state: TerminalState | null): void {
  if (!state || state.socketId !== observedSocketId) {
    const closingSocket = observedSocketId;
    for (const lane of Array.from(inputLanes.values())) {
      for (const pending of [...lane.pending]) {
        if (closingSocket && pending.socketId !== closingSocket) continue;
        const outcome: InputOutcome = pending.started
          ? {
              status: "ambiguous",
              inputSeq: pending.inputSeq,
              writtenBytes: 0,
              reason: "Sync closed after input was sent; the batch will not be retried",
            }
          : {
              status: "rejected",
              inputSeq: pending.inputSeq,
              writtenBytes: 0,
              reason: "Sync closed before input was sent",
            };
        finishInput(pending, outcome);
        signal("input.drop_burst", {
          sid: pending.sessionId,
          reason: outcome.status === "ambiguous" ? "generation_ambiguous" : "generation_closed",
          cooldownKey: pending.sessionId,
        });
      }
    }
    observedSocketId = state?.socketId ?? null;
    nextInputSeq = 0n;
  }

  reconcileViewportGeneration(state);

  if (!state?.ready) return;
  for (const lane of inputLanes.values()) {
    for (const pending of [...lane.pending]) trySendInput(pending, state);
  }
}

queueMicrotask(() => {
  registerSyncV2ControlHandler(handleControl);
  registerSyncV2GenerationHandler(handleGeneration);
});

export function sendTerminalInput(sessionId: string, bytes: Uint8Array): InputAdmission {
  const state = currentSyncV2TerminalState();
  if (!state) return { accepted: false, reason: "terminal Sync is not connected" };
  if (bytes.byteLength > MAX_INPUT_BYTES) {
    return { accepted: false, reason: "input exceeds 64 KiB" };
  }
  let lane = inputLanes.get(sessionId);
  if (!lane) {
    lane = { pending: [], bytes: 0 };
    inputLanes.set(sessionId, lane);
  }
  if (lane.pending.length >= MAX_PENDING_INPUTS_PER_SESSION
    || lane.bytes + bytes.byteLength > MAX_PENDING_INPUT_BYTES_PER_SESSION) {
    if (lane.pending.length === 0) inputLanes.delete(sessionId);
    return { accepted: false, reason: "terminal input queue is full" };
  }
  if (observedSocketId !== state.socketId) handleGeneration(state);
  const inputSeq = ++nextInputSeq;
  const owned = bytes.slice();
  const { promise, resolve } = Promise.withResolvers<InputOutcome>();
  const pending: PendingInput = {
    sessionId,
    inputSeq,
    bytes: owned,
    socketId: state.socketId,
    domainGeneration: state.domainGeneration,
    started: false,
    resolve,
    timer: null,
  };
  lane.pending.push(pending);
  lane.bytes += owned.byteLength;
  lastInputSendTs.set(sessionId, performance.now());
  try {
    currentSmokeTerminalInputObserver()?.(sessionId, owned.slice());
  } catch {
    // Smoke instrumentation must never perturb terminal input delivery.
  }
  trySendInput(pending, state);
  return { accepted: true, inputSeq, result: promise };
}

export function consumeLastInputSendTs(sessionId: string): number | undefined {
  const value = lastInputSendTs.get(sessionId);
  if (value !== undefined) lastInputSendTs.delete(sessionId);
  return value;
}

export function inputMapSizes(): number {
  return lastInputSendTs.size + inputLanes.size;
}

export interface TerminalOutboundSnapshot {
  claim: TerminalViewportClaimSnapshot;
  sync: {
    socket_generation: number | null;
    socket_id: string | null;
    process_epoch: string | null;
    domain_generation: string | null;
    ready: boolean;
  };
}

/** Bounded on-demand view of the current desired/confirmed viewport ownership
 * and terminal Sync identity. Stale-generation confirmation is never reported
 * as current. */
export function terminalOutboundSnapshot(sessionId: string): TerminalOutboundSnapshot {
  const sync = currentSyncV2TerminalState();
  return {
    claim: viewportClaimSnapshot(sessionId, sync),
    sync: {
      socket_generation: sync?.socketGeneration ?? null,
      socket_id: sync?.socketId ?? null,
      process_epoch: sync?.processEpoch ?? null,
      domain_generation: sync?.domainGeneration.toString() ?? null,
      ready: sync?.ready ?? false,
    },
  };
}

export function pruneTerminalOutbound(sessionId: string): void {
  pruneViewportSession(sessionId);
  forgetSmokeViewportSession(sessionId);
  const lane = inputLanes.get(sessionId);
  if (lane) {
    for (const pending of [...lane.pending]) {
      finishInput(pending, {
        status: pending.started ? "ambiguous" : "rejected",
        inputSeq: pending.inputSeq,
        writtenBytes: 0,
        reason: "session closed",
      });
    }
  }
  lastInputSendTs.delete(sessionId);
  persistViewportIntents();
}

/** Deterministic state reset for the focused outbound protocol tests. */
export function _resetTerminalOutboundForTest(): void {
  _resetViewportOutboundForTest();
  for (const lane of inputLanes.values()) {
    for (const pending of lane.pending) {
      clearTimeout(pending.timer ?? undefined);
      pending.resolve({
        status: pending.started ? "ambiguous" : "rejected",
        inputSeq: pending.inputSeq,
        writtenBytes: 0,
        reason: "test reset",
      });
    }
  }
  inputLanes.clear();
  lastInputSendTs.clear();
  observedSocketId = null;
  nextInputSeq = 0n;
  _resetSmokeOutboundForTest();
}
