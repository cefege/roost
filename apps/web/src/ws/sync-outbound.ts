import { diag, signal } from "@roost/shared/diag";
import { VIEWER_CLAIM_TTL_MS } from "@roost/shared/viewport";
import { getSessionTraceId, markPhase } from "../lib/diag.ts";
import {
  currentSyncV2TerminalState,
  registerSyncV2ControlHandler,
  registerSyncV2GenerationHandler,
  sendSyncV2Command,
} from "../store/sync.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const MAX_PENDING_INPUTS_PER_SESSION = 200;
const MAX_PENDING_INPUT_BYTES_PER_SESSION = 256 * 1024;
const INPUT_RESULT_TIMEOUT_MS = 10_000;
const MAX_VIEWPORT_INTENTS = 256;
const STORAGE_KEY = "roost.sync-v2.viewport-intents";
const HEARTBEAT_CAUSE = 6;

type TerminalState = NonNullable<ReturnType<typeof currentSyncV2TerminalState>>;
type OutboundCommand = Parameters<typeof sendSyncV2Command>[0];
type ResultControl = Parameters<Parameters<typeof registerSyncV2ControlHandler>[0]>[0];

export type InputOutcome =
  | { status: "accepted"; inputSeq: bigint; writtenBytes: number }
  | { status: "rejected"; inputSeq: bigint; writtenBytes: 0; reason: string }
  | { status: "ambiguous"; inputSeq: bigint; writtenBytes: number; reason: string };

export type InputAdmission =
  | { accepted: false; reason: string }
  | { accepted: true; inputSeq: bigint; result: Promise<InputOutcome> };

export type SmokeTerminalInputObserver = (sessionId: string, bytes: Uint8Array) => void;

export type ViewportOutcome =
  | {
      status: "accepted";
      sequence: bigint;
      effectiveCols: number;
      effectiveRows: number;
      channelResizeSeq: bigint;
    }
  | { status: "rejected" | "superseded"; sequence: bigint; reason: string };

export interface ViewportAdmission {
  sequence: bigint;
  result: Promise<ViewportOutcome>;
}

interface ViewportWaiter {
  resolve: (result: ViewportOutcome) => void;
}

interface ViewportIntent {
  sessionId: string;
  sequence: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  updatedAt: number;
  attemptedSocketId: string | null;
  attemptedDomainGeneration: bigint | null;
  waiters: ViewportWaiter[];
  tombstoneTimer: ReturnType<typeof setTimeout> | null;
  /** One-shot optimistic-spawn claim. Only an equivalent INITIAL consumes it
   * without sending; any real size change advances the sequence. */
  preclaimed: boolean;
}

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

const viewportIntents = new Map<string, ViewportIntent>();
const viewportSequenceFloors = new Map<string, { sequence: bigint; updatedAt: number }>();
const inputLanes = new Map<string, InputLane>();
const lastInputSendTs = new Map<string, number>();
let observedSocketId: string | null = null;
let nextInputSeq = 0n;
let smokeTerminalInputObserver: SmokeTerminalInputObserver | null = null;

/** Install the input-admission observer used by the lazy smoke backdoor.
 * Registration is ignored outside a smoke-enabled document, and the observer
 * receives its own byte copy so instrumentation cannot mutate the live batch. */
export function setSmokeTerminalInputObserver(
  observer: SmokeTerminalInputObserver | null,
): void {
  try {
    if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  } catch {
    return;
  }
  smokeTerminalInputObserver = observer;
}

function safeSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

function persistViewportIntents(): void {
  const storage = safeSessionStorage();
  if (!storage) return;
  try {
    const records: Array<Record<string, unknown>> = Array.from(viewportIntents.values())
      .map((intent) => ({
        sessionId: intent.sessionId,
        sequence: intent.sequence.toString(),
        cols: intent.cols,
        rows: intent.rows,
        cause: intent.cause,
        heldCellSeq: intent.heldCellSeq.toString(),
        updatedAt: intent.updatedAt,
      }));
    for (const [sessionId, floor] of viewportSequenceFloors) {
      if (viewportIntents.has(sessionId)) continue;
      records.push({
        sessionId,
        sequence: floor.sequence.toString(),
        updatedAt: floor.updatedAt,
        watermarkOnly: true,
      });
    }
    records.sort((a, b) => Number(b.updatedAt) - Number(a.updatedAt));
    records.length = Math.min(records.length, MAX_VIEWPORT_INTENTS);
    storage.setItem(STORAGE_KEY, JSON.stringify(records));
  } catch {
    // Storage can be denied or quota-limited; in-memory ownership remains live.
  }
}

function armTombstoneExpiry(intent: ViewportIntent): void {
  if (intent.tombstoneTimer) clearTimeout(intent.tombstoneTimer);
  intent.tombstoneTimer = null;
  if (intent.cols > 0 && intent.rows > 0) return;
  const remaining = VIEWER_CLAIM_TTL_MS - (Date.now() - intent.updatedAt);
  if (remaining <= 0) {
    if (viewportIntents.get(intent.sessionId) === intent) {
      viewportIntents.delete(intent.sessionId);
      persistViewportIntents();
    }
    return;
  }
  intent.tombstoneTimer = setTimeout(() => {
    if (viewportIntents.get(intent.sessionId) !== intent) return;
    viewportIntents.delete(intent.sessionId);
    for (const waiter of intent.waiters) {
      waiter.resolve({ status: "rejected", sequence: intent.sequence, reason: "viewport tombstone expired" });
    }
    persistViewportIntents();
  }, remaining);
}

function restoreViewportIntents(): void {
  const storage = safeSessionStorage();
  if (!storage) return;
  try {
    const decoded = JSON.parse(storage.getItem(STORAGE_KEY) ?? "[]") as Array<Record<string, unknown>>;
    for (const value of decoded.slice(0, MAX_VIEWPORT_INTENTS)) {
      if (typeof value.sessionId !== "string"
        || typeof value.sequence !== "string"
        || typeof value.updatedAt !== "number") continue;
      const sequence = BigInt(value.sequence);
      if (sequence <= 0n) continue;
      viewportSequenceFloors.set(value.sessionId, { sequence, updatedAt: value.updatedAt });
      if (value.watermarkOnly === true) continue;
      if (typeof value.cols !== "number"
        || typeof value.rows !== "number"
        || typeof value.cause !== "number"
        || typeof value.heldCellSeq !== "string") continue;
      const heldCellSeq = BigInt(value.heldCellSeq);
      if (heldCellSeq < 0n) continue;
      const intent: ViewportIntent = {
        sessionId: value.sessionId,
        sequence,
        cols: value.cols,
        rows: value.rows,
        cause: value.cause,
        heldCellSeq,
        updatedAt: value.updatedAt,
        attemptedSocketId: null,
        attemptedDomainGeneration: null,
        waiters: [],
        tombstoneTimer: null,
        preclaimed: false,
      };
      if ((intent.cols <= 0 || intent.rows <= 0)
        && Date.now() - intent.updatedAt >= VIEWER_CLAIM_TTL_MS) continue;
      viewportIntents.set(intent.sessionId, intent);
      armTombstoneExpiry(intent);
    }
  } catch {
    storage.removeItem(STORAGE_KEY);
  }
}

function command(value: unknown): OutboundCommand {
  return value as OutboundCommand;
}

function trySendViewport(intent: ViewportIntent, state = currentSyncV2TerminalState()): void {
  if (!state?.ready) return;
  if (intent.attemptedSocketId === state.socketId
    && intent.attemptedDomainGeneration === state.domainGeneration) return;
  const sent = sendSyncV2Command(command({
    case: "viewport",
    value: {
      sessionId: intent.sessionId,
      cols: intent.cols,
      rows: intent.rows,
      clientSeq: intent.sequence,
      cause: intent.cause,
      heldCellSeq: intent.heldCellSeq,
      domainGeneration: state.domainGeneration,
    },
  }));
  if (!sent) return;
  intent.attemptedSocketId = state.socketId;
  intent.attemptedDomainGeneration = state.domainGeneration;
}

function finishInput(pending: PendingInput, outcome: InputOutcome): void {
  const lane = inputLanes.get(pending.sessionId);
  if (!lane) return;
  const index = lane.pending.indexOf(pending);
  if (index < 0) return;
  lane.pending.splice(index, 1);
  lane.bytes -= pending.bytes.byteLength;
  if (pending.timer) clearTimeout(pending.timer);
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

function handleViewportControl(control: ResultControl, state: TerminalState): boolean {
  if (control.case !== "viewportAccepted" && control.case !== "viewportRejected") return false;
  const value = control.value;
  const intent = viewportIntents.get(value.sessionId);
  if (!intent
    || intent.sequence !== value.clientSeq
    || intent.attemptedSocketId !== state.socketId
    || intent.attemptedDomainGeneration !== value.domainGeneration
    || state.domainGeneration !== value.domainGeneration) return true;
  const waiters = intent.waiters.splice(0);
  if (control.case === "viewportAccepted") {
    const accepted = control.value;
    markPhase("viewport_accept", {
      sessionId: intent.sessionId,
      generation: accepted.domainGeneration,
      sequence: accepted.clientSeq,
    });
    for (const waiter of waiters) {
      waiter.resolve({
        status: "accepted",
        sequence: intent.sequence,
        effectiveCols: accepted.effectiveCols,
        effectiveRows: accepted.effectiveRows,
        channelResizeSeq: accepted.channelResizeSeq,
      });
    }
    if (intent.cols <= 0 || intent.rows <= 0) {
      clearTimeout(intent.tombstoneTimer ?? undefined);
      viewportIntents.delete(intent.sessionId);
    }
    persistViewportIntents();
  } else {
    const rejected = control.value;
    for (const waiter of waiters) {
      waiter.resolve({ status: "rejected", sequence: intent.sequence, reason: rejected.reason });
    }
  }
  return true;
}

function handleControl(control: ResultControl, state: TerminalState): void {
  if (handleInputControl(control, state)) return;
  handleViewportControl(control, state);
}

function handleGeneration(state: TerminalState | null): void {
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
    for (const intent of viewportIntents.values()) {
      intent.attemptedSocketId = null;
      intent.attemptedDomainGeneration = null;
    }
  }
  if (!state?.ready) return;
  for (const intent of viewportIntents.values()) trySendViewport(intent, state);
  for (const lane of inputLanes.values()) {
    for (const pending of [...lane.pending]) trySendInput(pending, state);
  }
}

restoreViewportIntents();
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
    smokeTerminalInputObserver?.(sessionId, owned.slice());
  } catch {
    // Smoke instrumentation must never perturb terminal input delivery.
  }
  trySendInput(pending, state);
  return { accepted: true, inputSeq, result: promise };
}

export function sendTerminalViewportIntent(
  sessionId: string,
  value: { cols: number; rows: number; cause: number; heldCellSeq?: number | bigint },
): ViewportAdmission {
  const prior = viewportIntents.get(sessionId);
  if (prior?.preclaimed) {
    const equivalentInitial = value.cause === 1
      && prior.cols === value.cols
      && prior.rows === value.rows;
    prior.preclaimed = false;
    if (equivalentInitial) {
      const state = currentSyncV2TerminalState();
      markPhase("viewport_enqueue", {
        sessionId,
        generation: state?.domainGeneration ?? null,
        sequence: prior.sequence,
      });
      markPhase("viewport_accept", {
        sessionId,
        generation: state?.domainGeneration ?? null,
        sequence: prior.sequence,
      });
      return {
        sequence: prior.sequence,
        result: Promise.resolve({
          status: "accepted",
          sequence: prior.sequence,
          effectiveCols: prior.cols,
          effectiveRows: prior.rows,
          channelResizeSeq: 0n,
        }),
      };
    }
  }
  const reuseSequence = value.cause === HEARTBEAT_CAUSE && prior !== undefined;
  const floor = viewportSequenceFloors.get(sessionId)?.sequence ?? 0n;
  const sequence = reuseSequence ? prior.sequence : (prior?.sequence ?? floor) + 1n;
  if (prior && !reuseSequence) {
    if (prior.tombstoneTimer) clearTimeout(prior.tombstoneTimer);
    for (const waiter of prior.waiters) {
      waiter.resolve({ status: "superseded", sequence: prior.sequence, reason: "newer viewport intent" });
    }
  }
  const { promise, resolve } = Promise.withResolvers<ViewportOutcome>();
  const intent: ViewportIntent = reuseSequence ? prior : {
    sessionId,
    sequence,
    cols: value.cols,
    rows: value.rows,
    cause: value.cause,
    heldCellSeq: BigInt(value.heldCellSeq ?? 0),
    updatedAt: Date.now(),
    attemptedSocketId: null,
    attemptedDomainGeneration: null,
    waiters: [],
    tombstoneTimer: null,
    preclaimed: false,
  };
  intent.cols = value.cols;
  intent.rows = value.rows;
  intent.cause = value.cause;
  intent.heldCellSeq = BigInt(value.heldCellSeq ?? 0);
  intent.updatedAt = Date.now();
  if (reuseSequence) {
    // Same-sequence heartbeats are intentional liveness/held-cell repairs and
    // must traverse even when this generation already accepted the claim.
    intent.attemptedSocketId = null;
    intent.attemptedDomainGeneration = null;
  }
  intent.waiters.push({ resolve });
  intent.preclaimed = false;
  viewportSequenceFloors.set(sessionId, { sequence, updatedAt: intent.updatedAt });
  viewportIntents.set(sessionId, intent);
  while (viewportIntents.size > MAX_VIEWPORT_INTENTS) {
    const oldest = Array.from(viewportIntents.values()).sort((a, b) => a.updatedAt - b.updatedAt)[0];
    if (!oldest || oldest === intent) break;
    viewportIntents.delete(oldest.sessionId);
    if (oldest.tombstoneTimer) clearTimeout(oldest.tombstoneTimer);
    for (const waiter of oldest.waiters) {
      waiter.resolve({ status: "rejected", sequence: oldest.sequence, reason: "viewport registry is full" });
    }
  }
  armTombstoneExpiry(intent);
  persistViewportIntents();
  const state = currentSyncV2TerminalState();
  markPhase("viewport_enqueue", {
    sessionId,
    generation: state?.domainGeneration ?? null,
    sequence,
  });
  trySendViewport(intent, state);
  return { sequence, result: promise };
}

/** Seed a viewport already committed by optimistic spawn. It becomes the
 * canonical replay intent without issuing a redundant INITIAL request. */
export function seedTerminalViewportIntent(
  sessionId: string,
  sequence: bigint,
  cols: number,
  rows: number,
  cause: number,
): void {
  const prior = viewportIntents.get(sessionId);
  if (prior && prior.sequence > sequence) return;
  if (prior?.tombstoneTimer) clearTimeout(prior.tombstoneTimer);
  const state = currentSyncV2TerminalState();
  viewportIntents.set(sessionId, {
    sessionId,
    sequence,
    cols,
    rows,
    cause,
    heldCellSeq: 0n,
    updatedAt: Date.now(),
    attemptedSocketId: state?.socketId ?? null,
    attemptedDomainGeneration: state?.domainGeneration ?? null,
    waiters: [],
    tombstoneTimer: null,
    preclaimed: true,
  });
  viewportSequenceFloors.set(sessionId, { sequence, updatedAt: Date.now() });
  persistViewportIntents();
}

export function consumeLastInputSendTs(sessionId: string): number | undefined {
  const value = lastInputSendTs.get(sessionId);
  if (value !== undefined) lastInputSendTs.delete(sessionId);
  return value;
}

export function inputMapSizes(): number {
  return lastInputSendTs.size + inputLanes.size;
}

export function pruneTerminalOutbound(sessionId: string): void {
  const intent = viewportIntents.get(sessionId);
  if (intent?.tombstoneTimer) clearTimeout(intent.tombstoneTimer);
  if (intent) {
    for (const waiter of intent.waiters) {
      waiter.resolve({ status: "rejected", sequence: intent.sequence, reason: "session closed" });
    }
  }
  viewportIntents.delete(sessionId);
  viewportSequenceFloors.delete(sessionId);
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
  for (const intent of viewportIntents.values()) {
    if (intent.tombstoneTimer) clearTimeout(intent.tombstoneTimer);
    for (const waiter of intent.waiters) {
      waiter.resolve({ status: "rejected", sequence: intent.sequence, reason: "test reset" });
    }
  }
  for (const lane of inputLanes.values()) {
    for (const pending of lane.pending) {
      if (pending.timer) clearTimeout(pending.timer);
      pending.resolve({
        status: pending.started ? "ambiguous" : "rejected",
        inputSeq: pending.inputSeq,
        writtenBytes: 0,
        reason: "test reset",
      });
    }
  }
  viewportIntents.clear();
  viewportSequenceFloors.clear();
  inputLanes.clear();
  lastInputSendTs.clear();
  observedSocketId = null;
  nextInputSeq = 0n;
  safeSessionStorage()?.removeItem(STORAGE_KEY);
}
