// Field-level checks shared by every terminal incident bundle validator.
// Called by terminal-capture-validate.ts (envelope) and
// terminal-capture-validate-layers.ts (per-layer sections).
// Every rejection is a fixed code plus a field PATH — never a message quoting
// the value, because the values here are terminal content.
//
// These are deliberately NOT zod, unlike every other wire schema in this
// package. Measured on one 5.79 MiB bundle: 8ms here versus 161ms for an
// equivalent zod schema set, against 6.7ms to gzip the same bytes. The worker
// runs this as a write-side gate while hosting up to 332 terminal cores on a
// single-threaded runtime, so at the admitted per-layer ceiling the zod version
// is most of a second of event-loop stall — every PTY on the machine freezing
// precisely when an operator is debugging one of them. The bounded 512 KiB
// request-path envelope check IS zod (terminal-capture.ts); only this
// whole-bundle walk is hand-rolled, and only for that reason.

import type { CellGridFrame, CellRow, CellSpan } from "./cell/types.ts";
import type { TerminalCaptureErrorCode } from "./terminal-capture.ts";

export type TerminalBundleValidation =
  | { readonly ok: true; readonly bundle: unknown }
  | {
      readonly ok: false;
      readonly code: TerminalCaptureErrorCode;
      readonly field: string;
    };

const UINT64_RE = /^(0|[1-9][0-9]{0,19})$/;
const UINT64_MAX = 18_446_744_073_709_551_615n;
const MAX_DIMENSION = 4096;

/** True for a decimal uint64 string — the only exact JSON representation of a
 *  raw byte offset or a stream sequence. A JSON number silently rounds past
 *  2^53, and a byte offset IS the parse boundary a replay depends on. */
export function isDecimalUint64(value: unknown): value is string {
  return typeof value === "string"
    && UINT64_RE.test(value)
    && BigInt(value) <= UINT64_MAX;
}

export function bundleFieldError(
  code: TerminalCaptureErrorCode,
  field: string,
): TerminalBundleValidation {
  return { ok: false, code, field };
}

export function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

export function isCount(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

export function isEpochMs(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) > 0;
}

export function isDimension(value: unknown): value is number {
  return Number.isSafeInteger(value)
    && (value as number) >= 1
    && (value as number) <= MAX_DIMENSION;
}

export function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

export function validateStreamIdentity(
  value: unknown,
  path: string,
): TerminalBundleValidation | null {
  const stream = asRecord(value);
  if (!stream) return bundleFieldError("evidence_malformed", path);
  if (typeof stream.stream_id !== "string" || stream.stream_id.length === 0) {
    return bundleFieldError("invalid_argument", `${path}.stream_id`);
  }
  if (typeof stream.grid_epoch !== "string" || stream.grid_epoch.length === 0) {
    return bundleFieldError("invalid_argument", `${path}.grid_epoch`);
  }
  if (!isDecimalUint64(stream.seq)) {
    return bundleFieldError("invalid_argument", `${path}.seq`);
  }
  if (stream.base_seq !== null && !isDecimalUint64(stream.base_seq)) {
    return bundleFieldError("invalid_argument", `${path}.base_seq`);
  }
  if (!isDimension(stream.cols) || !isDimension(stream.rows)) {
    return bundleFieldError("invalid_argument", `${path}.geometry`);
  }
  return null;
}

/** Structural check of a wire frame as it survived JSON: dimensions in range,
 *  dense index-ordered viewport for a full, and every span claiming at least
 *  one column. Column occupancy is what a canonical comparison walks, so an
 *  unchecked `columns` would shift a whole row's comparison silently. */
export function validateFrame(
  value: unknown,
  path: string,
): TerminalBundleValidation | null {
  const frame = asRecord(value);
  if (!frame) return bundleFieldError("evidence_malformed", path);
  if (!isDimension(frame.cols) || !isDimension(frame.rows)) {
    return bundleFieldError("invalid_argument", `${path}.geometry`);
  }
  if (typeof frame.full !== "boolean") {
    return bundleFieldError("invalid_argument", `${path}.full`);
  }
  if (typeof frame.streamId !== "string" || typeof frame.gridEpoch !== "string") {
    return bundleFieldError("invalid_argument", `${path}.identity`);
  }
  if (!isCount(frame.seq) || !isCount(frame.baseSeq)) {
    return bundleFieldError("invalid_argument", `${path}.seq`);
  }
  if (!isCount(frame.scrollbackTotal) || !isCount(frame.sbBase)) {
    return bundleFieldError("invalid_argument", `${path}.scrollback`);
  }
  for (const name of ["viewportRows", "scrollbackRows", "scrollbackAppend"] as const) {
    const rows = frame[name];
    if (!Array.isArray(rows)) return bundleFieldError("invalid_argument", `${path}.${name}`);
    for (const [idx, raw] of rows.entries()) {
      const rowError = validateRow(raw, `${path}.${name}[${idx}]`);
      if (rowError) return rowError;
    }
  }
  if (frame.full === true) {
    const viewportRows = frame.viewportRows as CellRow[];
    if (viewportRows.length !== frame.rows) {
      return bundleFieldError("invalid_argument", `${path}.viewportRows`);
    }
    for (const [idx, row] of viewportRows.entries()) {
      if (row.index !== idx) {
        return bundleFieldError("invalid_argument", `${path}.viewportRows[${idx}].index`);
      }
    }
  }
  return null;
}

export function validateRow(
  value: unknown,
  path: string,
): TerminalBundleValidation | null {
  const row = asRecord(value);
  if (!row) return bundleFieldError("evidence_malformed", path);
  if (!isCount(row.index)) return bundleFieldError("invalid_argument", `${path}.index`);
  const spans = row.spans;
  if (!Array.isArray(spans)) return bundleFieldError("invalid_argument", `${path}.spans`);
  for (const [idx, raw] of spans.entries()) {
    const span = asRecord(raw) as unknown as CellSpan | null;
    if (!span || typeof span.text !== "string") {
      return bundleFieldError("evidence_malformed", `${path}.spans[${idx}]`);
    }
    if (!Number.isSafeInteger(span.columns) || span.columns < 1) {
      return bundleFieldError("invalid_argument", `${path}.spans[${idx}].columns`);
    }
    if (!isCount(span.fg) || !isCount(span.bg) || !isCount(span.flags)) {
      return bundleFieldError("invalid_argument", `${path}.spans[${idx}].style`);
    }
  }
  return null;
}

/** Narrow an already-validated value. A canonical comparison accepts only a
 *  dense full frame, so callers need the same narrowing the validator proved. */
export function asCellGridFrame(value: unknown): CellGridFrame | null {
  return validateFrame(value, "$") === null ? (value as CellGridFrame) : null;
}
