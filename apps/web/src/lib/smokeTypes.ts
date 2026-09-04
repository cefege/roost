// Pure type surface of the smoke backdoor (window.__smoke), split out of
// lib/smoke.ts so consumers (smokeHarness, terminal tiers) can depend on the
// contract without importing the implementation module — smoke.ts imports
// harness functions, so a type-only edge from harness→smoke was a latent
// import cycle. Type-only module: erased at runtime.

import type {
  ScrollbackHistoryFloor,
  Session,
  Worker,
  Workspace,
} from "@roost/shared/wire";
import type { RendererPaintPresentation } from "./cellRenderer.ts";
import type { SpaPhaseTimeline } from "./diag.ts";
import type {
  PaintedCursorExpected,
  PaintedCursorProof,
  PaintedMarkerProof,
  TerminalTimingKind,
  TerminalTimingResult,
} from "./smokeHarness.ts";
import type { SyncRedialStatus } from "../store/sync.ts";
import type { TerminalBrowserStreamSnapshot } from "./terminalDiagSnapshot.ts";

export type { PaintedCursorProof } from "./smokeHarness.ts";

export interface SmokeTerminalInputBatch {
  sessionId: string;
  data: number[];
}

export interface SmokeTerminalInputCapture {
  batches: SmokeTerminalInputBatch[];
  droppedBatches: number;
}

export type SmokePaintedScrollbackProbe = RendererPaintPresentation;

export type RetainedMarkerScan = {
  gridEpoch: string;
  pages: number;
  scrollbackTotal: number;
  retainedFloor: number;
  retainedCap: number;
  /** WHY the scan stopped at retainedFloor, as the worker reported it: "none" =
   *  it reached absolute row 0 and nothing is missing, "evicted" = the core's line
   *  ring rolled past those rows, "resize_replay" = a resize rebuilt the grid from
   *  the bounded byte ring and it could not reach them. */
  retainedFloorReason: ScrollbackHistoryFloor;
  rowIndices: number[];
  rowGapCount: number;
  markerIds: number[];
  markerMin: number;
  markerMax: number;
  markerMissing: number;
  markerDuplicated: number[];
  markerOutOfOrder: number;
};

export interface TerminalStreamProbe {
  captured_at_ms: number;
  session_id: string;
  browser: TerminalBrowserStreamSnapshot;
  coord: {
    build: { git_sha: string | null; artifact_version: string | null };
    session: Record<string, unknown> | null;
    terminal_control: Record<string, unknown> | null;
  } | null;
  worker: {
    worker_fp: string | null;
    status: "ok" | "error" | "missing";
    response_ms: number | null;
    build: { git_sha: string | null; artifact_version: string | null } | null;
    session: Record<string, unknown> | null;
    error: { code: string | null; message: string | null } | null;
  };
}

export interface SmokeRenderProbe {
  found: boolean;
  mode: "cell" | "byte" | "none";
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  fromBottom: number;
  atBottom: boolean;
  rowCount: number;
  nonEmptyRows: number;
  firstLine: string;
  lastLine: string;
}

export interface SmokeMarkerScan {
  total: number;
  unique: number;
  min: number;
  max: number;
  duplicated: number[];
  missing: number;
  /** Count of render-order inversions and the first displaced marker. */
  outOfOrder: number;
  firstInversion: number;
}

export interface SmokeApi {
  /** Send raw bytes through the terminal transport — BYPASSES the textarea +
   *  focus pipeline. Use for variant coverage only, NEVER as the "can input?"
   *  test: it stays green when focus is dead. For the real input check use
   *  paneFocused() + real keystrokes (chrome_keyboard). */
  input(sessionId: string, text: string): Promise<void>;
  /** Exact accepted input batches in admission order. Smoke-only and bounded;
   *  reset immediately before the UI action under observation. */
  terminalInputCapture(): SmokeTerminalInputCapture;
  resetTerminalInputCapture(): void;
  /** Real-input regression probe: is the active pane's textarea actually the
   *  document.activeElement? Returns false when focus never landed (= input is
   *  dead even though input() would still succeed). This is the assertion that
   *  catches "I can't input anything". */
  paneFocused(sessionId: string): { hasSlot: boolean; hasTextarea: boolean; focused: boolean };
  /** Visible text of a pane's rendered grid (cell rows or wterm rows). Used to
   *  assert a typed marker actually echoed back through the full chain. */
  viewportText(sessionId: string): string;
  /** Render-correctness probe: the REAL painted scroll geometry + row counts
   *  for a pane. This is what catches "scroll jumped to top", "history lost",
   *  "rows duplicated" — none of which a wire/data-* test can see. Reads the
   *  actual scroll container (.wterm, shared by byte + cell renderers). */
  renderProbe(sessionId: string): SmokeRenderProbe;
  /** Bounded presentation-owned scrollback rows, spacer/gap, and reader anchor. */
  paintedScrollback(sessionId: string): SmokePaintedScrollbackProbe;
  /** Scan EVERY rendered row for `${prefix}<N>` markers. Detects history depth
   *  (min/max N), loss (missing Ns in [min,max]), and CORRUPTION (any N seen
   *  more than once = duplicated rows, the cell-mode tab-switch bug). */
  markerScan(sessionId: string, prefix: string): SmokeMarkerScan;
  /** Exact-marker paint proof: non-zero Range geometry inside the terminal and
   * visual viewport, visible computed style, and the same proof after 2×rAF. */
  waitForPaintedMarker(sessionId: string, marker: string, timeoutMs?: number): Promise<PaintedMarkerProof>;
  /** Cursor presentation proof: connected, stable grid-aligned geometry clipped
   * by terminal + visual viewport across 2×rAF. Blink opacity is tolerated. */
  waitForPaintedCursor(
    sessionId: string,
    expected?: PaintedCursorExpected,
    timeoutMs?: number,
  ): Promise<PaintedCursorProof>;
  /** One on-demand, bounded per-session snapshot spanning browser, coordinator,
   * and the routed worker. Missing layer fields remain explicit null/missing. */
  terminalStreamProbe(sessionId: string): Promise<TerminalStreamProbe>;
  /** Begin/finish the trusted-key, reveal, resize, and optimistic paint clocks.
   * trusted_key starts on the real `isTrusted` keydown, not this method call. */
  beginTerminalTiming(kind: TerminalTimingKind, sessionId?: string): Promise<string>;
  finishTerminalTiming(
    timingId: string,
    sessionId: string,
    marker: string,
    timeoutMs?: number,
  ): Promise<TerminalTimingResult>;
  /** Latest authoritative grid dimensions represented by the painted DOM. */
  terminalDimensions(sessionId: string): { cols: number; rows: number };
  /** Bounded eager SPA bootstrap/terminal phase marks for this document. */
  phaseTimeline(): SpaPhaseTimeline;
  /** Page the server-retained cell range into a non-DOM marker accumulator. */
  retainedMarkerScan(sessionId: string, prefix: string, pageRows?: number): Promise<RetainedMarkerScan>;
  /** Snapshot of current SPA store state. */
  state(): {
    sessions: Record<string, Session>;
    workspaces: Record<string, Workspace>;
    workers: Record<string, Worker>;
    pair_requests: Record<string, unknown>;
  };
  /** Automation visibility pin: treat the page as foregrounded while hidden or
   *  occluded so terminal views remain active and Sync timers keep running.
   *  Chrome may still starve a long-backgrounded tab's transport, so hidden
   *  probe windows remain short. */
  forceVisible(on: boolean): void;
  /** Pin app-level visibility to background; false releases the pin. */
  forceHidden(on: boolean): void;
  /** Drop the Sync tube the way a network failure does, with the redial loop
   *  pre-armed to the floor production can still reach: saturated capped
   *  backoff, plus the hidden-document sleep. Recovery uses no backdoor — a
   *  visible page must heal on its own redial, a hidden one on its next
   *  resume. */
  forceSyncMaxBackoff(): void;
  /** Redial status: consecutive failures, the capped pending delay, and whether
   *  a HIDDEN document is parked. A visible document is never parked. */
  syncRedialStatus(): SyncRedialStatus;
  /** Close and pause the Sync tube; paired resume starts a fresh generation. */
  pauseSyncTransport(): void;
  resumeSyncTransport(): void;
  /** How many cell frames have arrived for this session (smoke verification). */
  cellFrameCount(sessionId: string): number;
  /** Complete full baselines received for the session replica. */
  cellFullFrameCount(sessionId: string): number;
  /** Historical rows carried by the last full (normally zero for viewport-only replicas). */
  lastFullFrameSbRows(sessionId: string): number;
  /** Epoch-addressed retained-history RPCs issued for this session. */
  scrollbackBackfillRequestCount(sessionId: string): number;
  /** Opaque worker grid epoch on the latest cell frame. */
  cellGridEpoch(sessionId: string): string;
  /** Drop terminal application frames for exactly the current full terminal generation. */
  blackholeTerminalFramesForCurrentGeneration(sessionId: string): void;
  /** ACK but suppress exactly one non-full terminal frame before replica dispatch. */
  dropNextTerminalWireDelta(sessionId: string): void;
  /** Suppress exactly the next accepted cell frame's renderer delivery after the session replica folds it. */
  dropNextCellFrame(sessionId: string): void;
  droppedCellFrameCount(sessionId: string): number;
  /** Sync WebSocket dial count. Unchanged across a refocus = the socket was
   *  kept (no JWT sign + TLS handshake + since= backfill ahead of the reveal). */
  syncWsGeneration(): number;
  /** Navigate through the live Solid router, rather than synthetic popstate. */
  navigate(href: string): void;
  /** Kill a session via the standard mutation. */
  kill(sessionId: string): Promise<{ accepted: boolean }>;
  /** Spawn a shell on a worker; returns { session_id, channel_id }. */
  spawnShell(workerFp: string, folder: string, sessionId?: string): Promise<{ session_id: string; channel_id: number }>;
  /** Create a workspace attached to a session — bypasses the cwd-picker UI. */
  createWorkspace(workerFp: string, folder: string, sessionId: string): Promise<{ id: string; channel: number }>;
  /** Register a session created through the stack API/UI for scoped smoke cleanup. */
  trackCreatedSession(sessionId: string): void;
  /** Test resources created by this tab, cleaned without touching live state. */
  cleanupCreated(): Promise<{ killedSessions: string[]; deletedWorkspaces: string[]; errors: string[] }>;
  /** `workerFp` pins the spawn target; omit it to spawn on the most recently
   *  seen worker. */
  runFlow(options?: { workerFp?: string }): Promise<{ steps: Array<{ name: string; pass: boolean; detail: unknown }>; summary: string }>;
  runRenderStress(options: {
    sessionId: string;
    prefix: string;
    screen: "main" | "alt";
    iterations: number;
  }): Promise<{ verdict: "PASS" | "FAIL"; iterations: number; failCount: number; fails: unknown[] }>;
  /** att1-stream e2e: upload a synthetic file of `sizeBytes` through the REAL
   *  chunked uploadAttachment path (Blob.slice → AttachFileChunk → worker). Used
   *  to verify uploads >50 MB round-trip end-to-end. Returns the worker abs_path. */
  uploadAttachment(sessionId: string, sizeBytes: number, filename?: string): Promise<{ abs_path: string }>;
  /** att3 dedup probe: ask the worker whether it already holds content with
   *  this SHA-256. Returns { hit, abs_path }. */
  attachmentProbe(sessionId: string, sha256: string, sizeBytes: number, filename?: string): Promise<{ hit: boolean; abs_path: string }>;
  /** Chunked-download e2e: pull a worker file via the filesReadChunk loop and
   *  return the reassembled byte length + hex SHA-256 (integrity + no-size-cap
   *  proof). */
  downloadWorkerFile(workerFp: string, path: string): Promise<{ bytes: number; sha256: string }>;
  /** Perf regression probe: main-thread jank + paint volume + DOM/heap size for
   *  one pane. Composed from the always-on leak watcher and the cell-frame
   *  counters — it adds no instrumentation of its own, so reading it cannot
   *  perturb what it measures. Only longTaskCount/longTaskMs are windowed by
   *  resetPerfCounters(); every other field is a live total, so a caller wanting
   *  a per-window figure subtracts two reads.  */
  perfProbe(sessionId: string): {
    longTaskState: "available" | "unavailable";
    longTaskCount: number; longTaskMs: number;
    cellFrames: number; cellFullFrames: number;
    domNodes: number; cellRows: number; heldSbRows: number; heapMb: number;
    inputRttP50: number; inputRttP95: number;
  };
  /** Zero the long-task accumulators so perfProbe() measures one explicit
   *  window (a flood, a resize burst) rather than the whole page lifetime. */
  resetPerfCounters(): void;
}
