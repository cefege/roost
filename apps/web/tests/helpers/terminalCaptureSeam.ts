// Shared non-suite fixture for the terminal capture-control suites: identifiers,
// UI-state and result builders, and the module bodies each suite hands to
// mock.module for the recorder seam and the authenticated download boundary.
// Deliberately free of top-level await and of mock.module calls — a suite must own
// both, because `bun test --isolate` can run a suite body before a helper's own
// top-level awaits settle. Consumers: terminalSnapshotFacade.test.ts,
// terminalCaptureControl.test.ts.

import type {
  TerminalCaptureReason,
  TerminalCaptureResult,
  TerminalCaptureStatus,
} from "@roost/shared/terminal-capture";

export const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const RECORDING_ID = "11111111-1111-4111-8111-111111111111";

export interface CaptureUiStateFixture {
  sessionId: string;
  phase: "idle" | "arming" | "recording" | "expired" | "error";
  recordingId: string | null;
  expiresAtMs: number | null;
  lastResult: TerminalCaptureResult | null;
  lastError: string | null;
  heldEvidence: boolean;
}

export function captureUiState(phase: CaptureUiStateFixture["phase"]): CaptureUiStateFixture {
  return {
    sessionId: SESSION_ID,
    phase,
    recordingId: phase === "idle" ? null : RECORDING_ID,
    expiresAtMs: phase === "recording" ? 5_000 : null,
    lastResult: null,
    lastError: phase === "error" ? "worker_timeout" : null,
    heldEvidence: false,
  };
}

export function captureResult(
  action: "start" | "capture" | "stop",
  status: TerminalCaptureStatus,
  overrides: Partial<TerminalCaptureResult> = {},
): TerminalCaptureResult {
  return {
    capture_id: "22222222-2222-4222-8222-222222222222",
    recording_id: RECORDING_ID,
    session_id: SESSION_ID,
    action,
    status,
    expires_at_ms: null,
    worker_fp: "worker-fp",
    path: null,
    byte_length: null,
    error: null,
    recent_worker_capture: null,
    ...overrides,
  };
}

/** Everything the stubbed seam observed, in call order. */
export interface CaptureSeamLog {
  readonly order: string[];
  readonly freezes: Array<{ sessionId: string; reason: TerminalCaptureReason }>;
  readonly discarded: string[];
  readonly localDownloads: string[];
  readonly downloadedHrefs: string[];
}

/** What the stubbed seam reports back, plus the live lease-state listener. */
export interface CaptureSeamState {
  uiState: CaptureUiStateFixture;
  startResult: TerminalCaptureResult;
  stopResult: TerminalCaptureResult;
  frozenResult: TerminalCaptureResult;
  /** Make START reject, standing in for a transport fault that never produced a
   *  result the UI could project. */
  throwOnStart: boolean;
  listener: ((state: CaptureUiStateFixture) => void) | null;
}

export function createCaptureSeamLog(): CaptureSeamLog {
  return { order: [], freezes: [], discarded: [], localDownloads: [], downloadedHrefs: [] };
}

export function createCaptureSeamState(): CaptureSeamState {
  return {
    uiState: captureUiState("idle"),
    startResult: captureResult("start", "recording"),
    stopResult: captureResult("stop", "stopped"),
    frozenResult: captureResult("capture", "captured", {
      path: "/var/roost/terminal-incident-x.json.gz",
      byte_length: 12,
    }),
    throwOnStart: false,
    listener: null,
  };
}

export function resetCaptureSeam(state: CaptureSeamState, log: CaptureSeamLog): void {
  log.order.length = 0;
  log.freezes.length = 0;
  log.discarded.length = 0;
  log.localDownloads.length = 0;
  log.downloadedHrefs.length = 0;
  const fresh = createCaptureSeamState();
  state.uiState = fresh.uiState;
  state.startResult = fresh.startResult;
  state.stopResult = fresh.stopResult;
  state.frozenResult = fresh.frozenResult;
  state.throwOnStart = false;
}

/** Body for `mock.module("…/lib/terminalIncidentCapture.ts", …)`. The renderer and
 *  replica observers are included because terminal-render-scheduler.ts imports
 *  them from this same module; a partial mock breaks that importer. */
export function terminalCaptureSeamModule(
  state: CaptureSeamState,
  log: CaptureSeamLog,
): Record<string, unknown> {
  return {
    terminalCaptureUiState: () => state.uiState,
    subscribeTerminalCaptureUiState(
      _sessionId: string,
      listener: (next: CaptureUiStateFixture) => void,
    ) {
      state.listener = listener;
      return () => { state.listener = null; };
    },
    freezeTerminalCaptureEvidence(sessionId: string, reason: TerminalCaptureReason) {
      log.freezes.push({ sessionId, reason });
      log.order.push("freeze");
      return `frozen-${log.freezes.length}`;
    },
    discardTerminalCaptureEvidence(token: string) {
      log.discarded.push(token);
      log.order.push(`discard:${token}`);
    },
    downloadLocalTerminalEvidence(token: string) {
      log.localDownloads.push(token);
    },
    async captureTerminalIncidentFrozen(token: string) {
      log.order.push(`capture:${token}`);
      return state.frozenResult;
    },
    async captureTerminalIncident(sessionId: string, reason: TerminalCaptureReason) {
      log.order.push(`capture-live:${sessionId}:${reason}`);
      return state.frozenResult;
    },
    async startTerminalCapture(sessionId: string) {
      log.order.push(`start:${sessionId}`);
      if (state.throwOnStart) throw new Error("transport");
      return state.startResult;
    },
    async stopTerminalCapture(sessionId: string) {
      log.order.push(`stop:${sessionId}`);
      return state.stopResult;
    },
    disposeTerminalIncidentRecorder() {},
    noteTerminalRenderApply: () => {},
    noteTerminalRenderApplied: () => {},
    noteTerminalRendererDisposed: () => {},
  };
}

/** Body for `mock.module("…/lib/downloadWorkerFile.ts", …)`: the authenticated
 *  chunked read is the boundary, so the href it receives is the assertion. */
export function downloadWorkerFileModule(log: CaptureSeamLog): Record<string, unknown> {
  return {
    parseFileHref: () => null,
    async downloadWorkerFileByHref(href: string) {
      log.downloadedHrefs.push(href);
    },
  };
}

/** Publish a lease-state change the way the recorder would. */
export function emitLeaseState(state: CaptureSeamState, next: CaptureUiStateFixture): void {
  if (!state.listener) throw new Error("controller did not subscribe to lease state");
  state.listener(next);
}

/** Let one settle() chain finish: the seam promise, the announce and its toast. */
export async function drainSettle(): Promise<void> {
  for (let idx = 0; idx < 4; idx++) await Promise.resolve();
}
