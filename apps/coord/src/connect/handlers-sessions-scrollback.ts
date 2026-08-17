// The two read-only scrollback RPCs — get-scrollback-cells (backfill paging for
// the SPA's history spacer) and search-scrollback (server-side find, because the
// SPA holds at most MAX_HELD_SCROLLBACK_ROWS of the worker's retained grid).
// Both forward a browser-command frame to the session's worker hub socket and
// await its rpc reply. Spread into makeSessionHandlers' single returned object
// literal (handlers-sessions.ts) — never registered as a second router.service().

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  SessionsGetScrollbackCellsResponseSchema,
  SessionsSearchScrollbackResponseSchema, SessionsSearchScrollbackMatchSchema,
} from "@roost/shared/proto/coordinator_pb";
import { cellRowToProto } from "@roost/shared/cell/cell-proto";
import type { CellRow } from "@roost/shared/cell";
import { asSessionId } from "@roost/shared/wire";
import { requireAuth } from "./auth-interceptor.ts";
import { getWorkerHubSocket } from "./worker-service.ts";
import { createPendingRpc } from "../router/pending-rpcs.ts";
import { sendBrowserCmd } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";

type ScrollbackMethods = "sessionsGetScrollbackCells" | "sessionsSearchScrollback";

export function makeSessionScrollbackHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, ScrollbackMethods> {
  return {
    async sessionsGetScrollbackCells(req, ctx) {
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      const pending = createPendingRpc<{
        rows: CellRow[];
        cols: number;
        total: number;
        start_row: number;
        end_row: number;
        grid_epoch: string;
      }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "get-scrollback-cells" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        grid_epoch: req.gridEpoch,
        end_row: Number(req.endRow),
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
      });
    },

    async sessionsSearchScrollback(req, ctx) {
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      const pending = createPendingRpc<{ matches: Array<{ row: number; col: number; len: number; preview: string }>; truncated: boolean; total: number; cols: number }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "search-scrollback" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        query: req.query,
        case_sensitive: req.caseSensitive,
        regex: req.regex,
        max_matches: req.maxMatches,
      });
      const res = await pending.promise.catch((err: unknown): never => {
        // rejectPendingRpc surfaces EVERY worker rpc-error as
        // ConnectError(rawMessage, Code.Internal), so the worker's fixed
        // "invalid regex: " message prefix is the only signal that separates a
        // user's bad pattern from a real worker failure — the find bar needs
        // InvalidArgument to blame the pattern instead of showing a fault toast.
        if (err instanceof ConnectError && err.code === Code.Internal
          && err.rawMessage.startsWith("invalid regex: ")) {
          throw new ConnectError(err.rawMessage, Code.InvalidArgument);
        }
        // Every failure path THROWS: an empty match list would read to the find
        // bar as "that string is not in this session", which is a lie whenever
        // the search did not actually run to completion.
        if (err instanceof ConnectError) {
          if (err.code !== Code.DeadlineExceeded) throw err;
          throw new ConnectError("scrollback search timed out", Code.Unavailable);
        }
        throw new ConnectError(`scrollback search failed: ${String(err)}`, Code.Internal);
      });
      return create(SessionsSearchScrollbackResponseSchema, {
        matches: res.matches.map((m) => create(SessionsSearchScrollbackMatchSchema, {
          row: BigInt(m.row), col: m.col, len: m.len, preview: m.preview,
        })),
        truncated: res.truncated,
        scrollbackTotal: BigInt(res.total),
        cols: res.cols,
      });
    },
  };
}
