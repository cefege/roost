// Generation-aware terminal input transport. View membership and screen
// continuity live in store/terminal-stream.ts; this module owns only atomic
// input admission/result correlation.

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
  _resetSmokeOutboundForTest,
} from "./sync-outbound-smoke.ts";

export { setSmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";
export type { SmokeTerminalInputObserver } from "./sync-outbound-smoke.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const MAX_PENDING_INPUTS_PER_SESSION = 200;
const MAX_PENDING_INPUT_BYTES_PER_SESSION = 256 * 1024;
const INPUT_RESULT_TIMEOUT_MS = 10_000;

type TerminalState = SyncV2TerminalState;
type ResultControl = SyncV2Control;
type OutboundCommand = Parameters<typeof sendSyncV2Command>[0];

export type InputOutcome =
  | { status: "accepted"; inputSeq: bigint; writtenBytes: number }
  | { status: "rejected"; inputSeq: bigint; writtenBytes: 0; reason: string }
  | { status: "ambiguous"; inputSeq: bigint; writtenBytes: number; reason: string };

export type InputAdmission =
  | { accepted: false; reason: string }
  | { accepted: true; inputSeq: bigint; result: Promise<InputOutcome> };

interface PendingInput {
  sessionId: string;
  viewId: string | undefined;
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
let observedDomainGeneration: bigint | null = null;
let nextInputSeq = 0n;

function command(value: unknown): OutboundCommand {
  return value as OutboundCommand;
}

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

function trySendInput(
  pending: PendingInput,
  state = currentSyncV2TerminalState(),
): void {
  if (pending.started || !state?.ready) return;
  if (
    state.socketId !== pending.socketId
    || state.domainGeneration !== pending.domainGeneration
  ) {
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
      ...(pending.viewId === undefined ? {} : { viewId: pending.viewId }),
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
    view_id: pending.viewId,
  });
}

function findInput(sessionId: string, inputSeq: bigint): PendingInput | null {
  return inputLanes.get(sessionId)?.pending.find(
    (entry) => entry.inputSeq === inputSeq,
  ) ?? null;
}

function handleControl(control: ResultControl, state: TerminalState): void {
  if (
    control.case !== "inputAccepted"
    && control.case !== "inputRejected"
    && control.case !== "inputAmbiguous"
  ) return;
  const value = control.value;
  const pending = findInput(value.sessionId, value.inputSeq);
  if (
    !pending
    || !pending.started
    || pending.socketId !== state.socketId
    || pending.domainGeneration !== value.domainGeneration
    || pending.domainGeneration !== state.domainGeneration
  ) return;

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
    return;
  }
  if (control.case === "inputRejected") {
    finishInput(pending, {
      status: "rejected",
      inputSeq: pending.inputSeq,
      writtenBytes: 0,
      reason: control.value.reason,
    });
    return;
  }
  finishInput(pending, {
    status: "ambiguous",
    inputSeq: pending.inputSeq,
    writtenBytes: control.value.writtenBytes,
    reason: control.value.reason,
  });
}

export function handleGeneration(state: TerminalState | null): void {
  const changed = !state
    || state.socketId !== observedSocketId
    || state.domainGeneration !== observedDomainGeneration;
  if (changed) {
    const closingSocket = observedSocketId;
    const closingDomain = observedDomainGeneration;
    for (const lane of Array.from(inputLanes.values())) {
      for (const pending of [...lane.pending]) {
        if (
          closingSocket !== null
          && (pending.socketId !== closingSocket
            || pending.domainGeneration !== closingDomain)
        ) continue;
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
          reason: outcome.status === "ambiguous"
            ? "generation_ambiguous"
            : "generation_closed",
          cooldownKey: pending.sessionId,
        });
      }
    }
    observedSocketId = state?.socketId ?? null;
    observedDomainGeneration = state?.domainGeneration ?? null;
    nextInputSeq = 0n;
  }

  if (!state?.ready) return;
  for (const lane of inputLanes.values()) {
    for (const pending of [...lane.pending]) trySendInput(pending, state);
  }
}

queueMicrotask(() => {
  registerSyncV2ControlHandler(handleControl);
  registerSyncV2GenerationHandler(handleGeneration);
});

/** Admit one complete PTY input batch. `viewId` is attribution only; callers
 * without a mounted browser view intentionally omit it. */
export function sendTerminalInput(
  sessionId: string,
  bytes: Uint8Array,
  viewId?: string,
): InputAdmission {
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
  if (
    lane.pending.length >= MAX_PENDING_INPUTS_PER_SESSION
    || lane.bytes + bytes.byteLength > MAX_PENDING_INPUT_BYTES_PER_SESSION
  ) {
    if (lane.pending.length === 0) inputLanes.delete(sessionId);
    return { accepted: false, reason: "terminal input queue is full" };
  }
  if (
    observedSocketId !== state.socketId
    || observedDomainGeneration !== state.domainGeneration
  ) handleGeneration(state);
  const inputSeq = ++nextInputSeq;
  const owned = bytes.slice();
  const { promise, resolve } = Promise.withResolvers<InputOutcome>();
  const pending: PendingInput = {
    sessionId,
    viewId,
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
    // Smoke instrumentation must never perturb delivery.
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

export function pruneTerminalInput(sessionId: string): void {
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
}

export function _resetTerminalOutboundForTest(): void {
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
  observedDomainGeneration = null;
  nextInputSeq = 0n;
  _resetSmokeOutboundForTest();
}
