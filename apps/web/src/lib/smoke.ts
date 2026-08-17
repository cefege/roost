// Smoke backdoor — exposes a window.__smoke API so /roost-smoke
// (humanchrome) can drive PTY input + read state without going
// through wterm's textarea + KeyboardEvent pipeline (which is brittle
// for synthetic events).
//
// Only installed when localStorage.roostSmoke === "1". User runs
// `localStorage.roostSmoke = "1"; location.reload()` in the smoke tab.
//
// Phase-26 smoke-backdoor. crpc6: migrated from tRPC to Connect.

import { coordClient } from "../connect.ts";
import {
  forceSyncReconnect as forceSyncReconnectImpl,
  forceSyncRetryExhausted as forceSyncRetryExhaustedImpl,
  pauseSyncTransport as pauseSyncTransportImpl,
  resumeSyncTransport as resumeSyncTransportImpl,
  cellFrameCount as cellFrameCountImpl,
  cellFullFrameCount as cellFullFrameCountImpl,
  lastFullFrameSbRows as lastFullFrameSbRowsImpl,
  cellGridEpoch as cellGridEpochImpl,
  syncWsGeneration as syncWsGenerationImpl,
} from "../store/sync.ts";
import {
  dropNextCellFrame as dropNextCellFrameImpl,
  droppedCellFrameCount as droppedCellFrameCountImpl,
} from "../store/sync-dispatch.ts";
import { perfCounters, leakSample, resetPerfCounters as resetPerfCountersImpl } from "./leakWatch.ts";
import { rootStore, setRootStore } from "../store/root.ts";
import { workerPathBasename } from "./nativePath.ts";
import { setForceHidden, setForceVisible } from "./pageVisible.ts";
import { scrollbackBackfillRequestCount as scrollbackBackfillRequestCountImpl } from "./scrollbackBackfill.ts";
import {
  sendTerminalInput,
  setSmokeTerminalInputObserver,
} from "../ws/sync-outbound.ts";
import { phaseTimeline as phaseTimelineImpl } from "./diag.ts";
import type { SpaPhaseTimeline } from "./diag.ts";
import {
  beginTerminalTiming as beginTerminalTimingImpl,
  finishTerminalTiming as finishTerminalTimingImpl,
  runFlow as runFlowImpl,
  runRenderStress as runRenderStressImpl,
  waitForPaintedMarker as waitForPaintedMarkerImpl,
} from "./smokeHarness.ts";
import type {
  PaintedMarkerProof,
  TerminalTimingKind,
  TerminalTimingResult,
} from "./smokeHarness.ts";

export interface SmokeTerminalInputBatch {
  sessionId: string;
  data: number[];
}

export interface SmokeTerminalInputCapture {
  batches: SmokeTerminalInputBatch[];
  droppedBatches: number;
}

export type RetainedMarkerScan = {
  gridEpoch: string;
  pages: number;
  scrollbackTotal: number;
  retainedFloor: number;
  retainedCap: number;
  rowIndices: number[];
  rowGapCount: number;
  markerIds: number[];
  markerMin: number;
  markerMax: number;
  markerMissing: number;
  markerDuplicated: number[];
  markerOutOfOrder: number;
};

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
  renderProbe(sessionId: string): {
    found: boolean;
    mode: "cell" | "byte" | "none";
    fromBottom: number; atBottom: boolean;
    rowCount: number; nonEmptyRows: number;
    firstLine: string; lastLine: string;
  };
  /** Scan EVERY rendered row for `${prefix}<N>` markers. Detects history depth
   *  (min/max N), loss (missing Ns in [min,max]), and CORRUPTION (any N seen
   *  more than once = duplicated rows, the cell-mode tab-switch bug). */
  markerScan(sessionId: string, prefix: string): {
    total: number; unique: number; min: number; max: number;
    duplicated: number[]; missing: number;
    // outOfOrder: count of render-order inversions (= mangled/out-of-position
    // rows). firstInversion: the marker N where order first breaks, or -1.
    outOfOrder: number; firstInversion: number;
  };
  /** Exact-marker paint proof: non-zero Range geometry inside the terminal and
   * visual viewport, visible computed style, and the same proof after 2×rAF. */
  waitForPaintedMarker(sessionId: string, marker: string, timeoutMs?: number): Promise<PaintedMarkerProof>;
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
  /** Scroll a pane to an edge so the harness can assert history is reachable. */
  scrollToEdge(sessionId: string, edge: "top" | "bottom"): void;
  /** Snapshot of current SPA store state. */
  state(): {
    sessions: Record<string, unknown>;
    workspaces: Record<string, unknown>;
    workers: Record<string, unknown>;
    pair_requests: Record<string, unknown>;
  };
  /** Automation visibility pin: treat the page as foregrounded while the tab
   *  is hidden/occluded — claims stay held, timers tick, sync stream stays
   *  managed (lib/pageVisible.ts::setForceVisible). Fixes the FOREGROUND
   *  GOTCHA class of false verification failures (Author 2026-07-11 "push to
   *  front via API"). Chrome may still starve a long-backgrounded tab's
   *  HTTP/2 stream at the transport layer — keep hidden probe windows short. */
  forceVisible(on: boolean): void;
  /** Pin app-level visibility to background; false releases the pin. */
  forceHidden(on: boolean): void;
  /** Force a firehose WebSocket reconnect (closes the live WS). */
  forceSyncReconnect(): void;
  /** Park the existing Sync loop in its retry-exhausted state without recovering it. */
  forceSyncRetryExhausted(): void;
  /** Close and pause the Sync tube; paired resume starts a fresh generation. */
  pauseSyncTransport(): void;
  resumeSyncTransport(): void;
  /** How many cell frames have arrived for this session (smoke verification). */
  cellFrameCount(sessionId: string): number;
  /** How many FULL cell frames have arrived — a reveal of a current pane must
   *  not move this (the worker's claim snapshot is what it proves absent). */
  cellFullFrameCount(sessionId: string): number;
  /** Historical rows carried by the last authoritative full frame; viewport-
   * only resume requires this to remain exactly zero. */
  lastFullFrameSbRows(sessionId: string): number;
  /** Epoch-addressed retained-history RPCs issued for this session. */
  scrollbackBackfillRequestCount(sessionId: string): number;
  /** Opaque worker grid epoch on the latest cell frame. */
  cellGridEpoch(sessionId: string): string;
  /** Drop exactly the next cell frame before counters and pane dispatch. */
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
  runFlow(): Promise<{ steps: Array<{ name: string; pass: boolean; detail: unknown }>; summary: string }>;
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

// Created-resource registry, sessionStorage-backed (per tab, survives a reload).
// A spec that reloads mid-test — composer draft retention, deep-history resume,
// zoom — would otherwise wipe an in-memory Set and leak its sessions and
// workspaces into the shared test stack, where the extra live PTYs perturb
// every later scroll/frame assertion. cleanupCreated is the only reader.
const CREATED_KEY = "roostSmoke.created.v1";
const TERMINAL_INPUT_CAPTURE_MAX_BATCHES = 512;
const TERMINAL_INPUT_CAPTURE_MAX_BYTES = 1024 * 1024;

export function maybeInstallSmokeBackdoor(): void {
  if (typeof window === "undefined") return;
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;

  const spawned = new Set<string>();
  const workspaces = new Set<string>();
  try {
    const raw = sessionStorage.getItem(CREATED_KEY);
    const carried = raw ? JSON.parse(raw) : null;
    if (carried && typeof carried === "object" && "sessions" in carried && "workspaces" in carried) {
      const { sessions, workspaces: ws } = carried as { sessions: unknown; workspaces: unknown };
      if (Array.isArray(sessions)) for (const id of sessions) spawned.add(String(id));
      if (Array.isArray(ws)) for (const id of ws) workspaces.add(String(id));
    }
  } catch { /* privacy mode / unparseable — start clean */ }
  const persistCreated = () => {
    try {
      sessionStorage.setItem(CREATED_KEY, JSON.stringify({
        sessions: [...spawned],
        workspaces: [...workspaces],
      }));
    } catch { /* privacy mode */ }
  };
  const terminalInputBatches: Array<{ sessionId: string; data: Uint8Array }> = [];
  let terminalInputBytes = 0;
  let droppedTerminalInputBatches = 0;
  const clearTerminalInputCapture = () => {
    terminalInputBatches.length = 0;
    terminalInputBytes = 0;
    droppedTerminalInputBatches = 0;
  };
  setSmokeTerminalInputObserver((sessionId, data) => {
    if (data.byteLength > TERMINAL_INPUT_CAPTURE_MAX_BYTES) {
      droppedTerminalInputBatches++;
      return;
    }
    while (
      terminalInputBatches.length >= TERMINAL_INPUT_CAPTURE_MAX_BATCHES
      || terminalInputBytes + data.byteLength > TERMINAL_INPUT_CAPTURE_MAX_BYTES
    ) {
      const evicted = terminalInputBatches.shift();
      if (!evicted) break;
      terminalInputBytes -= evicted.data.byteLength;
      droppedTerminalInputBatches++;
    }
    terminalInputBatches.push({ sessionId, data });
    terminalInputBytes += data.byteLength;
  });
  const api: SmokeApi = {
    async cleanupCreated() {
      const killedSessions: string[] = [];
      const deletedWorkspaces: string[] = [];
      const errors: string[] = [];
      for (const sessionId of spawned) {
        try {
          await coordClient.sessionsKill({ sessionId });
          killedSessions.push(sessionId);
        } catch (error) {
          errors.push(`kill ${sessionId}: ${String(error)}`);
        }
      }
      spawned.clear();
      for (const workspaceId of workspaces) {
        for (let attempt = 0; attempt < 2; attempt++) {
          try {
            const { workspaces: current } = await coordClient.workspacesList({});
            const workspace = current.find((item) => item.id === workspaceId);
            if (!workspace) break;
            await coordClient.workspacesDelete({ id: workspace.id, ifVersion: workspace.version });
            deletedWorkspaces.push(workspaceId);
            break;
          } catch (error) {
            if (attempt === 1) errors.push(`delete workspace ${workspaceId}: ${String(error)}`);
          }
        }
      }
      workspaces.clear();
      persistCreated();
      return { killedSessions, deletedWorkspaces, errors };
    },
    async runFlow() {
      return runFlowImpl(api);
    },
    async runRenderStress(options) {
      return runRenderStressImpl(api, options);
    },
    async waitForPaintedMarker(sessionId, marker, timeoutMs) {
      return waitForPaintedMarkerImpl(sessionId, marker, timeoutMs);
    },
    async beginTerminalTiming(kind, sessionId) {
      return beginTerminalTimingImpl(kind, sessionId);
    },
    async finishTerminalTiming(timingId, sessionId, marker, timeoutMs) {
      return finishTerminalTimingImpl(timingId, sessionId, marker, timeoutMs);
    },
    phaseTimeline() {
      return phaseTimelineImpl();
    },
    async retainedMarkerScan(sessionId, prefix, pageRows = 512) {
      if (!Number.isSafeInteger(pageRows) || pageRows < 1 || pageRows > 4_096) {
        throw new Error(`invalid retained marker page size: ${pageRows}`);
      }
      const gridEpoch = cellGridEpochImpl(sessionId);
      if (!gridEpoch) throw new Error(`no cell grid epoch for ${sessionId}`);
      const escapedPrefix = prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const markerPattern = new RegExp(`${escapedPrefix}(\\d+)`, "g");
      const chunks: Array<Array<{ index: number; spans: Array<{ text: string }> }>> = [];
      let endRow = Number.MAX_SAFE_INTEGER;
      let scrollbackTotal: number | undefined;
      let retainedFloor = 0;
      let pages = 0;
      for (;;) {
        if (pages >= 128) throw new Error(`retained marker pagination exceeded 128 pages for ${sessionId}`);
        const response = await coordClient.sessionsGetScrollbackCells({
          sessionId,
          endRow: BigInt(endRow),
          maxRows: pageRows,
          gridEpoch,
        });
        pages++;
        const responseStart = Number(response.startRow);
        const responseEnd = Number(response.endRow);
        const responseTotal = Number(response.scrollbackTotal);
        if (
          response.gridEpoch !== gridEpoch
          || !Number.isSafeInteger(responseStart)
          || !Number.isSafeInteger(responseEnd)
          || !Number.isSafeInteger(responseTotal)
          || responseStart < 0
          || responseEnd < responseStart
          || responseTotal < responseEnd
        ) {
          throw new Error(`invalid retained marker page for ${sessionId}`);
        }
        if (scrollbackTotal === undefined) scrollbackTotal = responseTotal;
        else if (scrollbackTotal !== responseTotal) {
          throw new Error(`scrollback changed during retained marker scan for ${sessionId}`);
        }
        for (let index = 0; index < response.rows.length; index++) {
          if (response.rows[index]!.index !== responseStart + index) {
            throw new Error(`non-contiguous retained page for ${sessionId} at ${responseStart + index}`);
          }
        }
        if (response.rows.length > 0) chunks.unshift(response.rows);
        if (responseStart === 0 || response.rows.length === 0) {
          retainedFloor = responseStart;
          break;
        }
        if (responseStart >= endRow) throw new Error(`retained marker page made no progress for ${sessionId}`);
        endRow = responseStart;
      }

      const rows = chunks.flat();
      const rowIndices = rows.map((row) => row.index);
      let rowGapCount = 0;
      for (let index = 1; index < rowIndices.length; index++) {
        if (rowIndices[index] !== rowIndices[index - 1]! + 1) rowGapCount++;
      }
      const markerIds: number[] = [];
      for (const row of rows) {
        const text = row.spans.map((span) => span.text).join("");
        markerPattern.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = markerPattern.exec(text)) !== null) {
          const value = Number(match[1]);
          if (Number.isSafeInteger(value)) markerIds.push(value);
        }
      }
      const counts = new Map<number, number>();
      for (const marker of markerIds) counts.set(marker, (counts.get(marker) ?? 0) + 1);
      const unique = [...counts.keys()];
      const markerMin = unique.length > 0 ? Math.min(...unique) : 0;
      const markerMax = unique.length > 0 ? Math.max(...unique) : 0;
      const markerDuplicated = unique.filter((value) => (counts.get(value) ?? 0) > 1).sort((a, b) => a - b);
      let markerMissing = 0;
      for (let value = markerMin; value <= markerMax && unique.length > 0; value++) {
        if (!counts.has(value)) markerMissing++;
      }
      let markerOutOfOrder = 0;
      for (let index = 1; index < markerIds.length; index++) {
        if (markerIds[index]! < markerIds[index - 1]!) markerOutOfOrder++;
      }
      const total = scrollbackTotal ?? 0;
      return {
        gridEpoch,
        pages,
        scrollbackTotal: total,
        retainedFloor,
        retainedCap: total - retainedFloor,
        rowIndices,
        rowGapCount,
        markerIds,
        markerMin,
        markerMax,
        markerMissing,
        markerDuplicated,
        markerOutOfOrder,
      };
    },
    async input(sessionId, text) {
      const admission = sendTerminalInput(sessionId, new TextEncoder().encode(text));
      if (!admission.accepted) throw new Error(admission.reason);
      const outcome = await admission.result;
      if (outcome.status !== "accepted") throw new Error(outcome.reason);
    },
    terminalInputCapture() {
      return {
        batches: terminalInputBatches.map(({ sessionId, data }) => ({
          sessionId,
          data: Array.from(data),
        })),
        droppedBatches: droppedTerminalInputBatches,
      };
    },
    resetTerminalInputCapture() {
      clearTerminalInputCapture();
    },
    paneFocused(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const ta = slot?.querySelector("textarea") ?? null;
      return {
        hasSlot: !!slot,
        hasTextarea: !!ta,
        focused: !!ta && document.activeElement === ta,
      };
    },
    viewportText(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      return (slot?.textContent ?? "").replace(/\s+/g, " ").trim();
    },
    renderProbe(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const c = slot?.querySelector(".cell-grid") as HTMLElement | null;
      if (!c) {
        return { found: false, mode: "none" as const, scrollTop: 0, scrollHeight: 0,
          clientHeight: 0, fromBottom: 0, atBottom: false, rowCount: 0,
          nonEmptyRows: 0, firstLine: "", lastLine: "" };
      }
      const rows = Array.from(c.querySelectorAll(".cell-row"))
        .map((r) => (r.textContent ?? "").replace(/ /g, " ").replace(/\s+$/, ""));
      const nonEmpty = rows.filter((t) => t.trim() !== "");
      const fromBottom = c.scrollHeight - c.scrollTop - c.clientHeight;
      return {
        found: true, mode: "cell" as const,
        scrollTop: Math.round(c.scrollTop), scrollHeight: Math.round(c.scrollHeight),
        clientHeight: Math.round(c.clientHeight), fromBottom: Math.round(fromBottom),
        atBottom: c.scrollTop >= Math.max(0, c.scrollHeight - c.clientHeight),
        rowCount: rows.length, nonEmptyRows: nonEmpty.length,
        firstLine: nonEmpty[0] ?? "", lastLine: nonEmpty[nonEmpty.length - 1] ?? "",
      };
    },
    markerScan(sessionId, prefix) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const c = slot?.querySelector(".cell-grid") as HTMLElement | null;
      const re = new RegExp(prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "(\\d+)", "g");
      const counts = new Map<number, number>();
      const seq: number[] = []; // marker Ns in DOM render order
      for (const row of c?.querySelectorAll(".cell-row") ?? []) {
        re.lastIndex = 0;
        const text = row.textContent ?? "";
        let match: RegExpExecArray | null;
        while ((match = re.exec(text)) !== null) {
          const n = Number(match[1]);
          if (!Number.isSafeInteger(n)) continue;
          counts.set(n, (counts.get(n) ?? 0) + 1);
          seq.push(n);
        }
      }
      const ns = [...counts.keys()];
      const total = [...counts.values()].reduce((a, b) => a + b, 0);
      const min = ns.length ? Math.min(...ns) : 0;
      const max = ns.length ? Math.max(...ns) : 0;
      const duplicated = ns.filter((n) => (counts.get(n) ?? 0) > 1).sort((a, b) => a - b);
      let missing = 0;
      if (ns.length > 0) {
        for (let n = min; n <= max; n++) if (!counts.has(n)) missing++;
      }
      // Render-order inversions: `seq 1..N | echo` is strictly increasing, so
      // ANY render-order drop (seq[i+1] < seq[i]) = MANGLE — rows out of
      // position. This is the secondary-device-mangle / tab-switch-corruption
      // symptom that dup/loss counts alone miss.
      let outOfOrder = 0;
      let firstInversion = -1;
      for (let i = 1; i < seq.length; i++) {
        if (seq[i]! < seq[i - 1]!) {
          outOfOrder++;
          if (firstInversion < 0) firstInversion = seq[i]!;
        }
      }
      return { total, unique: ns.length, min, max, duplicated, missing, outOfOrder, firstInversion };
    },
    terminalDimensions(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
      const terminal = slot?.querySelector(".cell-grid") as HTMLElement | null;
      const viewport = terminal?.querySelector(".cell-viewport");
      const cols = Number.parseInt(terminal?.style.getPropertyValue("--cell-cols") ?? "", 10);
      const rows = viewport
        ? Array.from(viewport.children).filter((child) => child.classList.contains("cell-row")).length
        : 0;
      return { cols: Number.isSafeInteger(cols) ? cols : 0, rows };
    },
    scrollToEdge(sessionId, edge) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const c = slot?.querySelector(".wterm") as HTMLElement | null;
      if (!c) return;
      c.scrollTop = edge === "top" ? 0 : c.scrollHeight;
    },
    state() {
      return {
        sessions: { ...rootStore.sessions },
        workspaces: { ...rootStore.workspaces },
        workers: { ...rootStore.workers },
        pair_requests: { ...rootStore.pair_requests },
      };
    },
    forceSyncReconnect() {
      forceSyncReconnectImpl();
    },
    forceSyncRetryExhausted() {
      forceSyncRetryExhaustedImpl();
    },
    pauseSyncTransport() {
      pauseSyncTransportImpl();
    },
    resumeSyncTransport() {
      resumeSyncTransportImpl();
    },
    cellFrameCount(sessionId) {
      return cellFrameCountImpl(sessionId);
    },
    cellFullFrameCount(sessionId) {
      return cellFullFrameCountImpl(sessionId);
    },
    lastFullFrameSbRows(sessionId) {
      return lastFullFrameSbRowsImpl(sessionId);
    },
    scrollbackBackfillRequestCount(sessionId) {
      return scrollbackBackfillRequestCountImpl(sessionId);
    },
    cellGridEpoch(sessionId) {
      return cellGridEpochImpl(sessionId);
    },
    dropNextCellFrame(sessionId) {
      dropNextCellFrameImpl(sessionId);
    },
    droppedCellFrameCount(sessionId) {
      return droppedCellFrameCountImpl(sessionId);
    },
    perfProbe(sessionId) {
      const counters = perfCounters();
      const s = leakSample();
      return {
        longTaskState: counters.longTaskState,
        longTaskCount: counters.longTaskCount,
        longTaskMs: counters.longTaskMs,
        cellFrames: cellFrameCountImpl(sessionId),
        cellFullFrames: cellFullFrameCountImpl(sessionId),
        domNodes: s.dom_nodes ?? -1,
        cellRows: s.cell_rows ?? -1,
        heldSbRows: s.held_sb_rows ?? -1,
        heapMb: s.heap_mb ?? -1,
        inputRttP50: s.input_rtt_p50 ?? -1,
        inputRttP95: s.input_rtt_p95 ?? -1,
      };
    },
    resetPerfCounters() {
      resetPerfCountersImpl();
    },
    syncWsGeneration() {
      return syncWsGenerationImpl();
    },
    forceVisible(on) {
      setForceVisible(on);
    },
    forceHidden(on) {
      setForceHidden(on);
    },
    navigate(href) {
      window.dispatchEvent(new CustomEvent("roost-smoke-navigate", { detail: href }));
    },
    async kill(sessionId) {
      const res = await coordClient.sessionsKill({ sessionId });
      return { accepted: res.accepted };
    },
    async spawnShell(workerFp, folder, sessionId) {
      const res = await coordClient.sessionsSpawn({
        workerFp,
        kind: "shell",
        folder,
        sessionId,
      });
      spawned.add(res.sessionId);
      persistCreated();
      return { session_id: res.sessionId, channel_id: res.channelId };
    },
    trackCreatedSession(sessionId) {
      spawned.add(sessionId);
      persistCreated();
    },
    async createWorkspace(workerFp, folder, sessionId) {
      const existing = new Set(
        Object.values(rootStore.workspaces)
          .filter((w: { worker_fp?: string; name?: string }) => w.worker_fp === workerFp)
          .map((w: { name?: string }) => w.name ?? "")
      );
      const base = workerPathBasename(workerFp, folder) || "~";
      let name = base;
      let i = 2;
      while (existing.has(name)) { name = `${base} ${i++}`; }
      const ws = await coordClient.workspacesCreate({
        workerFp,
        name,
        folderPath: folder,
        attachSessionIds: [sessionId],
      });
      workspaces.add(ws.workspace!.id);
      persistCreated();
      const session = rootStore.sessions[sessionId] as { channel?: number } | undefined;
      return { id: ws.workspace!.id, channel: session?.channel ?? 0 };
    },
    async uploadAttachment(sessionId, sizeBytes, filename = `smoke-${sizeBytes}.bin`) {
      const { uploadAttachment } = await import("./attachments.ts");
      // Distinct bytes (not all-zero) so a corrupt assembly would mismatch.
      const buf = new Uint8Array(sizeBytes);
      for (let i = 0; i < sizeBytes; i++) buf[i] = i & 0xff;
      const file = new File([buf], filename, { type: "application/octet-stream" });
      return uploadAttachment({ id: sessionId }, file);
    },
    async attachmentProbe(sessionId, sha256, sizeBytes, filename = "probe.bin") {
      const res = await coordClient.attachmentProbe({
        sessionId, sha256, size: BigInt(sizeBytes), filename, shortPath: false,
      });
      return { hit: res.hit, abs_path: res.absPath };
    },
    async downloadWorkerFile(workerFp, path) {
      const CHUNK = 4 * 1024 * 1024;
      const parts: Uint8Array[] = [];
      let offset = 0;
      let bytes = 0;
      for (;;) {
        const res = await coordClient.filesReadChunk({ workerFp, path, offset: BigInt(offset), len: CHUNK });
        if (res.data.length) { parts.push(res.data); offset += res.data.length; bytes += res.data.length; }
        if (res.eof || res.data.length === 0) break;
      }
      const all = new Uint8Array(bytes);
      let p = 0;
      for (const part of parts) { all.set(part, p); p += part.length; }
      const digest = await crypto.subtle.digest("SHA-256", all);
      const sha256 = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
      return { bytes, sha256 };
    },
  };
  (window as Window & { __smoke?: SmokeApi }).__smoke = api;
  console.debug("[smoke] backdoor installed via window.__smoke");
}
