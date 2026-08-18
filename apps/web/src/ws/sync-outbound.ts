import { diag, signal } from "@roost/shared/diag";
import { getSessionTraceId, markPhase } from "../lib/diag.ts";
import {
  currentSyncV2TerminalState,
  registerSyncV2ControlHandler,
  registerSyncV2GenerationHandler,
  sendSyncV2Command,
  type SyncV2Control,
  type SyncV2TerminalState,
} from "../store/sync.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const MAX_PENDING_INPUTS_PER_SESSION = 200;
const MAX_PENDING_INPUT_BYTES_PER_SESSION = 256 * 1024;
const INPUT_RESULT_TIMEOUT_MS = 10_000;
const MAX_VIEWPORT_SESSIONS = 256;
const MAX_VIEWPORT_STATUS_LISTENERS = 32;
const STORAGE_KEY = "roost.sync-v2.viewport-intents";
const HEARTBEAT_CAUSE = 6;
const VIEWPORT_RESULT_TIMEOUT_MS = 10_000;
const VIEWPORT_REPAIR_TIMEOUT_MS = 3_000;
const VIEWPORT_RETRY_DELAYS_MS = [250, 500, 1_000, 2_000] as const;
const MAX_WIRE_DIMENSION = 0xffff_ffff;
const MAX_SAFE_CELL_SEQ = BigInt(Number.MAX_SAFE_INTEGER);

type TerminalState = SyncV2TerminalState;
type OutboundCommand = Parameters<typeof sendSyncV2Command>[0];
type ResultControl = SyncV2Control;

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
  /** Sequence assigned to the first attempt. Retries advance the wire sequence,
   * while the result reports the sequence that ultimately became ready. */
  sequence: bigint;
  result: Promise<ViewportOutcome>;
}

export interface TerminalViewportClaim {
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq?: number | bigint;
  /** Require a full frame newer than this attempt's dispatch receipt before
   * resolving the admission. Current-model claims normally leave this false. */
  repairRequired?: boolean;
}

export interface TerminalViewportFullFrame {
  seq: number;
  gridEpoch: string;
}

export type TerminalViewportOwnerStatus =
  | { status: "pending"; sequence: bigint; repairRequired: boolean }
  | { status: "retrying"; sequence: bigint; reason: string; retryInMs: number }
  | {
      status: "repairing";
      sequence: bigint;
      effectiveCols: number;
      effectiveRows: number;
      channelResizeSeq: bigint;
    }
  | {
      status: "ready";
      sequence: bigint;
      effectiveCols: number;
      effectiveRows: number;
      channelResizeSeq: bigint;
    }
  | { status: "rejected" | "superseded"; sequence: bigint; reason: string };

export type TerminalViewportStatusListener = (status: TerminalViewportOwnerStatus) => void;

export interface TerminalViewportOwner {
  readonly token: bigint;
  claim(value: TerminalViewportClaim): ViewportAdmission;
  heartbeat(heldCellSeq: number | bigint): void;
  noteFullFrame(frame: TerminalViewportFullFrame): void;
  subscribeStatus(listener: TerminalViewportStatusListener): () => void;
  dispose(): void;
}

interface ViewportDesired {
  sequence: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  repairRequired: boolean;
  updatedAt: number;
  admission: ViewportAdmission;
  resolve: (result: ViewportOutcome) => void;
  settled: boolean;
  retryCount: number;
  needsSequenceAdvance: boolean;
}

interface ViewportAttempt {
  sequence: bigint;
  socketId: string;
  domainGeneration: bigint;
  processEpoch: string;
  fullFrameReceiptFloor: number;
  fullFrameReady: boolean;
  phase: "result" | "repair";
  deadlineAt: number;
  timer: ReturnType<typeof setTimeout> | null;
  accepted: {
    effectiveCols: number;
    effectiveRows: number;
    channelResizeSeq: bigint;
  } | null;
}

interface ViewportPreclaim {
  sequence: bigint;
  cols: number;
  rows: number;
  cause: number;
  updatedAt: number;
}

interface ViewportSession {
  sessionId: string;
  sequenceFloor: bigint;
  sequenceUpdatedAt: number;
  ownerToken: bigint | null;
  desired: ViewportDesired | null;
  attempt: ViewportAttempt | null;
  preclaim: ViewportPreclaim | null;
  confirmed: {
    sequence: bigint;
    socketId: string;
    domainGeneration: bigint;
    processEpoch: string;
    effectiveCols: number;
    effectiveRows: number;
  } | null;
  retryTimer: ReturnType<typeof setTimeout> | null;
  retryAt: number | null;
  retryReason: string | null;
  retrySocketId: string | null;
  retryDomainGeneration: bigint | null;
  processEpoch: string | null;
  fullFrameReceipt: number;
  fullFrameSeq: number;
  fullFrameGridEpoch: string | null;
  status: TerminalViewportOwnerStatus | null;
  listeners: Set<TerminalViewportStatusListener>;
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

interface SmokeViewportRejectArm {
  afterSequence: bigint;
  socketId: string | null;
  domainGeneration: bigint | null;
}


const viewportSessions = new Map<string, ViewportSession>();
const inputLanes = new Map<string, InputLane>();
const lastInputSendTs = new Map<string, number>();
let observedSocketId: string | null = null;
let nextInputSeq = 0n;
let nextViewportOwnerToken = 0n;
let smokeTerminalInputObserver: SmokeTerminalInputObserver | null = null;
const smokeRejectNextViewportClaims = new Map<string, SmokeViewportRejectArm>();
const smokeRejectedViewportClaimCounts = new Map<string, number>();

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

function smokeBackdoorEnabled(): boolean {
  try {
    return typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1";
  } catch {
    return false;
  }
}

/** Reject exactly one positive viewport attempt before it enters the wire.
 * Smoke-only: models a definite negative reclaim result while keeping the
 * desired owner claim live so the normal retry path must recover it. */
export function rejectNextViewportClaim(sessionId: string): void {
  if (!smokeBackdoorEnabled()) return;
  const sync = currentSyncV2TerminalState();
  const session = viewportSession(sessionId);
  smokeRejectNextViewportClaims.set(sessionId, {
    afterSequence: session.desired?.sequence ?? session.sequenceFloor,
    socketId: sync?.socketId ?? null,
    domainGeneration: sync?.domainGeneration ?? null,
  });
}

export function rejectedViewportClaimCount(sessionId: string): number {
  return smokeRejectedViewportClaimCounts.get(sessionId) ?? 0;
}

function consumeSmokeViewportRejection(
  session: ViewportSession,
  desired: ViewportDesired,
  sync: TerminalState,
): boolean {
  if (desired.cols <= 0 || desired.rows <= 0 || smokeRejectNextViewportClaims.size === 0) return false;
  const armed = smokeRejectNextViewportClaims.get(session.sessionId);
  if (!armed
    || desired.sequence <= armed.afterSequence
    || (armed.socketId !== null && armed.socketId !== sync.socketId)
    || (armed.domainGeneration !== null && armed.domainGeneration !== sync.domainGeneration)) return false;
  smokeRejectNextViewportClaims.delete(session.sessionId);
  smokeRejectedViewportClaimCounts.set(
    session.sessionId,
    (smokeRejectedViewportClaimCounts.get(session.sessionId) ?? 0) + 1,
  );
  return true;
}

function safeSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

function boundedViewportReason(reason: string): string {
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

function emitViewportStatus(session: ViewportSession, status: TerminalViewportOwnerStatus): void {
  session.status = status;
  for (const listener of session.listeners) {
    try {
      listener(status);
    } catch {
      // Ownership and retry progress cannot depend on a diagnostic observer.
    }
  }
}

function clearViewportAttempt(session: ViewportSession): void {
  clearTimeout(session.attempt?.timer ?? undefined);
  session.attempt = null;
}

function clearViewportRetry(session: ViewportSession): void {
  clearTimeout(session.retryTimer ?? undefined);
  session.retryTimer = null;
  session.retryAt = null;
  session.retryReason = null;
  session.retrySocketId = null;
  session.retryDomainGeneration = null;
}

function settleViewportDesired(session: ViewportSession, outcome: ViewportOutcome): void {
  const desired = session.desired;
  if (!desired || desired.settled) return;
  desired.settled = true;
  desired.resolve(outcome);
}

function supersedeViewportDesired(session: ViewportSession, reason: string): void {
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
  smokeRejectNextViewportClaims.delete(session.sessionId);
  smokeRejectedViewportClaimCounts.delete(session.sessionId);
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

function viewportSession(sessionId: string): ViewportSession {
  let session = viewportSessions.get(sessionId);
  if (session) return session;
  session = newViewportSession(sessionId);
  viewportSessions.set(sessionId, session);
  trimViewportSessions(sessionId);
  return session;
}

function persistViewportIntents(): void {
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

function updateViewportSequence(
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

function advanceViewportSequence(session: ViewportSession, desired: ViewportDesired): void {
  updateViewportSequence(session, desired, session.sequenceFloor + 1n);
}

function command(value: unknown): OutboundCommand {
  return value as OutboundCommand;
}

function sendViewportCommand(
  session: ViewportSession,
  desired: ViewportDesired,
  sync: TerminalState,
  cause = desired.cause,
): boolean {
  return sendSyncV2Command(command({
    case: "viewport",
    value: {
      sessionId: session.sessionId,
      cols: desired.cols,
      rows: desired.rows,
      clientSeq: desired.sequence,
      cause,
      heldCellSeq: desired.heldCellSeq,
      domainGeneration: sync.domainGeneration,
    },
  }));
}

function finishViewportReady(session: ViewportSession, desired: ViewportDesired, attempt: ViewportAttempt): void {
  if (session.desired !== desired || session.attempt !== attempt || !attempt.accepted) return;
  const accepted = attempt.accepted;
  clearViewportAttempt(session);
  desired.retryCount = 0;
  desired.needsSequenceAdvance = false;
  desired.repairRequired = false;
  const outcome: Extract<ViewportOutcome, { status: "accepted" }> = {
    status: "accepted",
    sequence: desired.sequence,
    effectiveCols: accepted.effectiveCols,
    effectiveRows: accepted.effectiveRows,
    channelResizeSeq: accepted.channelResizeSeq,
  };
  emitViewportStatus(session, {
    status: "ready",
    sequence: outcome.sequence,
    effectiveCols: outcome.effectiveCols,
    effectiveRows: outcome.effectiveRows,
    channelResizeSeq: outcome.channelResizeSeq,
  });
  settleViewportDesired(session, outcome);
}

function scheduleViewportRetry(session: ViewportSession, desired: ViewportDesired, reason: string): void {
  if (session.desired !== desired) return;
  clearViewportAttempt(session);
  clearViewportRetry(session);
  desired.needsSequenceAdvance = true;
  const retryInMs = VIEWPORT_RETRY_DELAYS_MS[
    Math.min(desired.retryCount, VIEWPORT_RETRY_DELAYS_MS.length - 1)
  ]!;
  desired.retryCount += 1;
  session.retryAt = Date.now() + retryInMs;
  session.retryReason = boundedViewportReason(reason);
  const failedSync = currentSyncV2TerminalState();
  session.retrySocketId = failedSync?.socketId ?? null;
  session.retryDomainGeneration = failedSync?.domainGeneration ?? null;
  emitViewportStatus(session, {
    status: "retrying",
    sequence: desired.sequence,
    reason: session.retryReason,
    retryInMs,
  });
  session.retryTimer = setTimeout(() => {
    if (viewportSessions.get(session.sessionId) !== session || session.desired !== desired) return;
    session.retryTimer = null;
    session.retryAt = null;
    session.retryReason = null;
    session.retrySocketId = null;
    session.retryDomainGeneration = null;
    const sync = currentSyncV2TerminalState();
    if (!sync?.ready) {
      emitViewportStatus(session, {
        status: "pending",
        sequence: desired.sequence,
        repairRequired: desired.repairRequired,
      });
      return;
    }
    dispatchViewportDesired(session, desired, sync);
  }, retryInMs);
}

function dispatchViewportDesired(session: ViewportSession, desired: ViewportDesired, sync: TerminalState): void {
  if (viewportSessions.get(session.sessionId) !== session
    || session.desired !== desired
    || session.attempt
    || session.retryTimer
    || !sync.ready) return;
  if (desired.needsSequenceAdvance) advanceViewportSequence(session, desired);
  markPhase("viewport_enqueue", {
    sessionId: session.sessionId,
    generation: sync.domainGeneration,
    sequence: desired.sequence,
  });
  if (consumeSmokeViewportRejection(session, desired, sync)) {
    scheduleViewportRetry(session, desired, "smoke-injected viewport reclaim rejection");
    return;
  }
  const attempt: ViewportAttempt = {
    sequence: desired.sequence,
    socketId: sync.socketId,
    domainGeneration: sync.domainGeneration,
    processEpoch: sync.processEpoch,
    fullFrameReceiptFloor: session.fullFrameReceipt,
    fullFrameReady: false,
    phase: "result",
    deadlineAt: Date.now() + VIEWPORT_RESULT_TIMEOUT_MS,
    timer: null,
    accepted: null,
  };
  if (!sendViewportCommand(session, desired, sync)) {
    scheduleViewportRetry(session, desired, "terminal Sync did not admit the viewport command");
    return;
  }
  session.attempt = attempt;
  emitViewportStatus(session, {
    status: "pending",
    sequence: desired.sequence,
    repairRequired: desired.repairRequired,
  });
  attempt.timer = setTimeout(() => {
    if (viewportSessions.get(session.sessionId) !== session
      || session.desired !== desired
      || session.attempt !== attempt) return;
    scheduleViewportRetry(session, desired, "viewport result deadline expired");
  }, VIEWPORT_RESULT_TIMEOUT_MS);
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
  if (control.case !== "viewportAccepted"
    && control.case !== "viewportRejected"
    && control.case !== "viewportAmbiguous") return false;
  const value = control.value;
  const session = viewportSessions.get(value.sessionId);
  const desired = session?.desired;
  const attempt = session?.attempt;
  const current = currentSyncV2TerminalState();
  if (!session
    || !desired
    || !attempt
    || attempt.sequence !== value.clientSeq
    || desired.sequence !== value.clientSeq
    || attempt.socketId !== state.socketId
    || attempt.processEpoch !== state.processEpoch
    || attempt.domainGeneration !== value.domainGeneration
    || state.domainGeneration !== value.domainGeneration
    || !current
    || current.socketId !== attempt.socketId
    || current.processEpoch !== attempt.processEpoch
    || current.domainGeneration !== attempt.domainGeneration) return true;

  if (control.case === "viewportAccepted") {
    const accepted = control.value;
    clearTimeout(attempt.timer ?? undefined);
    attempt.timer = null;
    attempt.accepted = {
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
      channelResizeSeq: accepted.channelResizeSeq,
    };
    session.confirmed = {
      sequence: accepted.clientSeq,
      socketId: state.socketId,
      domainGeneration: accepted.domainGeneration,
      processEpoch: state.processEpoch,
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
    };
    markPhase("viewport_accept", {
      sessionId: session.sessionId,
      generation: accepted.domainGeneration,
      sequence: accepted.clientSeq,
    });
    if (!desired.repairRequired || attempt.fullFrameReady) {
      finishViewportReady(session, desired, attempt);
      return true;
    }
    attempt.phase = "repair";
    attempt.deadlineAt = Date.now() + VIEWPORT_REPAIR_TIMEOUT_MS;
    emitViewportStatus(session, {
      status: "repairing",
      sequence: desired.sequence,
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
      channelResizeSeq: accepted.channelResizeSeq,
    });
    attempt.timer = setTimeout(() => {
      if (viewportSessions.get(session.sessionId) !== session
        || session.desired !== desired
        || session.attempt !== attempt) return;
      scheduleViewportRetry(session, desired, "accepted viewport did not produce a newer full frame");
    }, VIEWPORT_REPAIR_TIMEOUT_MS);
    return true;
  }

  const reason = control.case === "viewportRejected"
    ? `viewport rejected: ${control.value.reason}`
    : `viewport outcome ambiguous: ${control.value.reason}`;
  scheduleViewportRetry(session, desired, reason);
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
  }

  for (const session of viewportSessions.values()) {
    const desired = session.desired;
    const producerChanged = state?.ready === true
      && session.processEpoch !== null
      && session.processEpoch !== state.processEpoch;
    if (state?.ready) session.processEpoch = state.processEpoch;
    if (producerChanged && desired && desired.cols > 0 && desired.rows > 0) {
      desired.repairRequired = true;
      desired.needsSequenceAdvance = true;
    }

    const confirmedCurrent = state?.ready === true
      && session.confirmed?.socketId === state.socketId
      && session.confirmed.processEpoch === state.processEpoch
      && session.confirmed.domainGeneration === state.domainGeneration;
    const confirmationInvalid = session.confirmed !== null && !confirmedCurrent;
    if (confirmationInvalid) {
      session.confirmed = null;
      if (desired) desired.needsSequenceAdvance = true;
    }

    const invalidAttempt = session.attempt !== null
      && (!state?.ready
        || session.attempt.socketId !== state.socketId
        || session.attempt.processEpoch !== state.processEpoch
        || session.attempt.domainGeneration !== state.domainGeneration);
    if (invalidAttempt) {
      clearViewportAttempt(session);
      if (desired) {
        desired.needsSequenceAdvance = true;
        desired.retryCount = 0;
      }
    }

    const invalidRetry = session.retryTimer !== null
      && (!state?.ready
        || session.retrySocketId !== state.socketId
        || session.retryDomainGeneration !== state.domainGeneration);
    if (invalidRetry) {
      clearViewportRetry(session);
      if (desired) {
        desired.needsSequenceAdvance = true;
        desired.retryCount = 0;
      }
    }

    if (!desired) continue;
    if (!state?.ready) {
      if (confirmationInvalid || invalidAttempt || invalidRetry) {
        emitViewportStatus(session, {
          status: "pending",
          sequence: desired.sequence,
          repairRequired: desired.repairRequired,
        });
      }
      continue;
    }
    if (confirmedCurrent
      && session.confirmed?.sequence === desired.sequence
      && !session.attempt
      && !session.retryTimer
      && !producerChanged) continue;
    dispatchViewportDesired(session, desired, state);
  }

  if (!state?.ready) return;
  for (const lane of inputLanes.values()) {
    for (const pending of [...lane.pending]) trySendInput(pending, state);
  }
}

/** `respawned` and a worker boot/reconcile `snapshot` change the PRODUCING
 * worker generation for these sessions: the old core is gone, and the claims the
 * worker held for it went with it (`_dropChannelState` / worker restart). This
 * is the same repair edge `processEpoch` already contracts, so it runs through
 * the same path — a tab holding a current positive owner sends a NEWER claim
 * with `heldCellSeq = 0` and requires the new core's authoritative full frame,
 * while a tab with no positive owner does nothing. The terminal never remounts.
 *
 * Call it after the session projection lands, so a claim can only be dispatched
 * once the store agrees with the coordinator about the session's route. */
export function noteTerminalProducerGeneration(sessionIds: Iterable<string>): void {
  const state = currentSyncV2TerminalState();
  for (const sessionId of sessionIds) {
    const session = viewportSessions.get(sessionId);
    const desired = session?.desired;
    if (!session || !desired || desired.cols <= 0 || desired.rows <= 0) continue;
    // The new core holds nothing this tab ever applied, and any acceptance on
    // record belongs to the retired producer.
    desired.heldCellSeq = 0n;
    desired.repairRequired = true;
    desired.needsSequenceAdvance = true;
    desired.retryCount = 0;
    session.confirmed = null;
    if (!state?.ready) {
      emitViewportStatus(session, {
        status: "pending",
        sequence: desired.sequence,
        repairRequired: true,
      });
      continue;
    }
    // An attempt or a scheduled retry aimed at the retired producer can never
    // become this repair: supersede it with the newer claim on the live
    // generation instead of waiting for its deadline.
    clearViewportAttempt(session);
    clearViewportRetry(session);
    dispatchViewportDesired(session, desired, state);
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

function inactiveViewportAdmission(sessionId: string, token: bigint, reason: string): ViewportAdmission {
  const session = viewportSessions.get(sessionId);
  const sequence = session?.desired?.sequence ?? session?.sequenceFloor ?? 0n;
  return {
    sequence,
    result: Promise.resolve({
      status: session?.ownerToken === token ? "rejected" : "superseded",
      sequence,
      reason,
    }),
  };
}


function claimTerminalViewport(
  sessionId: string,
  token: bigint,
  value: TerminalViewportClaim,
): ViewportAdmission {
  const session = viewportSessions.get(sessionId);
  if (!session || session.ownerToken !== token) {
    return inactiveViewportAdmission(sessionId, token, "viewport owner was superseded");
  }

  let heldCellSeq: bigint;
  try {
    heldCellSeq = BigInt(value.heldCellSeq ?? 0);
  } catch {
    return inactiveViewportAdmission(sessionId, token, "held cell sequence is invalid");
  }
  if (!Number.isSafeInteger(value.cols)
    || !Number.isSafeInteger(value.rows)
    || !Number.isSafeInteger(value.cause)
    || value.cols < 0
    || value.cols > MAX_WIRE_DIMENSION
    || value.rows > MAX_WIRE_DIMENSION
    || value.rows < 0
    || value.cause < 0
    || heldCellSeq < 0n
    || heldCellSeq > MAX_SAFE_CELL_SEQ) {
    return inactiveViewportAdmission(sessionId, token, "viewport claim is invalid");
  }
  const positive = value.cols > 0 && value.rows > 0;
  const repairRequired = positive
    && (value.repairRequired === true || heldCellSeq === 0n);
  const currentDesired = session.desired;
  if (currentDesired
    && currentDesired.cols === value.cols
    && currentDesired.rows === value.rows
    && currentDesired.cause === value.cause
    && currentDesired.heldCellSeq === heldCellSeq
    && currentDesired.repairRequired === repairRequired) {
    return currentDesired.admission;
  }

  const preclaim = session.preclaim;
  session.preclaim = null;
  if (preclaim
    && value.cause === 1
    && preclaim.cols === value.cols
    && preclaim.rows === value.rows) {
    if (currentDesired) supersedeViewportDesired(session, "optimistic viewport preclaim replaced the prior intent");
    const { promise, resolve } = Promise.withResolvers<ViewportOutcome>();
    const admission: ViewportAdmission = {
      sequence: preclaim.sequence,
      result: promise,
    };
    const desired: ViewportDesired = {
      sequence: preclaim.sequence,
      cols: preclaim.cols,
      rows: preclaim.rows,
      cause: value.cause,
      heldCellSeq,
      repairRequired,
      updatedAt: Date.now(),
      admission,
      resolve,
      settled: false,
      retryCount: 0,
      needsSequenceAdvance: false,
    };
    session.desired = desired;
    const sync = currentSyncV2TerminalState();
    if (!sync?.ready) {
      desired.needsSequenceAdvance = true;
      emitViewportStatus(session, {
        status: "pending",
        sequence: desired.sequence,
        repairRequired: desired.repairRequired,
      });
      return admission;
    }

    session.processEpoch = sync.processEpoch;
    session.confirmed = {
      sequence: preclaim.sequence,
      socketId: sync.socketId,
      domainGeneration: sync.domainGeneration,
      processEpoch: sync.processEpoch,
      effectiveCols: preclaim.cols,
      effectiveRows: preclaim.rows,
    };
    const accepted = {
      effectiveCols: preclaim.cols,
      effectiveRows: preclaim.rows,
      channelResizeSeq: 0n,
    };
    markPhase("viewport_enqueue", {
      sessionId,
      generation: sync.domainGeneration,
      sequence: preclaim.sequence,
    });
    markPhase("viewport_accept", {
      sessionId,
      generation: sync.domainGeneration,
      sequence: preclaim.sequence,
    });
    if (!repairRequired || session.fullFrameReceipt > 0) {
      desired.repairRequired = false;
      const outcome: Extract<ViewportOutcome, { status: "accepted" }> = {
        status: "accepted",
        sequence: preclaim.sequence,
        ...accepted,
      };
      emitViewportStatus(session, {
        status: "ready",
        sequence: outcome.sequence,
        effectiveCols: outcome.effectiveCols,
        effectiveRows: outcome.effectiveRows,
        channelResizeSeq: outcome.channelResizeSeq,
      });
      settleViewportDesired(session, outcome);
      return admission;
    }

    const attempt: ViewportAttempt = {
      sequence: preclaim.sequence,
      socketId: sync.socketId,
      domainGeneration: sync.domainGeneration,
      processEpoch: sync.processEpoch,
      fullFrameReceiptFloor: session.fullFrameReceipt,
      fullFrameReady: false,
      phase: "repair",
      deadlineAt: Date.now() + VIEWPORT_REPAIR_TIMEOUT_MS,
      timer: null,
      accepted,
    };
    session.attempt = attempt;
    emitViewportStatus(session, {
      status: "repairing",
      sequence: desired.sequence,
      effectiveCols: accepted.effectiveCols,
      effectiveRows: accepted.effectiveRows,
      channelResizeSeq: accepted.channelResizeSeq,
    });
    attempt.timer = setTimeout(() => {
      if (viewportSessions.get(session.sessionId) !== session
        || session.desired !== desired
        || session.attempt !== attempt) return;
      scheduleViewportRetry(session, desired, "optimistic viewport did not produce its authoritative full frame");
    }, VIEWPORT_REPAIR_TIMEOUT_MS);
    return admission;
  }

  if (currentDesired) supersedeViewportDesired(session, "newer viewport intent");
  const sequence = session.sequenceFloor + 1n;
  const { promise, resolve } = Promise.withResolvers<ViewportOutcome>();
  const admission: ViewportAdmission = { sequence, result: promise };
  const desired: ViewportDesired = {
    sequence,
    cols: value.cols,
    rows: value.rows,
    cause: value.cause,
    heldCellSeq,
    repairRequired,
    updatedAt: Date.now(),
    admission,
    resolve,
    settled: false,
    retryCount: 0,
    needsSequenceAdvance: false,
  };
  session.desired = desired;
  updateViewportSequence(session, desired, sequence);
  const sync = currentSyncV2TerminalState();
  if (sync?.ready) {
    dispatchViewportDesired(session, desired, sync);
  } else {
    emitViewportStatus(session, { status: "pending", sequence, repairRequired });
  }
  return admission;
}

export function acquireTerminalViewportOwner(sessionId: string): TerminalViewportOwner {
  const session = viewportSession(sessionId);
  if (session.desired) {
    supersedeViewportDesired(session, "newer viewport owner");
  } else {
    clearViewportAttempt(session);
    clearViewportRetry(session);
  }
  if (session.ownerToken !== null && session.listeners.size > 0) {
    emitViewportStatus(session, {
      status: "superseded",
      sequence: session.sequenceFloor,
      reason: "newer viewport owner",
    });
  }
  session.listeners.clear();
  session.confirmed = null;
  session.status = null;
  const token = ++nextViewportOwnerToken;
  session.ownerToken = token;
  let disposed = false;

  return {
    token,
    claim(value) {
      if (disposed) return inactiveViewportAdmission(sessionId, token, "viewport owner was disposed");
      return claimTerminalViewport(sessionId, token, value);
    },
    heartbeat(heldCellSeqValue) {
      if (disposed) return;
      const currentSession = viewportSessions.get(sessionId);
      const desired = currentSession?.desired;
      if (!currentSession
        || currentSession.ownerToken !== token
        || !desired
        || desired.cols <= 0
        || desired.rows <= 0) return;
      let heldCellSeq: bigint;
      try {
        heldCellSeq = BigInt(heldCellSeqValue);
      } catch {
        return;
      }
      if (heldCellSeq < 0n || heldCellSeq > MAX_SAFE_CELL_SEQ) return;
      desired.heldCellSeq = heldCellSeq;
      const sync = currentSyncV2TerminalState();
      if (!sync?.ready || currentSession.retryTimer) return;
      if (currentSession.attempt
        && (currentSession.attempt.socketId !== sync.socketId
          || currentSession.attempt.processEpoch !== sync.processEpoch
          || currentSession.attempt.domainGeneration !== sync.domainGeneration)) {
        handleGeneration(sync);
      }
      if (!currentSession.attempt) {
        const confirmedCurrent = currentSession.confirmed?.socketId === sync.socketId
          && currentSession.confirmed.processEpoch === sync.processEpoch
          && currentSession.confirmed.domainGeneration === sync.domainGeneration
          && currentSession.confirmed.sequence === desired.sequence;
        if (!confirmedCurrent) {
          dispatchViewportDesired(currentSession, desired, sync);
          return;
        }
      }
      sendViewportCommand(currentSession, desired, sync, HEARTBEAT_CAUSE);
    },
    noteFullFrame(frame) {
      if (disposed) return;
      const currentSession = viewportSessions.get(sessionId);
      if (!currentSession
        || currentSession.ownerToken !== token
        || frame.gridEpoch.length === 0
        || !Number.isSafeInteger(frame.seq)
        || frame.seq <= 0) return;
      if (currentSession.fullFrameGridEpoch === frame.gridEpoch
        && frame.seq <= currentSession.fullFrameSeq) return;
      currentSession.fullFrameGridEpoch = frame.gridEpoch;
      currentSession.fullFrameSeq = frame.seq;
      currentSession.fullFrameReceipt += 1;
      const attempt = currentSession.attempt;
      const desired = currentSession.desired;
      if (!attempt
        || !desired
        || currentSession.fullFrameReceipt <= attempt.fullFrameReceiptFloor) return;
      attempt.fullFrameReady = true;
      if (attempt.accepted) finishViewportReady(currentSession, desired, attempt);
    },
    subscribeStatus(listener) {
      if (disposed) return () => undefined;
      const currentSession = viewportSessions.get(sessionId);
      if (!currentSession
        || currentSession.ownerToken !== token
        || currentSession.listeners.size >= MAX_VIEWPORT_STATUS_LISTENERS) return () => undefined;
      currentSession.listeners.add(listener);
      if (currentSession.status) {
        try {
          listener(currentSession.status);
        } catch {
          // A status observer cannot perturb terminal ownership.
        }
      }
      let subscribed = true;
      return () => {
        if (!subscribed) return;
        subscribed = false;
        if (viewportSessions.get(sessionId)?.ownerToken === token) {
          currentSession.listeners.delete(listener);
        }
      };
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      const currentSession = viewportSessions.get(sessionId);
      if (!currentSession || currentSession.ownerToken !== token) return;
      currentSession.ownerToken = null;
      currentSession.listeners.clear();
    },
  };
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

export function consumeLastInputSendTs(sessionId: string): number | undefined {
  const value = lastInputSendTs.get(sessionId);
  if (value !== undefined) lastInputSendTs.delete(sessionId);
  return value;
}

export function inputMapSizes(): number {
  return lastInputSendTs.size + inputLanes.size;
}

export interface TerminalOutboundSnapshot {
  claim: {
    owner_token: string | null;
    sequence_floor: string;
    status: TerminalViewportOwnerStatus["status"] | null;
    desired: {
      client_seq: string;
      cols: number;
      rows: number;
      cause: number;
      held_cell_seq: string;
      updated_at_ms: number;
    } | null;
    confirmed: {
      client_seq: string;
      socket_id: string;
      domain_generation: string;
      effective_cols: number;
      effective_rows: number;
    } | null;
    attempt: {
      client_seq: string;
      socket_id: string;
      domain_generation: string;
      phase: "result" | "repair";
      deadline_at_ms: number;
    } | null;
    retry: {
      at_ms: number;
      reason: string;
    } | null;
  };
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
  const session = viewportSessions.get(sessionId);
  const desired = session?.desired;
  const preclaim = session?.preclaim;
  const sync = currentSyncV2TerminalState();
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
    claim: {
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
    },
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
  const session = viewportSessions.get(sessionId);
  if (session) evictViewportSession(session, "session closed");
  smokeRejectNextViewportClaims.delete(sessionId);
  smokeRejectedViewportClaimCounts.delete(sessionId);
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
  for (const session of [...viewportSessions.values()]) {
    evictViewportSession(session, "test reset");
  }
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
  viewportSessions.clear();
  smokeRejectNextViewportClaims.clear();
  smokeRejectedViewportClaimCounts.clear();
  inputLanes.clear();
  lastInputSendTs.clear();
  observedSocketId = null;
  nextInputSeq = 0n;
  smokeTerminalInputObserver = null;
  safeSessionStorage()?.removeItem(STORAGE_KEY);
}
