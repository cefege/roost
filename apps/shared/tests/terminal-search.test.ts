// Terminal-search wire and protobuf contract coverage.
// These cases pin Unicode bounds, safe paging numerics, strict result
// semantics, and the generated coordinator paging fields.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  GlobalSearchPartialReason,
  ScrollbackHistoryFloor as PbScrollbackHistoryFloor,
  SearchStopReason as PbSearchStopReason,
  SessionsSearchGlobalRequestSchema,
  SessionsSearchGlobalResponseSchema,
  SessionsSearchScrollbackRequestSchema,
  SessionsSearchScrollbackResponseSchema,
} from "../src/gen/roost/v1/coordinator_pb.ts";
import {
  allocateGlobalSearchMatchLimits,
  GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
  GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
  SearchStopReasonSchema,
  TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH,
  TERMINAL_SEARCH_MAX_MATCHES,
  TERMINAL_SEARCH_MAX_ROWS,
  TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS,
  TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
  TerminalSearchQuerySchema,
  WorkerSearchScrollbackResultSchema,
  WorkerGlobalSearchResultSchema,
  countUnicodeCodePoints,
  truncateUnicodeCodePoints,
} from "../src/terminal-search.ts";
import { ClientControlFrame } from "../src/wire/index.ts";

const FIXTURE_SESSION = {
  id: "00000000-0000-0000-0000-000000000001",
};
describe("bounded terminal-search contract", () => {
  const astralCodePoint = "\u{1f642}";
  const baseFrame = {
    kind: "search-scrollback" as const,
    request_id: "search-request",
    session_id: FIXTURE_SESSION.id,
    search_id: "search-id",
    grid_epoch: "epoch:1",
    query: "needle",
    case_sensitive: false,
    regex: false,
    max_rows: TERMINAL_SEARCH_MAX_ROWS,
    max_matches: TERMINAL_SEARCH_MAX_MATCHES,
  };
  const baseResult = {
    matches: [{ row: 9, col: 1, len: 2, preview: "match" }],
    truncated: false,
    total: 10,
    cols: 80,
    grid_epoch: "epoch:1",
    scanned_start_row: 0,
    scanned_end_row: 10,
    history_floor: "none" as const,
    stop_reason: "complete" as const,
  };

  test("counts and truncates Unicode code points without splitting astral characters", () => {
    const value = `A${astralCodePoint}B`;
    expect(countUnicodeCodePoints(value)).toBe(3);
    expect(truncateUnicodeCodePoints(value, 2)).toBe(`A${astralCodePoint}`);
    expect(truncateUnicodeCodePoints(value, 3)).toBe(value);
    expect(() => truncateUnicodeCodePoints(value, -1)).toThrow(RangeError);
  });

  test("accepts exact query, row, and match request boundaries", () => {
    const query = astralCodePoint.repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS);
    expect(query.length).toBe(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS * 2);
    expect(TerminalSearchQuerySchema.safeParse(query).success).toBe(true);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      query,
      before_row: Number.MAX_SAFE_INTEGER,
    }).success).toBe(true);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      query: astralCodePoint,
      before_row: 0,
      max_rows: 1,
      max_matches: 1,
    }).success).toBe(true);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      query: "",
    }).success).toBe(true);
    expect(ClientControlFrame.safeParse({
      kind: "cancel-scrollback-search",
      request_id: "cancel",
      session_id: FIXTURE_SESSION.id,
      search_request_id: "search-request",
    }).success).toBe(true);
  });

  test("rejects over-limit queries, rows, page sizes, and match counts", () => {
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      query: astralCodePoint.repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS + 1),
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      grid_epoch: "e".repeat(TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH + 1),
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      before_row: Number.MAX_SAFE_INTEGER + 1,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      before_row: -1,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      max_rows: TERMINAL_SEARCH_MAX_ROWS + 1,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      max_matches: TERMINAL_SEARCH_MAX_MATCHES + 1,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      max_rows: 0,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...baseFrame,
      max_matches: 0,
    }).success).toBe(false);
  });

  test("validates exact worker-result match and preview boundaries", () => {
    const boundaryMatch = {
      row: 0,
      col: 0xffff_ffff,
      len: 0xffff_ffff,
      preview: astralCodePoint.repeat(TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS),
    };
    const boundaryResult = {
      ...baseResult,
      matches: Array.from(
        { length: TERMINAL_SEARCH_MAX_MATCHES },
        () => boundaryMatch,
      ),
      scanned_end_row: 1,
    };
    expect(WorkerSearchScrollbackResultSchema.safeParse(boundaryResult).success).toBe(true);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      matches: [{
        ...baseResult.matches[0],
        preview: astralCodePoint.repeat(TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS + 1),
      }],
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...boundaryResult,
      matches: [...boundaryResult.matches, boundaryMatch],
    }).success).toBe(false);
  });

  test("rejects unsafe rows, unknown reasons, and unknown result fields", () => {
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      scanned_end_row: Number.MAX_SAFE_INTEGER + 1,
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      matches: [{ ...baseResult.matches[0], col: 0x1_0000_0000 }],
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      grid_epoch: "e".repeat(TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH + 1),
    }).success).toBe(false);
    expect(SearchStopReasonSchema.safeParse("cancelled").success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      stop_reason: "cancelled",
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      unvalidated: true,
    }).success).toBe(false);
  });

  test("enforces half-open rows, continuations, and truncated stop semantics", () => {
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      matches: [{ ...baseResult.matches[0], row: 10 }],
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      stop_reason: "row_limit",
      next_before_row: 1,
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      stop_reason: "row_limit",
      next_before_row: baseResult.scanned_start_row,
    }).success).toBe(true);
    for (const stopReason of ["complete", "match_limit", "deadline", "epoch_changed"] as const) {
      const truncated = stopReason === "match_limit" || stopReason === "deadline";
      expect(WorkerSearchScrollbackResultSchema.safeParse({
        ...baseResult, stop_reason: stopReason, truncated, next_before_row: 0,
      }).success).toBe(false);
    }
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      scanned_start_row: 1,
      matches: [{ ...baseResult.matches[0], row: 1 }],
    }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({
      ...baseResult,
      scanned_start_row: 1,
      matches: [{ ...baseResult.matches[0], row: 1 }],
      history_floor: "evicted",
    }).success).toBe(true);
    expect(WorkerSearchScrollbackResultSchema.safeParse({ ...baseResult, stop_reason: "deadline" }).success).toBe(false);
    expect(WorkerSearchScrollbackResultSchema.safeParse({ ...baseResult, stop_reason: "deadline", truncated: true }).success).toBe(true);
  });

  test("round-trips every proto paging field and stop enum", () => {
    const request = create(SessionsSearchScrollbackRequestSchema, {
      sessionId: FIXTURE_SESSION.id,
      searchId: "search-id",
      query: "needle",
      caseSensitive: true,
      regex: false,
      maxMatches: TERMINAL_SEARCH_MAX_MATCHES,
      gridEpoch: "epoch:1",
      beforeRow: 90n,
      maxRows: TERMINAL_SEARCH_MAX_ROWS,
    });
    const decodedRequest = fromBinary(
      SessionsSearchScrollbackRequestSchema,
      toBinary(SessionsSearchScrollbackRequestSchema, request),
    );
    expect(decodedRequest.beforeRow).toBe(90n);
    expect(decodedRequest.maxRows).toBe(TERMINAL_SEARCH_MAX_ROWS);
    expect(decodedRequest.searchId).toBe("search-id");

    const response = create(SessionsSearchScrollbackResponseSchema, {
      matches: [],
      truncated: false,
      scrollbackTotal: 100n,
      cols: 80,
      gridEpoch: "epoch:1",
      scannedStartRow: 10n,
      scannedEndRow: 90n,
      historyFloor: PbScrollbackHistoryFloor.EVICTED,
      nextBeforeRow: 10n,
      stopReason: PbSearchStopReason.ROW_LIMIT,
    });
    const decodedResponse = fromBinary(
      SessionsSearchScrollbackResponseSchema,
      toBinary(SessionsSearchScrollbackResponseSchema, response),
    );
    expect(decodedResponse.scannedStartRow).toBe(10n);
    expect(decodedResponse.scannedEndRow).toBe(90n);
    expect(decodedResponse.historyFloor).toBe(PbScrollbackHistoryFloor.EVICTED);
    expect(decodedResponse.nextBeforeRow).toBe(10n);
    expect(decodedResponse.stopReason).toBe(PbSearchStopReason.ROW_LIMIT);
  });

  test("bounds worker-global batch wire requests and aggregate replies", () => {
    expect(allocateGlobalSearchMatchLimits(
      GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
      GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    )).toEqual(Array(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS).fill(8));
    const sessions = Array.from({ length: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS }, (_, index) => ({
      session_id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
      grid_epoch: index === 0 ? "" : `epoch:${index}`,
      ...(index === 0 ? {} : { before_row: 10 }),
    }));
    const batch = {
      kind: "search-scrollback-batch" as const,
      request_id: "batch-request",
      search_id: "global-search",
      query: "needle",
      case_sensitive: false,
      sessions,
      max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
      max_matches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
      deadline_ms: GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
    };
    expect(ClientControlFrame.safeParse(batch).success).toBe(true);
    expect(ClientControlFrame.safeParse({
      ...batch,
      max_rows_per_session: 1,
      max_matches: 1,
    }).success).toBe(true);
    expect(ClientControlFrame.safeParse({
      ...batch,
      regex: false,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...batch,
      sessions: [...sessions, sessions[0]],
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...batch,
      max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION + 1,
    }).success).toBe(false);
    expect(ClientControlFrame.safeParse({
      ...batch,
      sessions: [sessions[0]!, ...sessions.slice(0, -1)],
    }).success).toBe(false);
    const entries = sessions.map(({ session_id }) => ({
      status: "ok" as const,
      session_id,
      result: baseResult,
    }));
    expect(WorkerGlobalSearchResultSchema.safeParse({ entries }).success).toBe(true);
    expect(WorkerGlobalSearchResultSchema.safeParse({
      entries: [...entries, entries[0]],
    }).success).toBe(false);
    expect(WorkerGlobalSearchResultSchema.safeParse({
      entries: [entries[0]!, ...entries.slice(0, -1)],
    }).success).toBe(false);
  });

  test("round-trips global search cursor, match, and typed partial fields", () => {
    const request = create(SessionsSearchGlobalRequestSchema, {
      query: "needle",
      caseSensitive: true,
      searchId: "global-search",
      cursor: "opaque-cursor",
    });
    const decodedRequest = fromBinary(
      SessionsSearchGlobalRequestSchema,
      toBinary(SessionsSearchGlobalRequestSchema, request),
    );
    expect(decodedRequest).toMatchObject({
      query: "needle",
      caseSensitive: true,
      searchId: "global-search",
      cursor: "opaque-cursor",
    });
    const response = create(SessionsSearchGlobalResponseSchema, {
      matches: [{
        sessionId: FIXTURE_SESSION.id,
        row: 9n,
        col: 1,
        len: 2,
        preview: "match",
        gridEpoch: "epoch:1",
      }],
      partials: [{
        sessionId: FIXTURE_SESSION.id,
        reason: GlobalSearchPartialReason.DEADLINE,
      }],
      nextCursor: "next-cursor",
      searchedSessions: 1,
      eligibleSessions: 2,
      truncated: true,
    });
    const decodedResponse = fromBinary(
      SessionsSearchGlobalResponseSchema,
      toBinary(SessionsSearchGlobalResponseSchema, response),
    );
    expect(decodedResponse.matches[0]?.row).toBe(9n);
    expect(decodedResponse.partials[0]?.reason).toBe(GlobalSearchPartialReason.DEADLINE);
    expect(decodedResponse.nextCursor).toBe("next-cursor");
    expect(decodedResponse.truncated).toBe(true);
  });
});
