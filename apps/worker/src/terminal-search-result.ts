// Validates and publishes worker scrollback-search RPC results.
// The scanner calls this boundary for diagnostics and coordinator replies.
// Pre-scan deadlines derive their empty range from the current terminal grid.

import { cellGridEpoch, scrollbackOrigin } from "@roost/shared/cell";
import { diag } from "@roost/shared/diag";
import {
  WorkerSearchScrollbackResultSchema,
  countUnicodeCodePoints,
  type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import type { ClientControlFrame } from "@roost/shared/wire";
import { historyFloorReason } from "./session-scrollback.ts";
import type { SessionShellRecord } from "./session-record.ts";
import type { CoordLink } from "./transport/coord-link.ts";

type SearchFrame = Extract<ClientControlFrame, { kind: "search-scrollback" }>;

export function publishSearchResult(
  frame: SearchFrame,
  requestId: string,
  coordLink: CoordLink,
  session: SessionShellRecord,
  data: WorkerSearchScrollbackResult,
): void {
  const result = WorkerSearchScrollbackResultSchema.parse(data);
  diag("scrollback.search", {
    sid: session.sessionId,
    channel_id: session.channelId,
    session_trace_id: session.session_trace_id,
    request_id: requestId,
    query_code_points: countUnicodeCodePoints(frame.query),
    regex: frame.regex,
    case_sensitive: frame.case_sensitive,
    matches: result.matches.length,
    rows_scanned: result.scanned_end_row - result.scanned_start_row,
    stop_reason: result.stop_reason,
    grid_epoch: result.grid_epoch,
  });
  coordLink.send({ kind: "rpc-ok", request_id: requestId, data: result });
}

export function publishPreScanDeadline(
  frame: SearchFrame,
  requestId: string,
  coordLink: CoordLink,
  session: SessionShellRecord,
): void {
  const core = session.wtermCore;
  const floor = scrollbackOrigin(core, session.cell_emit);
  const scrollbackTotal = floor + core.getScrollbackCount();
  const newestExclusive = scrollbackTotal + core.getRows();
  const boundary = Math.min(frame.before_row ?? newestExclusive, newestExclusive);
  publishSearchResult(frame, requestId, coordLink, session, {
    matches: [],
    truncated: true,
    scrollback_total: scrollbackTotal,
    cols: core.getCols(),
    grid_epoch: cellGridEpoch(session.cell_emit),
    scanned_start_row: boundary,
    scanned_end_row: boundary,
    history_floor: boundary <= floor
      ? historyFloorReason(session, floor > 0 ? floor - 1 : 0, floor)
      : "none",
    stop_reason: "deadline",
  });
}
