// Envelope validation for a terminal incident bundle, before anything replays
// it. Read by scripts/replay-terminal-incident.ts and by the worker's bundle
// writer as a write-side gate.
// Field primitives live in terminal-capture-fields.ts (which records the
// measurement behind not using zod for this whole-bundle walk), per-layer
// sections in terminal-capture-validate-layers.ts, bounds in
// terminal-capture.ts.
// A rejection is a fixed code plus a field PATH, never a message quoting the
// value: the values validated here are terminal content.

import {
  TERMINAL_INCIDENT_SCHEMA,
  type TerminalCaptureLayer,
  type TerminalIncidentBundle,
} from "./terminal-capture-bundle.ts";
import { isTerminalCaptureReason, type TerminalCaptureErrorCode } from "./terminal-capture.ts";
import { isTerminalUuid } from "./viewport.ts";
import {
  asRecord,
  bundleFieldError,
  isCount,
  isDecimalUint64,
  isEpochMs,
  isNullableString,
  validateStreamIdentity,
  type TerminalBundleValidation,
} from "./terminal-capture-fields.ts";
import {
  validateBrowserSection,
  validateCoordinatorSection,
  validateWorkerSection,
} from "./terminal-capture-validate-layers.ts";

export { isDecimalUint64 } from "./terminal-capture-fields.ts";

export type TerminalIncidentBundleValidation =
  | { readonly ok: true; readonly bundle: TerminalIncidentBundle }
  | {
      readonly ok: false;
      readonly code: TerminalCaptureErrorCode;
      readonly field: string;
    };

const COVERAGE_VALUES = new Set(["complete", "partial", "unavailable"]);
const LAYERS: readonly TerminalCaptureLayer[] = ["browser", "coordinator", "worker"];
const COVERAGE_FIELDS = ["cell_replay", "core_replay", "core_comparison"] as const;

export function validateTerminalIncidentBundle(
  value: unknown,
): TerminalIncidentBundleValidation {
  const root = asRecord(value);
  if (!root) return fail("evidence_malformed", "$");
  if (root.schema !== TERMINAL_INCIDENT_SCHEMA) return fail("evidence_malformed", "schema");
  for (const field of ["capture_id", "recording_id", "session_id"] as const) {
    if (typeof root[field] !== "string" || !isTerminalUuid(root[field] as string)) {
      return fail("invalid_argument", field);
    }
  }
  if (!isEpochMs(root.written_at_ms)) return fail("invalid_argument", "written_at_ms");

  const envelopeError = validateTrigger(root.trigger) ?? validateCoverage(root.coverage);
  if (envelopeError) return promote(envelopeError);

  for (const layer of LAYERS) {
    const raw = root[layer];
    if (raw === null || raw === undefined) continue;
    const section = asRecord(raw);
    if (!section) return fail("evidence_malformed", layer);
    const headerError = validateLayerHeader(section, layer);
    if (headerError) return promote(headerError);
    const sectionError = layer === "worker"
      ? validateWorkerSection(section)
      : layer === "coordinator"
        ? validateCoordinatorSection(section)
        : validateBrowserSection(section);
    if (sectionError) return promote(sectionError);
  }

  return { ok: true, bundle: value as TerminalIncidentBundle };
}

function validateTrigger(value: unknown): TerminalBundleValidation | null {
  const trigger = asRecord(value);
  if (!trigger) return bundleFieldError("evidence_malformed", "trigger");
  if (typeof trigger.reason !== "string" || !isTerminalCaptureReason(trigger.reason)) {
    return bundleFieldError("invalid_argument", "trigger.reason");
  }
  if (!LAYERS.includes(trigger.origin as TerminalCaptureLayer)) {
    return bundleFieldError("invalid_argument", "trigger.origin");
  }
  if (!isEpochMs(trigger.at_ms)) return bundleFieldError("invalid_argument", "trigger.at_ms");
  if (!isNullableString(trigger.stream_id)) {
    return bundleFieldError("invalid_argument", "trigger.stream_id");
  }
  if (!isNullableString(trigger.grid_epoch)) {
    return bundleFieldError("invalid_argument", "trigger.grid_epoch");
  }
  if (trigger.seq !== null && !isDecimalUint64(trigger.seq)) {
    return bundleFieldError("invalid_argument", "trigger.seq");
  }
  if (!isNullableString(trigger.detail)) {
    return bundleFieldError("invalid_argument", "trigger.detail");
  }
  if (!isCount(trigger.occurrence_count)) {
    return bundleFieldError("invalid_argument", "trigger.occurrence_count");
  }
  return null;
}

/** A coverage axis with no reason is a claim with no basis, so an empty reason
 *  list is rejected rather than read as "complete". */
function validateCoverage(value: unknown): TerminalBundleValidation | null {
  const coverage = asRecord(value);
  if (!coverage) return bundleFieldError("evidence_malformed", "coverage");
  for (const field of COVERAGE_FIELDS) {
    if (!COVERAGE_VALUES.has(coverage[field] as string)) {
      return bundleFieldError("invalid_argument", `coverage.${field}`);
    }
    const reasons = coverage[`${field}_reasons`];
    if (!Array.isArray(reasons) || reasons.some((entry) => typeof entry !== "string")) {
      return bundleFieldError("invalid_argument", `coverage.${field}_reasons`);
    }
    if (reasons.length === 0) {
      return bundleFieldError("invalid_argument", `coverage.${field}_reasons`);
    }
  }
  return null;
}

function validateLayerHeader(
  header: Record<string, unknown>,
  layer: TerminalCaptureLayer,
): TerminalBundleValidation | null {
  if (header.layer !== layer) return bundleFieldError("invalid_argument", `${layer}.layer`);
  if (!isEpochMs(header.captured_at_ms)) {
    return bundleFieldError("invalid_argument", `${layer}.captured_at_ms`);
  }
  const process = asRecord(header.process);
  if (!process || process.layer !== layer) {
    return bundleFieldError("evidence_malformed", `${layer}.process`);
  }
  for (const field of ["process_id", "git_sha", "artifact_version"] as const) {
    if (typeof process[field] !== "string") {
      return bundleFieldError("invalid_argument", `${layer}.process.${field}`);
    }
  }
  if (header.stream !== null && header.stream !== undefined) {
    const streamError = validateStreamIdentity(header.stream, `${layer}.stream`);
    if (streamError) return streamError;
  }
  const dropped = asRecord(header.dropped);
  if (!dropped) return bundleFieldError("evidence_malformed", `${layer}.dropped`);
  for (const field of ["records", "bytes", "rows", "raw_bytes", "samples"] as const) {
    if (!isCount(dropped[field])) {
      return bundleFieldError("invalid_argument", `${layer}.dropped.${field}`);
    }
  }
  if (!Array.isArray(header.omissions)) {
    return bundleFieldError("invalid_argument", `${layer}.omissions`);
  }
  return null;
}

function promote(error: TerminalBundleValidation): TerminalIncidentBundleValidation {
  return error.ok
    ? fail("internal", "$")
    : { ok: false, code: error.code, field: error.field };
}

function fail(
  code: TerminalCaptureErrorCode,
  field: string,
): TerminalIncidentBundleValidation {
  return { ok: false, code, field };
}
