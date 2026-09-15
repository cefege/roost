// Per-layer section validation for a terminal incident bundle: worker,
// coordinator and browser. Called only by terminal-capture-validate.ts after
// the envelope and the layer header passed.
// Field primitives come from terminal-capture-fields.ts (which records why this
// walk is not zod); bounds from terminal-capture.ts. Nothing here reads
// terminal text — it proves shape, bounds and per-segment sequence continuity
// so a replay conclusion can rest on an orderable record set.

import { TERMINAL_CAPTURE_LIMITS } from "./terminal-capture.ts";
import {
  asRecord,
  bundleFieldError,
  isDecimalUint64,
  isDimension,
  isEpochMs,
  validateFrame,
  validateRow,
  validateStreamIdentity,
  type TerminalBundleValidation,
} from "./terminal-capture-fields.ts";

const WORKER_BOUNDED_ARRAYS = [
  "emissions", "core_samples", "resizes", "raw", "history_ranges",
] as const;

const BROWSER_STATES = [
  "trigger_state", "pre_repair_state", "post_repair_state", "current_state",
] as const;

export function validateWorkerSection(
  section: Record<string, unknown>,
): TerminalBundleValidation | null {
  const segments = section.segments;
  if (!Array.isArray(segments)) return bundleFieldError("invalid_argument", "worker.segments");
  const segmentIds = new Set<string>();
  for (const [idx, raw] of segments.entries()) {
    const segment = asRecord(raw);
    if (!segment || typeof segment.segment_id !== "string") {
      return bundleFieldError("evidence_malformed", `worker.segments[${idx}]`);
    }
    if (!isDecimalUint64(segment.open_offset)) {
      return bundleFieldError("invalid_argument", `worker.segments[${idx}].open_offset`);
    }
    if (!isEpochMs(segment.opened_at_ms)) {
      return bundleFieldError("invalid_argument", `worker.segments[${idx}].opened_at_ms`);
    }
    segmentIds.add(segment.segment_id);
  }

  for (const name of WORKER_BOUNDED_ARRAYS) {
    const entries = section[name];
    if (!Array.isArray(entries)) return bundleFieldError("invalid_argument", `worker.${name}`);
    if (entries.length > TERMINAL_CAPTURE_LIMITS.layerEntries) {
      return bundleFieldError("resource_exhausted", `worker.${name}`);
    }
  }

  const emissionsError = validateWorkerEmissions(
    section.emissions as unknown[],
    segmentIds,
  );
  if (emissionsError) return emissionsError;

  for (const [idx, raw] of (section.core_samples as unknown[]).entries()) {
    const record = asRecord(raw);
    if (!record) return bundleFieldError("evidence_malformed", `worker.core_samples[${idx}]`);
    const streamError = validateStreamIdentity(
      record.stream,
      `worker.core_samples[${idx}].stream`,
    );
    if (streamError) return streamError;
    for (const name of ["core_frame", "fold_frame"] as const) {
      const frameError = validateFrame(record[name], `worker.core_samples[${idx}].${name}`);
      if (frameError) return frameError;
    }
  }

  for (const [idx, raw] of (section.raw as unknown[]).entries()) {
    const record = asRecord(raw);
    if (!record) return bundleFieldError("evidence_malformed", `worker.raw[${idx}]`);
    if (!isDecimalUint64(record.start_offset)) {
      return bundleFieldError("invalid_argument", `worker.raw[${idx}].start_offset`);
    }
    if (!isDecimalUint64(record.end_offset)) {
      return bundleFieldError("invalid_argument", `worker.raw[${idx}].end_offset`);
    }
    // Cross-field: neither offset is individually wrong, so the pair is named.
    if (BigInt(record.end_offset as string) < BigInt(record.start_offset as string)) {
      return bundleFieldError("invalid_argument", `worker.raw[${idx}].offsets`);
    }
    if (typeof record.base64 !== "string") {
      return bundleFieldError("invalid_argument", `worker.raw[${idx}].base64`);
    }
  }

  for (const [idx, raw] of (section.resizes as unknown[]).entries()) {
    const record = asRecord(raw);
    if (!record) return bundleFieldError("evidence_malformed", `worker.resizes[${idx}]`);
    if (!isDecimalUint64(record.install_offset)) {
      return bundleFieldError("invalid_argument", `worker.resizes[${idx}].install_offset`);
    }
    if (record.boundary_offset !== null && !isDecimalUint64(record.boundary_offset)) {
      return bundleFieldError("invalid_argument", `worker.resizes[${idx}].boundary_offset`);
    }
    for (const side of ["from", "to"] as const) {
      const geometry = asRecord(record[side]);
      if (!geometry || !isDimension(geometry.cols) || !isDimension(geometry.rows)) {
        return bundleFieldError("invalid_argument", `worker.resizes[${idx}].${side}`);
      }
    }
  }

  for (const name of ["core_scrollback_tail", "history_rows"] as const) {
    const rows = section[name];
    if (!Array.isArray(rows)) return bundleFieldError("invalid_argument", `worker.${name}`);
    for (const [idx, raw] of rows.entries()) {
      const rowError = validateRow(raw, `worker.${name}[${idx}]`);
      if (rowError) return rowError;
    }
  }
  return null;
}

/** A later emission in the SAME segment may never carry a lower sequence: that
 *  would make the retained fold unorderable, and any attribution drawn from it
 *  unfounded. Across segments the numbering restarts by design. */
function validateWorkerEmissions(
  emissions: unknown[],
  segmentIds: ReadonlySet<string>,
): TerminalBundleValidation | null {
  const lastSeqBySegment = new Map<string, bigint>();
  for (const [idx, raw] of emissions.entries()) {
    const record = asRecord(raw);
    if (!record) return bundleFieldError("evidence_malformed", `worker.emissions[${idx}]`);
    const segmentId = record.segment_id;
    if (typeof segmentId !== "string" || !segmentIds.has(segmentId)) {
      return bundleFieldError("invalid_argument", `worker.emissions[${idx}].segment_id`);
    }
    const streamError = validateStreamIdentity(
      record.stream,
      `worker.emissions[${idx}].stream`,
    );
    if (streamError) return streamError;
    const frameError = validateFrame(record.frame, `worker.emissions[${idx}].frame`);
    if (frameError) return frameError;
    const seq = BigInt((record.stream as Record<string, unknown>).seq as string);
    const previous = lastSeqBySegment.get(segmentId);
    if (previous !== undefined && seq < previous) {
      return bundleFieldError("invalid_argument", `worker.emissions[${idx}].stream.seq`);
    }
    lastSeqBySegment.set(segmentId, seq);
  }
  return null;
}

export function validateCoordinatorSection(
  section: Record<string, unknown>,
): TerminalBundleValidation | null {
  const records = section.records;
  if (!Array.isArray(records)) return bundleFieldError("invalid_argument", "coordinator.records");
  if (records.length > TERMINAL_CAPTURE_LIMITS.layerEntries) {
    return bundleFieldError("resource_exhausted", "coordinator.records");
  }
  for (const [idx, raw] of records.entries()) {
    const record = asRecord(raw);
    if (!record) return bundleFieldError("evidence_malformed", `coordinator.records[${idx}]`);
    const streamError = validateStreamIdentity(
      record.stream,
      `coordinator.records[${idx}].stream`,
    );
    if (streamError) return streamError;
    if (record.canonical !== null) {
      const frameError = validateFrame(
        record.canonical,
        `coordinator.records[${idx}].canonical`,
      );
      if (frameError) return frameError;
    }
  }
  return null;
}

export function validateBrowserSection(
  section: Record<string, unknown>,
): TerminalBundleValidation | null {
  if (!Array.isArray(section.events)) {
    return bundleFieldError("invalid_argument", "browser.events");
  }
  if (section.events.length > TERMINAL_CAPTURE_LIMITS.layerEntries) {
    return bundleFieldError("resource_exhausted", "browser.events");
  }
  for (const name of BROWSER_STATES) {
    const value = section[name];
    if (value === null || value === undefined) continue;
    const state = asRecord(value);
    if (!state) return bundleFieldError("evidence_malformed", `browser.${name}`);
    if (!isEpochMs(state.at_ms)) {
      return bundleFieldError("invalid_argument", `browser.${name}.at_ms`);
    }
    if (state.canonical !== null) {
      const frameError = validateFrame(state.canonical, `browser.${name}.canonical`);
      if (frameError) return frameError;
    }
    for (const rows of ["dom_history", "dom_viewport"] as const) {
      const entries = state[rows];
      if (!Array.isArray(entries)) {
        return bundleFieldError("invalid_argument", `browser.${name}.${rows}`);
      }
      if (entries.length > TERMINAL_CAPTURE_LIMITS.browserRowsMax) {
        return bundleFieldError("resource_exhausted", `browser.${name}.${rows}`);
      }
    }
    const modelRows = state.painted_model_history;
    if (!Array.isArray(modelRows)) {
      return bundleFieldError("invalid_argument", `browser.${name}.painted_model_history`);
    }
    for (const [idx, raw] of modelRows.entries()) {
      const rowError = validateRow(raw, `browser.${name}.painted_model_history[${idx}]`);
      if (rowError) return rowError;
    }
  }
  return null;
}
