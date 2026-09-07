// One bounded page chain over the coordinator scrollback-search RPC.
// TerminalFindController runs it for a fresh query and again to slide the
// window onto older rows; the chain holds no state past its own return.
// Guards and decoding come from terminalFindPaging.

import type { SessionsSearchScrollbackResponse } from "@roost/shared/proto/coordinator_pb";
import { SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import {
  TERMINAL_SEARCH_MAX_MATCHES,
  TERMINAL_SEARCH_MAX_PAGES,
  TERMINAL_SEARCH_MAX_ROWS,
} from "@roost/shared/terminal-search";
import { coordClient } from "../connect.ts";
import {
  decodePageMatches,
  searchContinuationIsValid,
  searchPageRangeIsValid,
  type FindMatch,
} from "./terminalFindPaging.ts";

/** The page of older rows a match cap left unscanned, plus the page budget
 *  already spent — the ceiling spans the query, not one chain. */
export interface OlderMatchPage {
  readonly epoch: string;
  readonly beforeRow: bigint;
  readonly pagesUsed: number;
}

export interface PageChainRequest {
  readonly sessionId: string;
  readonly searchId: string;
  readonly query: string;
  readonly caseSensitive: boolean;
  readonly regex: boolean;
  /** Grid numbering to pin; "" until the first page names one. */
  readonly epoch: string;
  /** Cursor to resume from, null to start at the newest row. */
  readonly resume: OlderMatchPage | null;
  readonly signal: AbortSignal;
  /** Live pane numbering; a chain never publishes across a renumbering. */
  readonly paneEpoch: () => string;
  /** False once a newer search owns the pane, which ends the chain silently. */
  readonly current: () => boolean;
}

export type PageChainOutcome =
  | { kind: "abandoned" }
  | { kind: "epoch-changed" }
  | {
    kind: "matches";
    matches: readonly FindMatch[];
    /** Older matches are known to exist beyond `matches`. */
    truncated: boolean;
    failed: boolean;
    /** Cursor onto those older matches, null when none is usable. */
    older: OlderMatchPage | null;
  };

export async function runTerminalFindPageChain(
  request: PageChainRequest,
): Promise<PageChainOutcome> {
  const initialEpoch = request.epoch;
  let requestedEpoch = initialEpoch;
  let beforeRow = request.resume?.beforeRow;
  let pages = request.resume?.pagesUsed ?? 0;
  const found: FindMatch[] = [];
  const paneAcceptsEpoch = (epoch: string): boolean => (
    request.paneEpoch() === epoch
    || (initialEpoch === "" && request.paneEpoch() === "" && epoch !== "")
  );
  const failedPartial = (): PageChainOutcome => (
    { kind: "matches", matches: found, truncated: false, failed: true, older: null }
  );

  for (;;) {
    if (!request.current()) return { kind: "abandoned" };
    if (pages >= TERMINAL_SEARCH_MAX_PAGES) return failedPartial();
    if (!paneAcceptsEpoch(requestedEpoch)) return { kind: "epoch-changed" };

    const remainingMatches = TERMINAL_SEARCH_MAX_MATCHES - found.length;
    if (remainingMatches <= 0) {
      return { kind: "matches", matches: found, truncated: true, failed: false, older: null };
    }
    let response: SessionsSearchScrollbackResponse;
    try {
      response = await coordClient.sessionsSearchScrollback({
        sessionId: request.sessionId,
        searchId: request.searchId,
        gridEpoch: requestedEpoch,
        query: request.query,
        caseSensitive: request.caseSensitive,
        regex: request.regex,
        maxRows: TERMINAL_SEARCH_MAX_ROWS,
        maxMatches: remainingMatches,
        ...(beforeRow === undefined ? {} : { beforeRow }),
      }, { signal: request.signal });
    } catch {
      if (!request.current()) return { kind: "abandoned" };
      if (!paneAcceptsEpoch(requestedEpoch)) return { kind: "epoch-changed" };
      return failedPartial();
    }
    pages++;

    if (!request.current()) return { kind: "abandoned" };
    if (response.stopReason === SearchStopReason.EPOCH_CHANGED) {
      return { kind: "epoch-changed" };
    }
    if (requestedEpoch === "") {
      if (response.gridEpoch === "") return failedPartial();
      requestedEpoch = response.gridEpoch;
    } else if (response.gridEpoch !== requestedEpoch) {
      return { kind: "epoch-changed" };
    }
    if (!paneAcceptsEpoch(requestedEpoch)) return { kind: "epoch-changed" };
    if (!searchPageRangeIsValid(response, beforeRow)) return failedPartial();

    const pageMatches = decodePageMatches(response, requestedEpoch);
    if (pageMatches === null || pageMatches.length > remainingMatches) {
      return failedPartial();
    }
    found.push(...pageMatches);

    const mayContinue = response.stopReason === SearchStopReason.ROW_LIMIT
      || response.stopReason === SearchStopReason.MATCH_LIMIT;
    if (!mayContinue && response.nextBeforeRow !== undefined) return failedPartial();
    if (response.stopReason === SearchStopReason.COMPLETE) {
      return { kind: "matches", matches: found, truncated: false, failed: false, older: null };
    }
    if (response.stopReason === SearchStopReason.MATCH_LIMIT) {
      return {
        kind: "matches",
        matches: found,
        truncated: true,
        failed: false,
        older: olderMatchPage(response, beforeRow, requestedEpoch, pages),
      };
    }
    if (response.stopReason === SearchStopReason.DEADLINE) {
      return { kind: "matches", matches: found, truncated: true, failed: true, older: null };
    }
    if (
      response.stopReason !== SearchStopReason.ROW_LIMIT
      || !searchContinuationIsValid(response, beforeRow)
    ) {
      return failedPartial();
    }
    if (found.length >= TERMINAL_SEARCH_MAX_MATCHES) {
      return {
        kind: "matches",
        matches: found,
        truncated: true,
        failed: false,
        older: olderMatchPage(response, beforeRow, requestedEpoch, pages),
      };
    }
    beforeRow = response.nextBeforeRow;
  }
}

/** A cursor is only worth keeping when it is valid, strictly older, and the
 *  query still has page budget left to spend on it. */
function olderMatchPage(
  response: SessionsSearchScrollbackResponse,
  beforeRow: bigint | undefined,
  epoch: string,
  pagesUsed: number,
): OlderMatchPage | null {
  if (response.nextBeforeRow === undefined) return null;
  if (!searchContinuationIsValid(response, beforeRow)) return null;
  if (pagesUsed >= TERMINAL_SEARCH_MAX_PAGES) return null;
  return { epoch, beforeRow: response.nextBeforeRow, pagesUsed };
}
