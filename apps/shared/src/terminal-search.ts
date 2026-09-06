// Shared terminal-search limits and JSON validation for the coordinator/worker lane.
// The worker scanner, coordinator relay, and browser find controller import this
// module so paging bounds and Unicode code-point semantics cannot drift.

import { z } from "zod";

export const TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS = 256;
export const TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH = 64;
export const TERMINAL_SEARCH_ID_MAX_LENGTH = 64;
export const TERMINAL_SEARCH_MAX_ROWS = 4_096;
export const TERMINAL_SEARCH_MAX_MATCHES = 256;
export const TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS = 512;
export const TERMINAL_SEARCH_MAX_PAGES = 32;
export const GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS = 32;
export const GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION = 2_048;
export const GLOBAL_TERMINAL_SEARCH_MAX_MATCHES = 256;
export const GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS = 5_000;
export const GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS = 4_500;
export const GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS = 60_000;
export const GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE = 4;
export const TERMINAL_SEARCH_RPC_DEADLINE_MS = 8_000;

const SafeNonnegativeIntegerSchema = z.number()
  .int()
  .nonnegative()
  .max(Number.MAX_SAFE_INTEGER);
const Uint32Schema = z.number().int().nonnegative().max(0xffff_ffff);
const PositiveUint32Schema = z.number().int().positive().max(0xffff_ffff);

/** Count Unicode code points without allocating an intermediate code-point array. */
export function countUnicodeCodePoints(value: string): number {
  let count = 0;
  let offset = 0;
  while (offset < value.length) {
    const codePoint = value.codePointAt(offset)!;
    offset += codePoint > 0xffff ? 2 : 1;
    count++;
  }
  return count;
}

/** Divide a page cap across sessions so no selected session is starved. */
export function allocateGlobalSearchMatchLimits(
  totalMatches: number,
  sessionCount: number,
): number[] {
  if (!Number.isSafeInteger(totalMatches) || totalMatches < 0) {
    throw new RangeError("totalMatches must be a safe nonnegative integer");
  }
  if (!Number.isSafeInteger(sessionCount) || sessionCount <= 0) {
    throw new RangeError("sessionCount must be a positive safe integer");
  }
  const base = Math.floor(totalMatches / sessionCount);
  const remainder = totalMatches % sessionCount;
  return Array.from(
    { length: sessionCount },
    (_, index) => base + (index < remainder ? 1 : 0),
  );
}

function hasAtMostUnicodeCodePoints(value: string, maxCodePoints: number): boolean {
  let count = 0;
  let offset = 0;
  while (offset < value.length) {
    if (count === maxCodePoints) return false;
    const codePoint = value.codePointAt(offset)!;
    offset += codePoint > 0xffff ? 2 : 1;
    count++;
  }
  return true;
}

/** Truncate at a Unicode code-point boundary, preserving the original when it fits. */
export function truncateUnicodeCodePoints(value: string, maxCodePoints: number): string {
  if (!Number.isSafeInteger(maxCodePoints) || maxCodePoints < 0) {
    throw new RangeError("maxCodePoints must be a safe nonnegative integer");
  }

  let count = 0;
  let offset = 0;
  while (offset < value.length && count < maxCodePoints) {
    const codePoint = value.codePointAt(offset)!;
    offset += codePoint > 0xffff ? 2 : 1;
    count++;
  }
  return offset === value.length ? value : value.slice(0, offset);
}

function boundedCodePointString(maxCodePoints: number, field: string) {
  return z.string().refine(
    value => hasAtMostUnicodeCodePoints(value, maxCodePoints),
    `${field} exceeds ${maxCodePoints} Unicode code points`,
  );
}

export const TerminalSearchQuerySchema = boundedCodePointString(
  TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
  "terminal search query",
);
export const TerminalSearchGridEpochSchema = z.string()
  .max(TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH);
export const TerminalSearchIdSchema = z.string()
  .min(1)
  .max(TERMINAL_SEARCH_ID_MAX_LENGTH);
export const TerminalSearchRowSchema = SafeNonnegativeIntegerSchema;
export const TerminalSearchMaxRowsSchema = z.number()
  .int()
  .positive()
  .max(TERMINAL_SEARCH_MAX_ROWS);
export const TerminalSearchMaxMatchesSchema = z.number()
  .int()
  .positive()
  .max(TERMINAL_SEARCH_MAX_MATCHES);

export const SearchStopReasonSchema = z.enum([
  "complete",
  "row_limit",
  "match_limit",
  "deadline",
  "epoch_changed",
]);
export type SearchStopReason = z.infer<typeof SearchStopReasonSchema>;

export const ScrollbackHistoryFloorSchema = z.enum([
  "none",
  "evicted",
  "resize_replay",
]);

export const WorkerSearchScrollbackMatchSchema = z.object({
  row: TerminalSearchRowSchema,
  col: Uint32Schema,
  len: Uint32Schema,
  preview: boundedCodePointString(
    TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS,
    "terminal search preview",
  ),
}).strict().readonly();
export type WorkerSearchScrollbackMatch = z.infer<
  typeof WorkerSearchScrollbackMatchSchema
>;

export const WorkerSearchScrollbackResultSchema = z.object({
  matches: z.array(WorkerSearchScrollbackMatchSchema)
    .max(TERMINAL_SEARCH_MAX_MATCHES)
    .readonly(),
  truncated: z.boolean(),
  total: TerminalSearchRowSchema,
  cols: PositiveUint32Schema,
  grid_epoch: TerminalSearchGridEpochSchema.min(1),
  scanned_start_row: TerminalSearchRowSchema,
  scanned_end_row: TerminalSearchRowSchema,
  history_floor: ScrollbackHistoryFloorSchema,
  next_before_row: TerminalSearchRowSchema.optional(),
  stop_reason: SearchStopReasonSchema,
}).strict().readonly().superRefine((result, context) => {
  if (result.scanned_start_row > result.scanned_end_row) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "scanned_start_row must not exceed scanned_end_row",
      path: ["scanned_start_row"],
    });
  }
  for (let index = 0; index < result.matches.length; index++) {
    const row = result.matches[index]!.row;
    if (row < result.scanned_start_row || row >= result.scanned_end_row) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        message: "match row is outside the scanned half-open range",
        path: ["matches", index, "row"],
      });
    }
  }
  const expectedTruncated = result.stop_reason === "match_limit"
    || result.stop_reason === "deadline";
  if (result.truncated !== expectedTruncated) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "truncated must match a match_limit or deadline stop",
      path: ["truncated"],
    });
  }
  if (
    result.stop_reason === "complete"
    && result.history_floor === "none"
    && result.scanned_start_row !== 0
  ) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "complete search without a history floor must start at row 0",
      path: ["scanned_start_row"],
    });
  }
  if (result.stop_reason === "row_limit"
      && result.next_before_row !== result.scanned_start_row) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "row_limit requires next_before_row at scanned_start_row",
      path: ["next_before_row"],
    });
  }
  if (result.stop_reason !== "row_limit" && result.next_before_row !== undefined) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "only row_limit can carry a continuation row",
      path: ["next_before_row"],
    });
  }
});
export type WorkerSearchScrollbackResult = z.infer<
  typeof WorkerSearchScrollbackResultSchema
>;

export const WorkerGlobalSearchErrorSchema = z.enum([
  "session_closed",
  "deadline",
  "epoch_changed",
  "no_terminal",
  "internal",
]);
export type WorkerGlobalSearchError = z.infer<typeof WorkerGlobalSearchErrorSchema>;

const WorkerGlobalSearchSessionIdSchema = z.string().uuid();
export const WorkerGlobalSearchEntrySchema = z.discriminatedUnion("status", [
  z.object({
    status: z.literal("ok"),
    session_id: WorkerGlobalSearchSessionIdSchema,
    result: WorkerSearchScrollbackResultSchema,
  }).strict(),
  z.object({
    status: z.literal("error"),
    session_id: WorkerGlobalSearchSessionIdSchema,
    error: WorkerGlobalSearchErrorSchema,
  }).strict(),
]);
export type WorkerGlobalSearchEntry = z.infer<typeof WorkerGlobalSearchEntrySchema>;

export const WorkerGlobalSearchResultSchema = z.object({
  entries: z.array(WorkerGlobalSearchEntrySchema)
    .max(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS)
    .readonly(),
}).strict().readonly().superRefine((result, context) => {
  const seenSessionIds = new Set<string>();
  for (let index = 0; index < result.entries.length; index++) {
    const sessionId = result.entries[index]!.session_id;
    if (seenSessionIds.has(sessionId)) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        message: "global search result session IDs must be unique",
        path: ["entries", index, "session_id"],
      });
    }
    seenSessionIds.add(sessionId);
  }
});
export type WorkerGlobalSearchResult = z.infer<typeof WorkerGlobalSearchResultSchema>;
