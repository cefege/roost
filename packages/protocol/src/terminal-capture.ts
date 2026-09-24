// Contract for opt-in terminal incident capture: identifiers, the closed
// literal sets, every bound the three recorders enforce, and the validation the
// coordinator runs before it allocates a lease or dispatches to a worker.
// Consumed by apps/coord (capture bridge), apps/worker (recorder + storage),
// apps/web (recorder + menu) and scripts/replay-terminal-incident.ts.
// Bundle JSON shapes live in terminal-capture-bundle.ts; exact state
// comparison lives in terminal-capture-view.ts. Both are re-exported here so
// "@roost/protocol/terminal-capture" is the single import for every layer.

import { TerminalCaptureAction } from "./gen/roost/v1/coordinator_pb.ts";
import { hasAtMostUtf8Bytes, utf8ByteLength } from "./ui-state.ts";
import { isTerminalUuid } from "./viewport.ts";
import {
  TERMINAL_INCIDENT_SCHEMA,
  type TerminalBrowserSection,
  type TerminalCaptureLayer,
  type TerminalCaptureReason,
  type TerminalCaptureTrigger,
  type TerminalCoordinatorSection,
} from "./terminal-capture-bundle.ts";

export * from "./terminal-capture-bundle.ts";
export * from "./terminal-capture-view.ts";

export type TerminalCaptureActionName = "start" | "capture" | "stop";

export type TerminalCaptureStatus =
  | "recording"
  | "captured"
  | "stopped"
  | "partial"
  | "error";

/** Fixed failure vocabulary. A capture failure never returns a validation or
 *  parser message, because those can quote the terminal text being validated. */
export type TerminalCaptureErrorCode =
  | "invalid_argument"
  | "permission_denied"
  | "session_unknown"
  | "worker_offline"
  | "worker_timeout"
  | "worker_failed"
  | "lease_conflict"
  | "lease_expired"
  | "lease_absent"
  | "resource_exhausted"
  | "evidence_too_large"
  | "evidence_malformed"
  | "capture_in_flight"
  | "rate_limited"
  | "capture_expired"
  | "storage_failed"
  | "internal";

export const TERMINAL_CAPTURE_REASONS: readonly TerminalCaptureReason[] = [
  "manual",
  "history_identity",
  "viewport_model",
  "worker_emission",
  "pre_repair",
];

export function isTerminalCaptureReason(value: string): value is TerminalCaptureReason {
  return (TERMINAL_CAPTURE_REASONS as readonly string[]).includes(value);
}

/** Every bound the recorders, the bridge and the storage writer enforce. One
 *  table so a limit cannot drift between the layer that produces evidence and
 *  the layer that refuses it. */
export const TERMINAL_CAPTURE_LIMITS = {
  /** Server-time lease window for one recording. */
  leaseMs: 30 * 60_000,
  /** Idempotent START renewal cadence while the debugging pane is visible. */
  renewIntervalMs: 5 * 60_000,
  maxRecordingsPerProcess: 2,
  maxRecordingsPerDocument: 2,
  /** Per session per layer, across all retained records. */
  layerBytes: 8 * 1024 * 1024,
  layerEntries: 128,
  rawBytes: 1024 * 1024,
  cellBytes: 6 * 1024 * 1024,
  metadataBytes: 1024 * 1024,
  browserEvidenceBytes: 512 * 1024,
  coordinatorEvidenceBytes: 512 * 1024,
  bundleBytes: 32 * 1024 * 1024,
  /** Dedicated capture deadline; the ordinary diag snapshot keeps its 2s. */
  captureDeadlineMs: 10_000,
  completedCaptureIds: 128,
  retentionMs: 24 * 60 * 60_000,
  /** Automatic captures per session, regardless of a new epoch. */
  automaticCooldownMs: 60_000,
  manualCooldownMs: 10_000,
  coreSampleIntervalMs: 250,
  coreSampleMaxCells: 16_384,
  coreSampleBudgetUs: 2_000,
  coreSampleSuppressMs: 1_000,
  coreScrollbackTailRows: 128,
  captureHistoryRows: 256,
  browserHistoryTailRows: 128,
  browserRowsMax: 512,
  /** Combined bytecap + incident retention on one worker. */
  storageFiles: 50,
  storageBytes: 500 * 1024 * 1024,
} as const;

export interface TerminalCaptureCommand {
  readonly action: TerminalCaptureActionName;
  readonly session_id: string;
  readonly recording_id: string;
  readonly capture_id: string;
  readonly reason: TerminalCaptureReason;
  /** Frozen browser evidence; empty for START/STOP. */
  readonly browser_evidence_json: string;
}

export interface TerminalCaptureFileRef {
  readonly capture_id: string;
  readonly path: string;
  readonly byte_length: number;
  readonly status: TerminalCaptureStatus;
}

/** `terminal_capture` member of DiagSnapshotResponse.snapshot_json. IDs,
 *  reasons, counts, bounds and status only — never terminal content. */
export interface TerminalCaptureResult {
  readonly capture_id: string;
  readonly recording_id: string;
  readonly session_id: string;
  readonly action: TerminalCaptureActionName;
  readonly status: TerminalCaptureStatus;
  readonly expires_at_ms: number | null;
  readonly worker_fp: string | null;
  readonly path: string | null;
  readonly byte_length: number | null;
  readonly error: TerminalCaptureErrorCode | null;
  /** Last worker-local frozen incident, so a worker-triggered capture is
   *  downloadable even though the browser never asked for it. */
  readonly recent_worker_capture: TerminalCaptureFileRef | null;
}

export type TerminalCaptureValidation =
  | { readonly ok: true; readonly command: TerminalCaptureCommand }
  | {
      readonly ok: false;
      readonly code: TerminalCaptureErrorCode;
      readonly field: string;
    };

const ACTION_NAMES: Record<TerminalCaptureAction, TerminalCaptureActionName | null> = {
  [TerminalCaptureAction.UNSPECIFIED]: null,
  [TerminalCaptureAction.START]: "start",
  [TerminalCaptureAction.CAPTURE]: "capture",
  [TerminalCaptureAction.STOP]: "stop",
};

export function terminalCaptureActionName(
  action: TerminalCaptureAction,
): TerminalCaptureActionName | null {
  return ACTION_NAMES[action] ?? null;
}

export function terminalCaptureActionValue(
  name: TerminalCaptureActionName,
): TerminalCaptureAction {
  return name === "start"
    ? TerminalCaptureAction.START
    : name === "capture"
      ? TerminalCaptureAction.CAPTURE
      : TerminalCaptureAction.STOP;
}

export interface TerminalCaptureRequestFields {
  readonly action: TerminalCaptureAction;
  readonly sessionId: string;
  readonly captureId: string;
  readonly recordingId: string;
  readonly reason: string;
  readonly browserEvidenceJson: string;
}

/** Validate the wire request before any cache lookup, lease allocation or
 *  worker dispatch. UNSPECIFIED is invalid whenever the message is present, and
 *  START/STOP must carry no evidence so a control call can never smuggle a
 *  payload past the CAPTURE size gate. */
export function validateTerminalCaptureRequest(
  fields: TerminalCaptureRequestFields,
): TerminalCaptureValidation {
  const action = terminalCaptureActionName(fields.action);
  if (action === null) return invalid("invalid_argument", "action");
  if (!isTerminalUuid(fields.sessionId)) return invalid("invalid_argument", "session_id");
  if (!isTerminalUuid(fields.recordingId)) return invalid("invalid_argument", "recording_id");
  if (!isTerminalUuid(fields.captureId)) return invalid("invalid_argument", "capture_id");
  if (!isTerminalCaptureReason(fields.reason)) return invalid("invalid_argument", "reason");
  if (action !== "capture") {
    if (fields.browserEvidenceJson.length !== 0) {
      return invalid("invalid_argument", "browser_evidence_json");
    }
    if (fields.reason !== "manual") return invalid("invalid_argument", "reason");
  } else if (utf8ByteLength(fields.browserEvidenceJson) > TERMINAL_CAPTURE_LIMITS.browserEvidenceBytes) {
    return invalid("evidence_too_large", "browser_evidence_json");
  }
  return {
    ok: true,
    command: {
      action,
      session_id: fields.sessionId,
      recording_id: fields.recordingId,
      capture_id: fields.captureId,
      reason: fields.reason,
      browser_evidence_json: fields.browserEvidenceJson,
    },
  };
}

/** Envelope every layer's evidence carries so a bridge can confirm the payload
 *  belongs to THIS capture without inspecting terminal content.
 *
 *  The layer's own section is NESTED under a member named for that layer, and
 *  never flattened onto the envelope: the persisted bundle already carries the
 *  identity at top level, so spreading it into every section would duplicate
 *  the source of truth — and a flattened payload validates as an envelope
 *  while silently failing as a section, which drops a whole layer's evidence
 *  with no error anyone reads. `checkTerminalCaptureEnvelope` therefore proves
 *  the section member exists before a bridge forwards anything. */
export interface TerminalCaptureEvidenceEnvelope {
  readonly schema: typeof TERMINAL_INCIDENT_SCHEMA;
  readonly layer: TerminalCaptureLayer;
  readonly capture_id: string;
  readonly recording_id: string;
  readonly session_id: string;
}

export interface TerminalCaptureBrowserPayload extends TerminalCaptureEvidenceEnvelope {
  readonly layer: "browser";
  /** The browser is usually the trigger origin, so it ships the trigger. */
  readonly trigger: TerminalCaptureTrigger;
  readonly browser: TerminalBrowserSection;
}

export interface TerminalCaptureCoordinatorPayload extends TerminalCaptureEvidenceEnvelope {
  readonly layer: "coordinator";
  readonly coordinator: TerminalCoordinatorSection;
}

export type TerminalCaptureEnvelopeCheck =
  | {
      readonly ok: true;
      readonly envelope: TerminalCaptureEvidenceEnvelope;
      /** The nested layer section, already proved to be a plain object. */
      readonly section: Record<string, unknown>;
      /** Present only when the payload carries one. */
      readonly trigger: TerminalCaptureTrigger | null;
    }
  | {
      readonly ok: false;
      readonly code: TerminalCaptureErrorCode;
      readonly field: string;
    };

/** Shallow envelope check on untrusted JSON. Deep row validation is the bundle
 *  writer's and the replay script's job; a bridge only proves the payload is
 *  well-formed, that cross-session evidence cannot be attached, and that the
 *  layer section it claims to carry is actually there.
 *  The size gate short-circuits: an oversized payload is refused after reading
 *  `browserEvidenceBytes + 1` code units rather than walking all of it. */
export function checkTerminalCaptureEnvelope(
  json: string,
  expected: { readonly layer: TerminalCaptureLayer; readonly command: TerminalCaptureCommand },
): TerminalCaptureEnvelopeCheck {
  if (!hasAtMostUtf8Bytes(json, TERMINAL_CAPTURE_LIMITS.browserEvidenceBytes)) {
    return { ok: false, code: "evidence_too_large", field: "browser_evidence_json" };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return { ok: false, code: "evidence_malformed", field: "browser_evidence_json" };
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { ok: false, code: "evidence_malformed", field: "browser_evidence_json" };
  }
  const record = parsed as Record<string, unknown>;
  if (record.schema !== TERMINAL_INCIDENT_SCHEMA) {
    return { ok: false, code: "evidence_malformed", field: "schema" };
  }
  if (record.layer !== expected.layer) {
    return { ok: false, code: "evidence_malformed", field: "layer" };
  }
  if (record.capture_id !== expected.command.capture_id) {
    return { ok: false, code: "permission_denied", field: "capture_id" };
  }
  if (record.recording_id !== expected.command.recording_id) {
    return { ok: false, code: "permission_denied", field: "recording_id" };
  }
  if (record.session_id !== expected.command.session_id) {
    return { ok: false, code: "permission_denied", field: "session_id" };
  }
  const nested = record[expected.layer];
  if (nested === null || typeof nested !== "object" || Array.isArray(nested)) {
    return { ok: false, code: "evidence_malformed", field: expected.layer };
  }
  const trigger = record.trigger;
  return {
    ok: true,
    envelope: {
      schema: TERMINAL_INCIDENT_SCHEMA,
      layer: expected.layer,
      capture_id: expected.command.capture_id,
      recording_id: expected.command.recording_id,
      session_id: expected.command.session_id,
    },
    section: nested as Record<string, unknown>,
    trigger: trigger !== null && typeof trigger === "object" && !Array.isArray(trigger)
      ? trigger as unknown as TerminalCaptureTrigger
      : null,
  };
}

export { hasAtMostUtf8Bytes, utf8ByteLength } from "./ui-state.ts";

/** Filename of one incident bundle. The capture UUID is the whole name: a
 *  client never chooses a path, and the sid is not in the filename because the
 *  bundle already carries it under 0600. */
export function terminalCaptureFileName(captureId: string): string {
  return `terminal-incident-${captureId}.json.gz`;
}

export const TERMINAL_CAPTURE_FILE_PREFIX = "terminal-incident-";
export const TERMINAL_CAPTURE_FILE_SUFFIX = ".json.gz";

/** True for a name this storage owner created. File deletion is restricted to
 *  recognized capture files so retention can never unlink a neighbour's log. */
export function isTerminalCaptureFileName(name: string): boolean {
  if (
    !name.startsWith(TERMINAL_CAPTURE_FILE_PREFIX)
    || !name.endsWith(TERMINAL_CAPTURE_FILE_SUFFIX)
  ) return false;
  const captureId = name.slice(
    TERMINAL_CAPTURE_FILE_PREFIX.length,
    name.length - TERMINAL_CAPTURE_FILE_SUFFIX.length,
  );
  return isTerminalUuid(captureId);
}

function invalid(
  code: TerminalCaptureErrorCode,
  field: string,
): TerminalCaptureValidation {
  return { ok: false, code, field };
}
