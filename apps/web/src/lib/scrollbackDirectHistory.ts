// Resolves terminal history through the elected direct route when one owns this session.
// ScrollbackBackfill validates and splices the returned page; this module only
// selects its carrier. Coordinator history remains normal without an elected
// direct route and is attempted once after a direct read error or overlimit.

import type { SessionsGetScrollbackCellsResponse } from "@roost/protocol/proto/coordinator_pb";
import type { LocalScrollbackResponse } from "@roost/protocol/proto/local_terminal_pb";
import { coordClient } from "../client/rpc/connect.ts";
import { currentTerminalGenerationToken } from "../store/terminal-stream-publication.ts";
import {
  terminalDirectRegistry,
  type LocalScrollbackQuery,
  type TerminalDirectConnection,
} from "../store/terminal-stream-transport.ts";
import { terminalGenerationTokenEquals } from "../store/terminal-stream-types.ts";

export type ScrollbackPageResponse = Pick<
  SessionsGetScrollbackCellsResponse,
  "rows" | "cols" | "scrollbackTotal" | "startRow" | "endRow" | "gridEpoch" | "historyFloor"
>;

const DIRECT_SCROLLBACK_OVERLIMIT = "scrollback response exceeds direct transport limit";

/** Direct history is legal only for the exact canonical connection/token pair. */
export async function requestScrollbackPage(
  sessionId: string,
  query: LocalScrollbackQuery,
): Promise<ScrollbackPageResponse> {
  const direct = electedDirectHistoryConnection(sessionId);
  if (!direct) return coordClient.sessionsGetScrollbackCells(query);
  let directPage: LocalScrollbackResponse;
  try {
    directPage = await direct.requestScrollback(query);
  } catch {
    if (electedDirectHistoryConnection(sessionId) !== direct) {
      throw new Error("direct terminal scrollback route changed");
    }
    return coordClient.sessionsGetScrollbackCells(query);
  }
  if (directPage.error === "") {
    recordDirectHistoryResponseForSmoke(sessionId);
    return directPage;
  }
  if (directPage.error === DIRECT_SCROLLBACK_OVERLIMIT) {
    return coordClient.sessionsGetScrollbackCells(query);
  }
  throw new Error("direct terminal scrollback request was rejected");
}

function electedDirectHistoryConnection(
  sessionId: string,
): TerminalDirectConnection | null {
  const connection = terminalDirectRegistry.activeForSession(sessionId);
  const token = currentTerminalGenerationToken(sessionId);
  if (!connection || !token || !terminalGenerationTokenEquals(connection.token(), token)) {
    return null;
  }
  return connection;
}

function recordDirectHistoryResponseForSmoke(sessionId: string): void {
  if (import.meta.env.VITE_ROOST_SMOKE !== "1" || typeof window === "undefined") return;
  const smokeWindow = window as unknown as { __roostRecordDirectHistoryResponse?: (id: string) => void };
  smokeWindow.__roostRecordDirectHistoryResponse?.(sessionId);
}
