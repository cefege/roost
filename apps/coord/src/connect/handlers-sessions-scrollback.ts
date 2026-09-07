// Owns terminal cell retrieval, bounded scrollback search, and explicit search
// cancellation for session RPCs. Searches run on the worker because the SPA
// holds only a bounded window of the authoritative terminal grid.
// The handlers forward browser-command frames through the worker hub and are
// spread into makeSessionHandlers' single returned service object.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  ScrollbackHistoryFloor as PbScrollbackHistoryFloor,
  SearchStopReason as PbSearchStopReason,
  SessionsCancelScrollbackSearchResponseSchema,
  SessionsGetScrollbackCellsResponseSchema,
  SessionsSearchScrollbackMatchSchema,
  SessionsSearchScrollbackResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  TERMINAL_SEARCH_MAX_MATCHES,
  TERMINAL_SEARCH_MAX_ROWS,
  TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
  TERMINAL_SEARCH_RPC_DEADLINE_MS,
  TerminalSearchIdSchema,
  TerminalSearchGridEpochSchema,
  TerminalSearchMaxMatchesSchema,
  TerminalSearchMaxRowsSchema,
  TerminalSearchQuerySchema,
  WorkerSearchScrollbackResultSchema,
  type SearchStopReason,
} from "@roost/shared/terminal-search";
import { log } from "@roost/shared/log";
import { cellRowToProto } from "@roost/shared/cell/cell-proto";
import type { CellRow } from "@roost/shared/cell";
import { asSessionId, type ScrollbackHistoryFloor } from "@roost/shared/wire";
import {
  requireAccountDevice,
  requireDashboardActor,
  requireSearchTabId,
  tabIdKey,
} from "./auth-interceptor.ts";
import {
  cancelPendingRpc,
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import { sendBrowserCmd, requireSessionWorkerSocket } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";

type ScrollbackMethods =
  | "sessionsGetScrollbackCells"
  | "sessionsSearchScrollback"
  | "sessionsCancelScrollbackSearch";

const MAX_SAFE_ROW = BigInt(Number.MAX_SAFE_INTEGER);

const SEARCH_STOP_REASON_PROTO: Record<SearchStopReason, PbSearchStopReason> = {
  complete: PbSearchStopReason.COMPLETE,
  row_limit: PbSearchStopReason.ROW_LIMIT,
  match_limit: PbSearchStopReason.MATCH_LIMIT,
  deadline: PbSearchStopReason.DEADLINE,
  epoch_changed: PbSearchStopReason.EPOCH_CHANGED,
};

function requireJsonSafeRow(value: bigint, field: string): number {
  if (
    typeof value !== "bigint"
    || value < 0n
    || value > MAX_SAFE_ROW
  ) {
    throw new ConnectError(
      `${field} must be a nonnegative JSON-safe integer`,
      Code.InvalidArgument,
    );
  }
  return Number(value);
}

function validateSearchRequest(req: {
  query: string;
  gridEpoch: string;
  searchId: string;
  maxRows: number;
  maxMatches: number;
  beforeRow?: bigint;
}): number | undefined {
  if (!TerminalSearchIdSchema.safeParse(req.searchId).success) {
    throw new ConnectError(
      "scrollback search search_id must contain 1 to 64 characters",
      Code.InvalidArgument,
    );
  }
  if (!TerminalSearchGridEpochSchema.safeParse(req.gridEpoch).success) {
    throw new ConnectError(
      "scrollback search grid_epoch is too long",
      Code.InvalidArgument,
    );
  }
  if (!TerminalSearchQuerySchema.safeParse(req.query).success) {
    throw new ConnectError(
      `scrollback search query must contain at most ${TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS} Unicode code points`,
      Code.InvalidArgument,
    );
  }
  if (!TerminalSearchMaxRowsSchema.safeParse(req.maxRows).success) {
    throw new ConnectError(
      `scrollback search max_rows must be between 1 and ${TERMINAL_SEARCH_MAX_ROWS}`,
      Code.InvalidArgument,
    );
  }
  if (!TerminalSearchMaxMatchesSchema.safeParse(req.maxMatches).success) {
    throw new ConnectError(
      `scrollback search max_matches must be between 1 and ${TERMINAL_SEARCH_MAX_MATCHES}`,
      Code.InvalidArgument,
    );
  }
  return req.beforeRow === undefined
    ? undefined
    : requireJsonSafeRow(req.beforeRow, "scrollback search before_row");
}

function parseWorkerSearchResult(
  data: unknown,
  request: {
    beforeRow?: number;
    maxRows: number;
    maxMatches: number;
    gridEpoch: string;
  },
) {
  const parsed = WorkerSearchScrollbackResultSchema.safeParse(data);
  if (!parsed.success) {
    throw new ConnectError("malformed scrollback search result", Code.Internal);
  }
  const result = parsed.data;
  const scannedRows = result.scanned_end_row - result.scanned_start_row;
  const epochMismatchAllowed = result.stop_reason === "epoch_changed"
    || (
      result.stop_reason === "deadline"
      && result.scanned_start_row === result.scanned_end_row
    );
  const requestMismatch = result.matches.length > request.maxMatches
    || scannedRows > request.maxRows
    || (request.beforeRow !== undefined && result.scanned_end_row > request.beforeRow)
    || (result.stop_reason === "row_limit" && scannedRows !== request.maxRows)
    || (result.stop_reason === "match_limit"
      && result.matches.length !== request.maxMatches)
    || (
      request.gridEpoch !== ""
      && result.grid_epoch !== request.gridEpoch
      && !epochMismatchAllowed
    );
  if (requestMismatch) {
    throw new ConnectError("malformed scrollback search result", Code.Internal);
  }
  return result;
}

function remapSearchFailure(error: unknown): never {
  if (
    error instanceof ConnectError
    && error.code === Code.Internal
    && error.rawMessage.startsWith("invalid regex: ")
  ) {
    throw new ConnectError(error.rawMessage, Code.InvalidArgument);
  }
  if (error instanceof ConnectError) {
    if (error.code !== Code.DeadlineExceeded) throw error;
    throw new ConnectError("scrollback search timed out", Code.Unavailable);
  }
  throw new ConnectError(`scrollback search failed: ${String(error)}`, Code.Internal);
}

/** The worker names the floor it hit in the wire vocabulary; this RPC speaks the
 *  proto enum. One total map so a new reason cannot be silently dropped. */
const HISTORY_FLOOR_PROTO: Record<ScrollbackHistoryFloor, PbScrollbackHistoryFloor> = {
  none: PbScrollbackHistoryFloor.UNSPECIFIED,
  evicted: PbScrollbackHistoryFloor.EVICTED,
  resize_replay: PbScrollbackHistoryFloor.RESIZE_REPLAY,
};

export function makeSessionScrollbackHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, ScrollbackMethods> {
  return {
    async sessionsGetScrollbackCells(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      const endRow = requireJsonSafeRow(req.endRow, "scrollback cells end_row");
      const { row, sock } = await requireSessionWorkerSocket(deps.db, actor, req.sessionId);
      const pending = createPendingRpc<{
        rows: CellRow[];
        cols: number;
        total: number;
        start_row: number;
        end_row: number;
        grid_epoch: string;
        history_floor: ScrollbackHistoryFloor;
      }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "get-scrollback-cells" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        grid_epoch: req.gridEpoch,
        end_row: endRow,
        max_rows: req.maxRows,
      });
      let res;
      try {
        res = await pending.promise;
      } catch (error) {
        if (error instanceof ConnectError) {
          if (error.code !== Code.DeadlineExceeded) throw error;
          throw new ConnectError("scrollback cells serve timed out", Code.Unavailable);
        }
        throw new ConnectError(`scrollback cells serve failed: ${String(error)}`, Code.Internal);
      }
      return create(SessionsGetScrollbackCellsResponseSchema, {
        rows: res.rows.map(cellRowToProto),
        cols: res.cols,
        scrollbackTotal: BigInt(res.total),
        startRow: BigInt(res.start_row),
        endRow: BigInt(res.end_row),
        gridEpoch: res.grid_epoch,
        // A worker older than the field reports nothing, which is exactly
        // UNSPECIFIED: no floor claim, rather than a guessed one.
        historyFloor: HISTORY_FLOOR_PROTO[res.history_floor] ?? PbScrollbackHistoryFloor.UNSPECIFIED,
      });
    },

    async sessionsSearchScrollback(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      const viewerId = `${caller.fingerprint}:${requireSearchTabId(ctx.values)}`;
      const beforeRow = validateSearchRequest(req);
      const { row, sock } = await requireSessionWorkerSocket(
        deps.db,
        actor,
        req.sessionId,
      );
      const pending = createPendingRpc<unknown>(
        TERMINAL_SEARCH_RPC_DEADLINE_MS,
        row.worker_fp,
      );
      let commandSent = false;
      let cancellationHandled = false;
      const cancelSearch = (): void => {
        if (cancellationHandled) return;
        cancellationHandled = true;
        if (commandSent) {
          try {
            sendBrowserCmd(sock, caller, pending.request_id, {
              kind: "cancel-scrollback-search" as const,
              request_id: pending.request_id,
              session_id: asSessionId(req.sessionId),
              search_request_id: req.searchId,
            }, viewerId);
          } catch (error) {
            log.warn("scrollback-search", "cancel_send_failed", {
              request_id: pending.request_id,
              worker_fp: row.worker_fp,
              error: String(error),
            });
          }
        }
        cancelPendingRpc(pending.request_id, row.worker_fp);
      };
      ctx.signal.addEventListener("abort", cancelSearch, { once: true });
      if (!ctx.signal.aborted) {
        try {
          sendBrowserCmd(sock, caller, pending.request_id, {
            kind: "search-scrollback" as const,
            request_id: pending.request_id,
            session_id: asSessionId(req.sessionId),
            search_id: req.searchId,
            grid_epoch: req.gridEpoch,
            query: req.query,
            case_sensitive: req.caseSensitive,
            regex: req.regex,
            ...(beforeRow === undefined ? {} : { before_row: beforeRow }),
            max_rows: req.maxRows,
            max_matches: req.maxMatches,
          }, viewerId);
          commandSent = true;
        } catch (error) {
          rejectPendingRpcUnavailable(
            pending.request_id,
            error instanceof ConnectError
              ? error.rawMessage
              : `scrollback search send failed: ${String(error)}`,
            row.worker_fp,
          );
        }
      }
      if (ctx.signal.aborted) cancelSearch();
      let rawResult;
      try {
        rawResult = await pending.promise.catch(remapSearchFailure);
      } finally {
        ctx.signal.removeEventListener("abort", cancelSearch);
      }
      const result = parseWorkerSearchResult(rawResult, {
        beforeRow,
        maxRows: req.maxRows,
        maxMatches: req.maxMatches,
        gridEpoch: req.gridEpoch,
      });
      return create(SessionsSearchScrollbackResponseSchema, {
        matches: result.matches.map((match) =>
          create(SessionsSearchScrollbackMatchSchema, {
            row: BigInt(match.row),
            col: match.col,
            len: match.len,
            preview: match.preview,
          })
        ),
        truncated: result.truncated,
        scrollbackTotal: BigInt(result.scrollback_total),
        cols: result.cols,
        gridEpoch: result.grid_epoch,
        scannedStartRow: BigInt(result.scanned_start_row),
        scannedEndRow: BigInt(result.scanned_end_row),
        historyFloor: HISTORY_FLOOR_PROTO[result.history_floor],
        nextBeforeRow: result.next_before_row === undefined
          ? undefined
          : BigInt(result.next_before_row),
        stopReason: SEARCH_STOP_REASON_PROTO[result.stop_reason],
      });
    },

    async sessionsCancelScrollbackSearch(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      if (!TerminalSearchIdSchema.safeParse(req.searchId).success) {
        throw new ConnectError(
          "scrollback search search_id must contain 1 to 64 characters",
          Code.InvalidArgument,
        );
      }
      const { sock } = await requireSessionWorkerSocket(
        deps.db,
        actor,
        req.sessionId,
      );
      const tabId = ctx.values.get(tabIdKey);
      const viewerId = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
      sendBrowserCmd(sock, caller, req.searchId, {
        kind: "cancel-scrollback-search" as const,
        request_id: req.searchId,
        session_id: asSessionId(req.sessionId),
        search_request_id: req.searchId,
      }, viewerId);
      return create(SessionsCancelScrollbackSearchResponseSchema, {});
    },
  };
}
