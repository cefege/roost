// Gathers and freezes one browser evidence payload for a terminal incident
// capture, fits it to the request budget keeping the trigger and repair
// snapshots first, and names everything it had to drop. Also owns the local
// JSON export offered when a bundle never reached its worker.
// Called by terminalIncidentCapture.ts; reads the painted state through
// terminalIncidentCaptureObserver.ts and the bounds from @roost/protocol.

import {
  TERMINAL_CAPTURE_LIMITS,
  TERMINAL_INCIDENT_SCHEMA,
  utf8ByteLength,
  type TerminalBrowserPaintedState,
  type TerminalBrowserSection,
  type TerminalCaptureOmission,
  type TerminalCaptureReason,
  type TerminalCaptureStreamIdentity,
  type TerminalCaptureTrigger,
  type TerminalDomRow,
} from "@roost/protocol/terminal-capture";
import type { TerminalGeometry } from "@roost/protocol/viewport";
import type { CellRow } from "@roost/protocol/cell";
import { getTabId } from "../client/auth/tab-id.ts";
import { terminalStreamDiagnosticSnapshot } from "../store/terminal-stream-diagnostics.ts";
import { canonicalTerminalFrame } from "../store/terminal-stream-replica.ts";
import { rendererRegistryEntry } from "./terminalPreview.ts";
import {
  capturePaintedState,
  committedStreamIdentity,
} from "./terminalIncidentCaptureObserver.ts";
import { streamIdentityOfFrame } from "./terminalIncidentDom.ts";
import type {
  FrozenBrowserEvidence,
  TerminalIncidentRecorder,
} from "./terminalIncidentCaptureState.ts";

/** Envelope plus this layer's section, exactly as the coordinator bridge
 *  validates it before forwarding to the owning worker. */
export interface TerminalIncidentBrowserPayload {
  readonly schema: typeof TERMINAL_INCIDENT_SCHEMA;
  readonly layer: "browser";
  readonly capture_id: string;
  readonly recording_id: string;
  readonly session_id: string;
  readonly trigger: TerminalCaptureTrigger;
  readonly browser: TerminalBrowserSection;
}

interface BrowserEvidenceInput {
  readonly recorder: TerminalIncidentRecorder;
  readonly captureId: string;
  readonly reason: TerminalCaptureReason;
  readonly currentState: TerminalBrowserPaintedState | null;
  readonly replica: unknown;
  readonly stream: TerminalCaptureStreamIdentity | null;
  readonly geometry: TerminalGeometry | null;
}

/** Identity of this document's recorder, minted once per page load. */
const PROCESS_ID = crypto.randomUUID();

/** Read the live painted state and freeze everything this recorder holds.
 *  Synchronous by contract: callers invoke it before any await, so the payload
 *  describes the DOM that triggered the capture, not the DOM a repair left. */
export function freezeRecorderEvidence(
  recorder: TerminalIncidentRecorder,
  reason: TerminalCaptureReason,
): FrozenBrowserEvidence {
  const renderer = recorder.renderer ?? rendererRegistryEntry(recorder.sessionId)?.renderer ?? null;
  const projection = renderer?.rendererProjection() ?? null;
  const canonical = canonicalTerminalFrame(recorder.sessionId);
  return freezeBrowserEvidence({
    recorder,
    captureId: crypto.randomUUID(),
    reason,
    currentState: projection ? capturePaintedState(recorder, projection, "current", null) : null,
    replica: terminalStreamDiagnosticSnapshot(recorder.sessionId),
    stream: streamIdentityOfFrame(canonical ?? projection?.canonical ?? null)
      ?? committedStreamIdentity(recorder),
    geometry: canonical ? { cols: canonical.cols, rows: canonical.rows } : null,
  });
}

const APP_VERSION = "VITE_APP_VERSION" in import.meta.env
  ? String(import.meta.env.VITE_APP_VERSION)
  : "";
const BUILD_SHA = "VITE_BUILD_SHA" in import.meta.env
  ? String(import.meta.env.VITE_BUILD_SHA)
  : "";

function freezeBrowserEvidence(input: BrowserEvidenceInput): FrozenBrowserEvidence {
  const recorder = input.recorder;
  const trigger: TerminalCaptureTrigger = recorder.trigger ?? {
    reason: input.reason,
    origin: "browser",
    at_ms: Date.now(),
    stream_id: input.stream?.stream_id ?? null,
    grid_epoch: input.stream?.grid_epoch ?? null,
    seq: input.stream?.seq ?? null,
    detail: null,
    occurrence_count: 1,
  };
  const section: TerminalBrowserSection = {
    layer: "browser",
    captured_at_ms: Date.now(),
    process: {
      layer: "browser",
      process_id: PROCESS_ID,
      git_sha: BUILD_SHA,
      artifact_version: APP_VERSION,
      wasm_identity: null,
      worker_fp: null,
      viewer_id: getTabId(),
      user_agent: typeof navigator === "undefined" ? null : navigator.userAgent,
    },
    stream: input.stream,
    geometry: input.geometry,
    dropped: {
      records: recorder.droppedEvents,
      bytes: 0,
      rows: recorder.droppedRows,
      raw_bytes: 0,
      samples: 0,
    },
    omissions: [],
    events: recorder.events.slice(),
    replica: input.replica,
    trigger_state: recorder.triggerState,
    pre_repair_state: recorder.preRepairState ?? recorder.preState,
    post_repair_state: recorder.postRepairState,
    current_state: input.currentState,
  };
  const payload: TerminalIncidentBrowserPayload = {
    schema: TERMINAL_INCIDENT_SCHEMA,
    layer: "browser",
    capture_id: input.captureId,
    recording_id: recorder.recordingId,
    session_id: recorder.sessionId,
    trigger,
    browser: section,
  };
  const fitted = fitBrowserEvidence(payload);
  return {
    captureId: input.captureId,
    recordingId: recorder.recordingId,
    reason: input.reason,
    frozenAtMs: trigger.at_ms,
    payload,
    wireJson: fitted.json,
    partial: fitted.partial,
  };
}

export interface FittedBrowserEvidence {
  readonly json: string;
  readonly partial: boolean;
}

/** Fit the payload into `browserEvidenceBytes`. Trigger, pre-repair and
 *  current snapshots are retained first; oldest whole replay segments go, then
 *  nonvisible history rows, then whole snapshots. A trimmed export is never
 *  reported as complete. */
export function fitBrowserEvidence(
  payload: TerminalIncidentBrowserPayload,
): FittedBrowserEvidence {
  const limit = TERMINAL_CAPTURE_LIMITS.browserEvidenceBytes;
  let json = JSON.stringify(payload);
  if (utf8ByteLength(json) <= limit) return { json, partial: false };

  const omissions: TerminalCaptureOmission[] = [...payload.browser.omissions];
  let section = payload.browser;

  if (section.events.length > 0) {
    omissions.push(sectionOmission("events", section.events.length));
    section = { ...section, events: [] };
    json = JSON.stringify(withSection(payload, section, omissions));
    if (utf8ByteLength(json) <= limit) return { json, partial: true };
  }

  const trimmed = trimStateHistory(section, omissions);
  if (trimmed !== section) {
    section = trimmed;
    json = JSON.stringify(withSection(payload, section, omissions));
    if (utf8ByteLength(json) <= limit) return { json, partial: true };
  }

  for (const drop of ["post_repair_state", "current_state", "pre_repair_state"] as const) {
    if (section[drop] === null) continue;
    omissions.push(sectionOmission(drop, 1));
    section = { ...section, [drop]: null };
    json = JSON.stringify(withSection(payload, section, omissions));
    if (utf8ByteLength(json) <= limit) return { json, partial: true };
  }

  // The triggering snapshot alone does not fit: send metadata only and keep the
  // whole payload locally for download rather than silently dropping evidence.
  if (section.trigger_state !== null) omissions.push(sectionOmission("trigger_state", 1));
  section = { ...section, trigger_state: null, replica: null };
  return {
    json: JSON.stringify(withSection(payload, section, omissions)),
    partial: true,
  };
}

/** Local JSON export of the untrimmed frozen payload. The browser never keeps
 *  the file; the operator downloads it and the recorder can release it. */
export function downloadFrozenBrowserEvidence(evidence: FrozenBrowserEvidence): void {
  if (typeof document === "undefined" || typeof URL.createObjectURL !== "function") return;
  const blob = new Blob([JSON.stringify(evidence.payload)], { type: "application/json" });
  const href = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = href;
  anchor.download = `terminal-incident-browser-${evidence.captureId}.json`;
  // A detached anchor and a synchronous revoke cancel the download in
  // Firefox and Safari; downloadWorkerFile.ts uses this exact sequence.
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(href), 1000);
}

function withSection(
  payload: TerminalIncidentBrowserPayload,
  section: TerminalBrowserSection,
  omissions: readonly TerminalCaptureOmission[],
): TerminalIncidentBrowserPayload {
  return { ...payload, browser: { ...section, omissions } };
}

/** Drop the nonvisible history tail from every retained snapshot. Viewport
 *  rows are what the operator saw, so they are never dropped here. */
function trimStateHistory(
  section: TerminalBrowserSection,
  omissions: TerminalCaptureOmission[],
): TerminalBrowserSection {
  let changed = false;
  const trim = (state: TerminalBrowserPaintedState | null): TerminalBrowserPaintedState | null => {
    if (!state || (state.dom_history.length === 0 && state.painted_model_history.length === 0)) {
      return state;
    }
    changed = true;
    omissions.push(historyOmission(state.phase, state.dom_history, state.painted_model_history));
    return { ...state, dom_history: [], painted_model_history: [] };
  };
  const next: TerminalBrowserSection = {
    ...section,
    trigger_state: trim(section.trigger_state),
    pre_repair_state: trim(section.pre_repair_state),
    post_repair_state: trim(section.post_repair_state),
    current_state: trim(section.current_state),
  };
  return changed ? next : section;
}

function historyOmission(
  phase: string,
  domRows: readonly TerminalDomRow[],
  modelRows: readonly CellRow[],
): TerminalCaptureOmission {
  const first = domRows[0]?.index ?? modelRows[0]?.index ?? null;
  const last = domRows[domRows.length - 1]?.index
    ?? modelRows[modelRows.length - 1]?.index
    ?? null;
  return {
    kind: "rows",
    name: `${phase}.history`,
    reason: "evidence_trimmed",
    dropped_count: domRows.length + modelRows.length,
    dropped_bytes: 0,
    range: first === null || last === null
      ? null
      : { start: String(first), end: String(last + 1) },
  };
}

function sectionOmission(name: string, dropped: number): TerminalCaptureOmission {
  return {
    kind: name === "events" ? "records" : "section",
    name,
    reason: "evidence_trimmed",
    dropped_count: dropped,
    dropped_bytes: 0,
    range: null,
  };
}
