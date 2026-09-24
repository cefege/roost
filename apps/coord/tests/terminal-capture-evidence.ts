// Typed layer-evidence fixtures for the terminal-capture suites. Every payload
// is built from the shared TerminalCapture*Payload types, so the flattened
// shape that silently dropped a whole layer in production cannot compile here.
// Identifiers and the marker text come from terminal-capture-harness.ts.
// Used by terminal-capture-bridge.test.ts, terminal-capture-recorder.test.ts
// and diag-snapshot-handlers.test.ts.

import {
  TERMINAL_INCIDENT_SCHEMA,
  type TerminalBrowserPaintedState,
  type TerminalBrowserSection,
  type TerminalCaptureBrowserPayload,
  type TerminalCaptureCoordinatorPayload,
  type TerminalCaptureProcessIdentity,
  type TerminalCaptureTrigger,
} from "@roost/protocol/terminal-capture";
import { EPOCH, STREAM } from "./terminal-screen-hub-harness.ts";
import { CAPTURE_1, EVIDENCE_MARKER, RECORDING_A, SESSION_A } from "./terminal-capture-harness.ts";

export interface EvidenceIdentity {
  captureId: string;
  recordingId: string;
  sessionId: string;
}

/** The browser layer's frozen payload: envelope plus its section NESTED under
 *  the `browser` member. */
export function browserEvidencePayload(
  overrides: Partial<EvidenceIdentity> = {},
): TerminalCaptureBrowserPayload {
  return {
    schema: TERMINAL_INCIDENT_SCHEMA,
    layer: "browser",
    capture_id: overrides.captureId ?? CAPTURE_1,
    recording_id: overrides.recordingId ?? RECORDING_A,
    session_id: overrides.sessionId ?? SESSION_A,
    trigger: browserTrigger(),
    browser: browserSection(),
  };
}

export function browserEvidenceJson(overrides: Partial<EvidenceIdentity> = {}): string {
  return JSON.stringify(browserEvidencePayload(overrides));
}

/** A payload whose nested layer section is gone. This is the exact production
 *  defect: it still names the capture, so an identity-only check admits it and
 *  the bundle writer then has no section to validate. */
export function withoutLayerSection(
  payload: TerminalCaptureBrowserPayload | TerminalCaptureCoordinatorPayload,
): Record<string, unknown> {
  const corrupted: Record<string, unknown> = { ...payload };
  delete corrupted[payload.layer];
  return corrupted;
}

export function browserTrigger(): TerminalCaptureTrigger {
  return {
    reason: "history_identity",
    origin: "browser",
    at_ms: 1,
    stream_id: STREAM,
    grid_epoch: EPOCH,
    seq: "2",
    detail: "duplicate_absolute_index",
    occurrence_count: 1,
  };
}

function browserProcessIdentity(): TerminalCaptureProcessIdentity {
  return {
    layer: "browser",
    process_id: "94000000-0000-4000-8000-000000000001",
    git_sha: "test",
    artifact_version: "test",
    wasm_identity: null,
    worker_fp: null,
    viewer_id: "viewer-a",
    user_agent: "test-agent",
  };
}

function paintedState(): TerminalBrowserPaintedState {
  return {
    at_ms: 1,
    phase: "current",
    apply_mode: "delta",
    canonical: null,
    committed: null,
    pending: null,
    painted_model_history: [],
    dom_history: [],
    // The marker stands in for terminal text: a bundle carries it, a response
    // and a log line never may.
    dom_viewport: [{
      order: 0,
      index: null,
      columns: EVIDENCE_MARKER.length,
      fingerprint: 7,
      text: EVIDENCE_MARKER,
      span_count: 1,
    }],
    gaps: [],
    cursor: { row: 0, col: 0, visible: true },
    scroll: { top: 0, height: 32, client_height: 32, row_height: 16 },
    reader: null,
    active: true,
    visible: true,
    omissions: [],
  };
}

function browserSection(): TerminalBrowserSection {
  return {
    layer: "browser",
    captured_at_ms: 1,
    process: browserProcessIdentity(),
    stream: null,
    geometry: null,
    dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
    omissions: [],
    events: [],
    replica: null,
    trigger_state: paintedState(),
    pre_repair_state: null,
    post_repair_state: null,
    current_state: paintedState(),
  };
}
