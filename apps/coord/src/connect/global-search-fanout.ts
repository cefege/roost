// Owns authorized session selection, worker grouping, result validation, and
// cancellation fan-out for dashboard-wide terminal search. The RPC handler owns
// request lifecycle and cursors; this module keeps the bounded worker page seam
// small enough to audit independently.

import { GlobalSearchPartialReason } from "@roost/shared/proto/coordinator_pb";
import {
  allocateGlobalSearchMatchLimits,
  GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
  WorkerGlobalSearchResultSchema,
  type WorkerGlobalSearchEntry,
  type WorkerSearchScrollbackResult,
} from "@roost/shared/terminal-search";
import { log } from "@roost/shared/log";
import { asSessionId } from "@roost/shared/wire";
import type { KyselyDB } from "../db/connection.ts";
import type { AccountDeviceCaller } from "./auth-interceptor.ts";
import { getWorkerHubSocket } from "./worker-service.ts";
import { sendBrowserCmd, type WorkerHubSocket } from "./router-helpers.ts";
import type { GlobalSearchSessionPosition } from "./global-search-cursors.ts";
import type { GlobalSearchPageLimits } from "./global-search-options.ts";

export type GlobalSearchMatch = {
  sessionId: string;
  row: number;
  col: number;
  len: number;
  preview: string;
  gridEpoch: string;
};

export interface GlobalSearchSessionOutcome {
  matches: readonly GlobalSearchMatch[];
  partialReason?: GlobalSearchPartialReason;
  continuation?: GlobalSearchSessionPosition;
  searched: boolean;
}

export interface OnlineGlobalSearchGroup {
  workerFp: string;
  socket: WorkerHubSocket;
  sessions: readonly GlobalSearchSessionPosition[];
  matchBudget: number;
}

export interface AuthorizedGlobalSearchPage {
  /** Newest-first sessions this page may search, capped at `maxSessions`. */
  sessions: GlobalSearchSessionPosition[];
  /** Exact count over the same authorization predicate. The page cap bounds
   *  the work, never the denominator: reporting the cap as the total told the
   *  browser a 100-session dashboard was fully searched after 32. */
  eligibleSessions: number;
}

export async function listAuthorizedGlobalSearchSessions(
  db: KyselyDB,
  dashboardId: string,
  maxSessions: number,
): Promise<AuthorizedGlobalSearchPage> {
  const rows = await authorizedGlobalSearchSessions(db, dashboardId)
    .select([
      "session.id as session_id",
      "session.worker_fp as worker_fp",
      "session.created_at as created_at",
    ])
    .orderBy("session.created_at", "desc")
    .orderBy("session.id", "desc")
    .limit(maxSessions)
    .execute();
  const counted = await authorizedGlobalSearchSessions(db, dashboardId)
    .select((eb) => eb.fn.countAll<number>().as("eligible"))
    .executeTakeFirst();
  return {
    sessions: rows.map((row) => ({
      sessionId: row.session_id,
      workerFp: row.worker_fp,
      gridEpoch: "",
    })),
    eligibleSessions: Math.max(Number(counted?.eligible ?? rows.length), rows.length),
  };
}

/** The single authorization predicate for dashboard-wide search: the page
 *  query and the denominator count MUST NOT drift apart. */
function authorizedGlobalSearchSessions(db: KyselyDB, dashboardId: string) {
  return db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .where("session.dashboard_id", "=", dashboardId)
    .where("session.status", "=", "open")
    .where("worker.dashboard_id", "=", dashboardId)
    .where("worker.deleted_at_ms", "is", null);
}

export async function reauthorizeGlobalSearchSessions(
  db: KyselyDB,
  dashboardId: string,
  positions: readonly GlobalSearchSessionPosition[],
): Promise<{
  authorized: GlobalSearchSessionPosition[];
  closedSessionIds: string[];
}> {
  const rows = await db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select(["session.id as session_id", "session.worker_fp as worker_fp"])
    .where("session.id", "in", positions.map((position) => position.sessionId))
    .where("session.dashboard_id", "=", dashboardId)
    .where("session.status", "=", "open")
    .where("worker.dashboard_id", "=", dashboardId)
    .where("worker.deleted_at_ms", "is", null)
    .execute();
  const workerBySession = new Map(
    rows.map((row) => [row.session_id, row.worker_fp] as const),
  );
  const authorized: GlobalSearchSessionPosition[] = [];
  const closedSessionIds: string[] = [];
  const seenSessionIds = new Set<string>();
  for (const position of positions) {
    if (seenSessionIds.has(position.sessionId)) continue;
    seenSessionIds.add(position.sessionId);
    const workerFp = workerBySession.get(position.sessionId);
    if (!workerFp || workerFp !== position.workerFp) {
      closedSessionIds.push(position.sessionId);
    } else {
      authorized.push(position);
    }
  }
  return { authorized, closedSessionIds };
}

export function groupOnlineGlobalSearchSessions(
  sessions: readonly GlobalSearchSessionPosition[],
  outcomes: Map<string, GlobalSearchSessionOutcome>,
  maxMatches: number,
): OnlineGlobalSearchGroup[] {
  const sessionsByWorker = new Map<string, GlobalSearchSessionPosition[]>();
  const seenSessionIds = new Set<string>();
  for (const session of sessions) {
    if (seenSessionIds.has(session.sessionId)) continue;
    seenSessionIds.add(session.sessionId);
    const group = sessionsByWorker.get(session.workerFp) ?? [];
    group.push(session);
    sessionsByWorker.set(session.workerFp, group);
  }
  const online: Array<Omit<OnlineGlobalSearchGroup, "matchBudget">> = [];
  for (const [workerFp, workerSessions] of sessionsByWorker) {
    const socket = getWorkerHubSocket(workerFp);
    if (socket) {
      online.push({ workerFp, socket, sessions: workerSessions });
      continue;
    }
    for (const session of workerSessions) {
      outcomes.set(session.sessionId, {
        matches: [],
        continuation: session,
        partialReason: GlobalSearchPartialReason.WORKER_UNAVAILABLE,
        searched: false,
      });
    }
  }
  const sessionCount = online.reduce(
    (total, group) => total + group.sessions.length,
    0,
  );
  if (sessionCount === 0) return [];
  const sessionBudgets = allocateGlobalSearchMatchLimits(maxMatches, sessionCount);
  const scheduled: OnlineGlobalSearchGroup[] = [];
  let budgetIndex = 0;
  for (const group of online) {
    let matchBudget = 0;
    const sessions: GlobalSearchSessionPosition[] = [];
    for (const session of group.sessions) {
      const sessionBudget = sessionBudgets[budgetIndex++]!;
      if (sessionBudget > 0) {
        sessions.push(session);
        matchBudget += sessionBudget;
      } else {
        outcomes.set(session.sessionId, {
          matches: [],
          continuation: session,
          searched: false,
        });
      }
    }
    if (sessions.length > 0) scheduled.push({ ...group, sessions, matchBudget });
  }
  return scheduled;
}

export function validateGlobalSearchGroupResult(
  rawResult: unknown,
  group: OnlineGlobalSearchGroup,
  limits: GlobalSearchPageLimits,
): readonly WorkerGlobalSearchEntry[] | null {
  const parsed = WorkerGlobalSearchResultSchema.safeParse(rawResult);
  if (!parsed.success || parsed.data.entries.length !== group.sessions.length) return null;
  const sessionMatchBudgets = allocateGlobalSearchMatchLimits(
    group.matchBudget,
    group.sessions.length,
  );
  let matchCount = 0;
  const matchIdentities = new Set<string>();
  for (let index = 0; index < parsed.data.entries.length; index++) {
    const entry = parsed.data.entries[index]!;
    const request = group.sessions[index]!;
    if (entry.session_id !== request.sessionId) return null;
    if (entry.status === "error") continue;
    const result = entry.result;
    const scannedRows = result.scanned_end_row - result.scanned_start_row;
    const epochMismatch = request.gridEpoch.length > 0
      && result.grid_epoch !== request.gridEpoch;
    const zeroRowDeadlineAfterEpochChange = epochMismatch
      && result.stop_reason === "deadline"
      && scannedRows === 0;
    if (
      (request.gridEpoch.length === 0 && request.beforeRow !== undefined)
      || scannedRows > limits.maxRowsPerSession
      || (request.beforeRow !== undefined
        && !epochMismatch
        && result.scanned_end_row !== request.beforeRow)
      || (result.stop_reason === "row_limit"
        && scannedRows !== limits.maxRowsPerSession)
      || (epochMismatch
        && result.stop_reason !== "epoch_changed"
        && !zeroRowDeadlineAfterEpochChange)
      || result.matches.length > sessionMatchBudgets[index]!
    ) return null;
    for (const match of result.matches) {
      const identity = JSON.stringify([
        entry.session_id,
        result.grid_epoch,
        match.row,
        match.col,
        match.len,
      ]);
      if (matchIdentities.has(identity)) return null;
      matchIdentities.add(identity);
    }
    matchCount += result.matches.length;
    if (matchCount > group.matchBudget) return null;
  }
  return parsed.data.entries;
}

/** A same-epoch deadline page that scanned nothing and resumed exactly where it
 *  was asked to start made no progress: handing that position back produces an
 *  endless chain of pages, each burning a page deadline and a worker lane
 *  without reading a row. herdr refuses such a request outright
 *  (`stale_content`); we keep the matches found so far and end the session as a
 *  deadline partial. A page whose epoch changed is excluded: its continuation
 *  drops the row entirely and restarts from the newest row, which is progress. */
function stalledAtRequestedRow(
  session: GlobalSearchSessionPosition,
  result: WorkerSearchScrollbackResult,
  epochMismatch: boolean,
): boolean {
  return !epochMismatch
    && result.scanned_end_row === result.scanned_start_row
    && session.beforeRow !== undefined
    && result.scanned_start_row === session.beforeRow;
}

function outcomeForSearchResult(
  session: GlobalSearchSessionPosition,
  result: WorkerSearchScrollbackResult,
): GlobalSearchSessionOutcome {
  let partialReason: GlobalSearchPartialReason | undefined;
  if (result.stop_reason === "epoch_changed") {
    partialReason = GlobalSearchPartialReason.EPOCH_CHANGED;
  } else if (result.stop_reason === "match_limit") {
    partialReason = GlobalSearchPartialReason.MATCH_LIMIT;
  } else if (result.stop_reason === "deadline") {
    partialReason = GlobalSearchPartialReason.DEADLINE;
  } else if (result.history_floor !== "none") {
    partialReason = GlobalSearchPartialReason.HISTORY_EVICTED;
  }
  const epochMismatch = session.gridEpoch.length > 0
    && session.gridEpoch !== result.grid_epoch;
  const stalled = stalledAtRequestedRow(session, result, epochMismatch);
  let continuation: GlobalSearchSessionPosition | undefined;
  if (result.stop_reason === "epoch_changed") {
    continuation = { ...session, gridEpoch: "", beforeRow: undefined };
  } else if (
    result.history_floor === "none"
    && (result.stop_reason === "row_limit" || result.stop_reason === "match_limit")
    && result.next_before_row !== undefined
  ) {
    continuation = {
      ...session,
      gridEpoch: result.grid_epoch,
      beforeRow: result.next_before_row,
    };
  } else if (
    result.history_floor === "none"
    && result.stop_reason === "deadline"
    && !stalled
  ) {
    continuation = epochMismatch
      ? { ...session, gridEpoch: result.grid_epoch, beforeRow: undefined }
      : {
        ...session,
        gridEpoch: result.grid_epoch,
        beforeRow: result.scanned_start_row,
      };
  }
  return {
    matches: result.matches.map((match) => ({
      sessionId: session.sessionId,
      row: match.row,
      col: match.col,
      len: match.len,
      preview: match.preview,
      gridEpoch: result.grid_epoch,
    })),
    ...(partialReason === undefined ? {} : { partialReason }),
    ...(continuation === undefined ? {} : { continuation }),
    searched: true,
  };
}

export function outcomeForGlobalSearchEntry(
  session: GlobalSearchSessionPosition,
  entry: WorkerGlobalSearchEntry,
): GlobalSearchSessionOutcome {
  if (entry.status === "ok") return outcomeForSearchResult(session, entry.result);
  const partialReason = entry.error === "deadline"
    ? GlobalSearchPartialReason.DEADLINE
    : entry.error === "epoch_changed"
      ? GlobalSearchPartialReason.EPOCH_CHANGED
      : entry.error === "session_closed" || entry.error === "no_terminal"
        ? GlobalSearchPartialReason.SESSION_CLOSED
        : GlobalSearchPartialReason.MALFORMED_RESULT;
  // A malformed entry scanned nothing, so resuming it mid-page would repeat
  // the same request forever. A session with no row position yet has never
  // been searched, and retrying it is real progress.
  const stalledMalformed = partialReason === GlobalSearchPartialReason.MALFORMED_RESULT
    && session.beforeRow !== undefined;
  return {
    matches: [],
    partialReason,
    ...(partialReason === GlobalSearchPartialReason.SESSION_CLOSED || stalledMalformed
      ? {}
      : { continuation: session }),
    searched: true,
  };
}

export function sendGlobalSearchCancellationBatches(
  caller: AccountDeviceCaller,
  viewerId: string,
  searchId: string,
  sessions: readonly GlobalSearchSessionPosition[],
): void {
  const byWorker = new Map<string, string[]>();
  const seenSessionIds = new Set<string>();
  for (const session of sessions) {
    if (
      seenSessionIds.size >= GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS
      || seenSessionIds.has(session.sessionId)
    ) continue;
    seenSessionIds.add(session.sessionId);
    const sessionIds = byWorker.get(session.workerFp) ?? [];
    sessionIds.push(session.sessionId);
    byWorker.set(session.workerFp, sessionIds);
  }
  for (const [workerFp, sessionIds] of byWorker) {
    const socket = getWorkerHubSocket(workerFp);
    if (!socket || sessionIds.length === 0) continue;
    const requestId = crypto.randomUUID();
    try {
      sendBrowserCmd(socket, caller, requestId, {
        kind: "cancel-scrollback-search-batch",
        request_id: requestId,
        search_id: searchId,
        session_ids: sessionIds.map(asSessionId),
      }, viewerId);
    } catch (error) {
      log.warn("global-search", "cancel_send_failed", {
        worker_fp: workerFp,
        error: String(error),
      });
    }
  }
}
