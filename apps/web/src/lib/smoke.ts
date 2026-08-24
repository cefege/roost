// Smoke backdoor — exposes a window.__smoke API so the terminal tier
// (smoke/terminal/) can drive PTY input + read state without going
// through wterm's textarea + KeyboardEvent pipeline (which is brittle
// for synthetic events).
//
// Only installed when localStorage.roostSmoke === "1"; smoke/terminal/
// fixtures.ts sets it in an init script before the SPA boots.
//
// The API contract (SmokeApi + probe/scan result types) lives in
// lib/smokeTypes.ts — harness-side consumers type against it without
// importing this implementation module (which imports them right back).

import { coordClient } from "../connect.ts";
import {
  forceSyncMaxBackoff as forceSyncMaxBackoffImpl,
  syncRedialStatus as syncRedialStatusImpl,
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
} from "../store/terminal-stream-diagnostics.ts";
import { perfCounters, leakSample, resetPerfCounters as resetPerfCountersImpl } from "./leakWatch.ts";
import { rootStore, setRootStore } from "../store/root.ts";
import { workerPathBasename } from "./nativePath.ts";
import { setForceHidden, setForceVisible } from "./pageVisible.ts";
import {
  SCROLLBACK_FLOOR_REASON,
  scrollbackBackfillRequestCount as scrollbackBackfillRequestCountImpl,
} from "./scrollbackBackfill.ts";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import {
  sendTerminalInput,
  setSmokeTerminalInputObserver,
} from "../ws/sync-outbound.ts";
import {
  rendererRegistryEntry,
} from "./terminalPreview.ts";
import { MAX_HELD_SCROLLBACK_ROWS } from "./cellRenderer.ts";
import { phaseTimeline as phaseTimelineImpl } from "./diag.ts";
import {
  beginTerminalTiming as beginTerminalTimingImpl,
  finishTerminalTiming as finishTerminalTimingImpl,
  runFlow as runFlowImpl,
  runRenderStress as runRenderStressImpl,
  waitForPaintedCursor as waitForPaintedCursorImpl,
  waitForPaintedMarker as waitForPaintedMarkerImpl,
} from "./smokeHarness.ts";
import {
  terminalBrowserStreamSnapshot,
  type TerminalBrowserStreamSnapshot,
} from "./terminalDiagSnapshot.ts";

import type { SmokeApi, TerminalStreamProbe } from "./smokeTypes.ts";
export type {
  PaintedCursorProof,
  RetainedMarkerScan,
  SmokeApi,
  SmokeTerminalInputBatch,
  SmokeTerminalInputCapture,
  TerminalStreamProbe,
} from "./smokeTypes.ts";

// Created-resource registry, sessionStorage-backed (per tab, survives a reload).
// A spec that reloads mid-test — composer draft retention, deep-history resume,
// zoom — would otherwise wipe an in-memory Set and leak its sessions and
// workspaces into the shared test stack, where the extra live PTYs perturb
// every later scroll/frame assertion. cleanupCreated is the only reader.
const CREATED_KEY = "roostSmoke.created.v1";
const TERMINAL_INPUT_CAPTURE_MAX_BATCHES = 512;
const TERMINAL_INPUT_CAPTURE_MAX_BYTES = 1024 * 1024;

function diagnosticRecord(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function diagnosticBuild(
  value: unknown,
): { git_sha: string | null; artifact_version: string | null } | null {
  const record = diagnosticRecord(value);
  if (!record) return null;
  return {
    git_sha: typeof record.git_sha === "string" ? record.git_sha : null,
    artifact_version: typeof record.artifact_version === "string"
      ? record.artifact_version
      : null,
  };
}
function normalizeTerminalStreamProbe(
  sessionId: string,
  browser: TerminalBrowserStreamSnapshot,
  rawSnapshot: unknown,
): TerminalStreamProbe {
  const root = diagnosticRecord(rawSnapshot);
  if (!root) throw new Error("coordinator diagnostic snapshot was not an object");
  const coordRecord = diagnosticRecord(root.coord);
  const coordSessions = diagnosticRecord(coordRecord?.sessions);
  const coordSession = diagnosticRecord(coordSessions?.[sessionId]);
  const terminalView = diagnosticRecord(coordSession?.terminal_view);
  const terminalEffective = diagnosticRecord(terminalView?.effective);
  const terminalControl = terminalView ? {
    active_view_count: terminalView.activeViews, parked_view_count: terminalView.parkedViews,
    stream_id: terminalView.streamId, unavailable: terminalView.unavailable,
    effective_cols: terminalEffective?.cols ?? null, effective_rows: terminalEffective?.rows ?? null,
  } : null;
  const route = diagnosticRecord(coordSession?.route);
  const workerFp = typeof route?.worker_fp === "string" ? route.worker_fp : null;
  const workers = diagnosticRecord(root.workers);
  const workerEnvelope = workerFp ? diagnosticRecord(workers?.[workerFp]) : null;
  const workerStatus = workerEnvelope?.status;
  const responseMs = typeof workerEnvelope?.response_ms === "number"
    && Number.isFinite(workerEnvelope.response_ms)
    ? workerEnvelope.response_ms
    : null;
  const workerSnapshot = workerStatus === "ok"
    ? diagnosticRecord(workerEnvelope?.snapshot)
    : null;
  const workerSessions = diagnosticRecord(workerSnapshot?.sessions);
  const workerSession = diagnosticRecord(workerSessions?.[sessionId]);
  const workerError = diagnosticRecord(workerEnvelope?.error);
  const capturedAtMs = typeof root.captured_at_ms === "number"
    && Number.isFinite(root.captured_at_ms)
    ? root.captured_at_ms
    : browser.captured_at_ms;

  return {
    captured_at_ms: capturedAtMs,
    session_id: sessionId,
    browser,
    coord: coordRecord ? {
      build: diagnosticBuild(coordRecord.build) ?? {
        git_sha: null,
        artifact_version: null,
      },
      session: coordSession,
      terminal_control: terminalControl,
    } : null,
    worker: {
      worker_fp: workerFp,
      status: workerStatus === "ok" || workerStatus === "error"
        ? workerStatus
        : "missing",
      response_ms: responseMs,
      build: diagnosticBuild(workerSnapshot?.build),
      session: workerSession,
      error: workerStatus === "error" ? {
        code: typeof workerError?.code === "string" ? workerError.code : null,
        message: typeof workerError?.message === "string" ? workerError.message : null,
      } : null,
    },
  };
}

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
    async runFlow(options) {
      return runFlowImpl(api, options);
    },
    async runRenderStress(options) {
      return runRenderStressImpl(api, options);
    },
    async waitForPaintedMarker(sessionId, marker, timeoutMs) {
      return waitForPaintedMarkerImpl(sessionId, marker, timeoutMs);
    },
    async waitForPaintedCursor(sessionId, expected, timeoutMs) {
      return waitForPaintedCursorImpl(sessionId, expected, timeoutMs);
    },
    async terminalStreamProbe(sessionId) {
      const browser = terminalBrowserStreamSnapshot(sessionId);
      const response = await coordClient.diagSnapshot({
        spaStateJson: JSON.stringify(browser),
      });
      let decoded: unknown;
      try {
        decoded = JSON.parse(response.snapshotJson);
      } catch (error) {
        throw new Error(`coordinator diagnostic snapshot was invalid JSON: ${String(error)}`);
      }
      return normalizeTerminalStreamProbe(sessionId, browser, decoded);
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
      // Every page carries the floor its own clamp hit; the LAST one is the page
      // that established the floor this scan reports.
      let retainedFloorReason: ScrollbackHistoryFloor = "none";
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
        retainedFloorReason = SCROLLBACK_FLOOR_REASON[response.historyFloor] ?? "none";
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
        retainedFloorReason,
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
    paintedScrollback(sessionId) {
      const renderer = rendererRegistryEntry(sessionId)?.renderer;
      return renderer?.paintPresentation(MAX_HELD_SCROLLBACK_ROWS) ?? {
        rows: [],
        headSpacerPx: 0,
        tailGapPx: 0,
        readerAnchor: null,
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
    state() {
      return {
        sessions: { ...rootStore.sessions },
        workspaces: { ...rootStore.workspaces },
        workers: { ...rootStore.workers },
        pair_requests: { ...rootStore.pair_requests },
      };
    },
    forceSyncMaxBackoff() {
      forceSyncMaxBackoffImpl();
    },
    syncRedialStatus() {
      return syncRedialStatusImpl();
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
