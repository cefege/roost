// Bounded find-in-scrollback state for one terminal pane.
// A debounced cancellable page chain accumulates newest-first matches within
// one grid epoch; deep matches backfill before reveal. CellTerminal owns it.

import { createSignal } from "solid-js";
import { SearchStopReason } from "@roost/shared/proto/coordinator_pb";
import { TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_PAGES, TERMINAL_SEARCH_MAX_ROWS } from "@roost/shared/terminal-search";
import { diag } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";
import type { CellGridRenderer } from "./cellRenderer.ts";
import type { FindHit } from "./cellRow.ts";
import {
  decodePageMatches,
  searchContinuationIsValid,
  searchPageRangeIsValid,
  type FindMatch,
} from "./terminalFindPaging.ts";
import type { ScrollbackBackfill } from "./scrollbackBackfill.ts";

export type { FindMatch } from "./terminalFindPaging.ts";
export const FIND_DEBOUNCE_MS = 300;

export interface TerminalFind {
  open: () => boolean;
  query: () => string;
  matches: () => readonly FindMatch[];
  /** 1-based position of the active match, 0 when there is none. */
  index: () => number;
  truncated: () => boolean;
  /** Invalid/incomplete search — shown on the input, never as a toast. */
  failed: () => boolean;
  caseSensitive: () => boolean;
  regex: () => boolean;
  openFind(): void;
  closeFind(): void;
  setQuery(next: string): void;
  toggleCaseSensitive(): void;
  toggleRegex(): void;
  step(delta: number): void;
  dispose(): void;
}

export function createTerminalFind(opts: {
  sessionId: string;
  renderer: () => CellGridRenderer | null;
  backfill: () => ScrollbackBackfill | null;
}): TerminalFind {
  const [open, setOpen] = createSignal(false);
  const [query, setQueryRaw] = createSignal("");
  const [matches, setMatches] = createSignal<readonly FindMatch[]>([]);
  const [index, setIndex] = createSignal(0);
  const [truncated, setTruncated] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [caseSensitive, setCaseSensitive] = createSignal(false);
  const [regex, setRegex] = createSignal(false);

  let debounce: ReturnType<typeof setTimeout> | null = null;
  // Monotonic token: only the newest search may publish. A stale response
  // otherwise overwrites highlights the user has already moved past.
  let token = 0;
  let disposed = false;
  let activeSearch: { controller: AbortController; searchId: string } | null = null;

  /** Current grid numbering of this pane's authoritative frame, "" before the
   *  first frame lands. */
  function paneEpoch(): string {
    return opts.renderer()?.backfillAnchor()?.gridEpoch ?? "";
  }

  /** Paint only hits owned by the pane's current grid numbering. */
  function publish(list: readonly FindMatch[], active: number): void {
    const renderer = opts.renderer();
    if (!renderer) return;
    const epoch = list[0]?.epoch;
    if (epoch !== undefined && epoch !== paneEpoch()) {
      renderer.setFindHighlights(new Map(), null);
      return;
    }
    const hits = new Map<number, FindHit[]>();
    for (const m of list) {
      const existing = hits.get(m.row);
      if (existing) existing.push({ col: m.col, len: m.len });
      else hits.set(m.row, [{ col: m.col, len: m.len }]);
    }
    const activeMatch = active > 0 ? list[active - 1] : undefined;
    renderer.setFindHighlights(
      hits,
      activeMatch ? { row: activeMatch.row, col: activeMatch.col } : null,
    );
  }

  function clear(): void {
    setMatches([]);
    setIndex(0);
    setTruncated(false);
    setFailed(false);
    publish([], 0);
  }

  function currentSearch(mine: number, cancellation: AbortController): boolean {
    return !disposed && mine === token && !cancellation.signal.aborted;
  }

  function cancelActiveSearch(): void {
    const active = activeSearch;
    if (!active) return;
    activeSearch = null;
    active.controller.abort();
    void coordClient.sessionsCancelScrollbackSearch({
      sessionId: opts.sessionId,
      searchId: active.searchId,
    }).catch((error) => {
      diag("scrollback.search_cancel_failed", {
        sid: opts.sessionId,
        error: String(error),
      });
    });
  }

  function stopActiveSearch(): void {
    token++;
    cancelActiveSearch();
  }

  /** Convert the worker's newest-first traversal to UI reading order. */
  function installResult(
    newestFirst: readonly FindMatch[],
    incomplete: boolean,
    didFail: boolean,
    epochRetryBudget: number,
  ): void {
    const list = [...newestFirst]
      .sort((left, right) => (left.row - right.row) || (left.col - right.col));
    setMatches(list);
    setTruncated(incomplete);
    setFailed(didFail);
    const active = list.length;
    setIndex(active);
    publish(list, active);
    if (active > 0) void reveal(list[active - 1]!, epochRetryBudget);
  }

  function installFailedPartial(newestFirst: readonly FindMatch[], epochRetryBudget: number): void {
    installResult(newestFirst, newestFirst.length > 0, true, epochRetryBudget);
  }

  function retryAfterEpochChange(
    mine: number,
    cancellation: AbortController,
    epochRetryBudget: number,
  ): void {
    if (!currentSearch(mine, cancellation)) return;
    clear();
    if (epochRetryBudget > 0 && query().length > 0) {
      void searchNow(epochRetryBudget - 1);
      return;
    }
    setFailed(true);
  }

  /** Run one chain; the retry budget covers one grid re-numbering through reveal. */
  async function searchNow(epochRetryBudget = 1): Promise<void> {
    const q = query();
    cancelActiveSearch();
    const mine = ++token;
    if (q.length === 0) {
      activeSearch = null;
      clear();
      return;
    }

    const cancellation = new AbortController();
    const searchId = crypto.randomUUID();
    activeSearch = { controller: cancellation, searchId };
    const initialEpoch = paneEpoch();
    let requestedEpoch = initialEpoch;
    let beforeRow: bigint | undefined;
    const newestFirst: FindMatch[] = [];
    let pages = 0;
    const paneAcceptsEpoch = (epoch: string): boolean => (
      paneEpoch() === epoch || (initialEpoch === "" && paneEpoch() === "" && epoch !== "")
    );

    try {
      for (;;) {
        if (!currentSearch(mine, cancellation)) return;
        if (pages >= TERMINAL_SEARCH_MAX_PAGES) {
          installFailedPartial(newestFirst, epochRetryBudget);
          return;
        }
        if (!paneAcceptsEpoch(requestedEpoch)) {
          retryAfterEpochChange(mine, cancellation, epochRetryBudget);
          return;
        }

        const remainingMatches = TERMINAL_SEARCH_MAX_MATCHES - newestFirst.length;
        if (remainingMatches <= 0) {
          installResult(newestFirst, true, false, epochRetryBudget);
          return;
        }
        const res = await coordClient.sessionsSearchScrollback({
          sessionId: opts.sessionId,
          searchId,
          gridEpoch: requestedEpoch,
          query: q,
          caseSensitive: caseSensitive(),
          regex: regex(),
          maxRows: TERMINAL_SEARCH_MAX_ROWS,
          maxMatches: remainingMatches,
          ...(beforeRow === undefined ? {} : { beforeRow }),
        }, { signal: cancellation.signal });
        pages++;

        if (!currentSearch(mine, cancellation)) return;
        if (res.stopReason === SearchStopReason.EPOCH_CHANGED) {
          retryAfterEpochChange(mine, cancellation, epochRetryBudget);
          return;
        }
        if (requestedEpoch === "") {
          if (res.gridEpoch === "") {
            installFailedPartial(newestFirst, epochRetryBudget);
            return;
          }
          requestedEpoch = res.gridEpoch;
        } else if (res.gridEpoch !== requestedEpoch) {
          retryAfterEpochChange(mine, cancellation, epochRetryBudget);
          return;
        }
        if (!paneAcceptsEpoch(requestedEpoch)) {
          retryAfterEpochChange(mine, cancellation, epochRetryBudget);
          return;
        }
        if (!searchPageRangeIsValid(res, beforeRow)) {
          installFailedPartial(newestFirst, epochRetryBudget);
          return;
        }

        const pageMatches = decodePageMatches(res, requestedEpoch);
        if (pageMatches === null || pageMatches.length > remainingMatches) {
          installFailedPartial(newestFirst, epochRetryBudget);
          return;
        }
        newestFirst.push(...pageMatches);

        if (
          res.stopReason !== SearchStopReason.ROW_LIMIT
          && res.nextBeforeRow !== undefined
        ) {
          installFailedPartial(newestFirst, epochRetryBudget);
          return;
        }
        if (res.stopReason === SearchStopReason.COMPLETE) {
          installResult(newestFirst, false, false, epochRetryBudget);
          return;
        }
        if (res.stopReason === SearchStopReason.MATCH_LIMIT) {
          installResult(
            newestFirst.slice(0, TERMINAL_SEARCH_MAX_MATCHES),
            true, false, epochRetryBudget,
          );
          return;
        }
        if (res.stopReason === SearchStopReason.DEADLINE) {
          installResult(newestFirst, true, true, epochRetryBudget);
          return;
        }
        if (
          res.stopReason !== SearchStopReason.ROW_LIMIT
          || !searchContinuationIsValid(res, beforeRow)
        ) {
          installFailedPartial(newestFirst, epochRetryBudget);
          return;
        }
        if (newestFirst.length >= TERMINAL_SEARCH_MAX_MATCHES) {
          installResult(newestFirst, true, false, epochRetryBudget);
          return;
        }
        beforeRow = res.nextBeforeRow;
      }
    } catch {
      if (!currentSearch(mine, cancellation)) return;
      if (!paneAcceptsEpoch(requestedEpoch)) {
        retryAfterEpochChange(mine, cancellation, epochRetryBudget);
        return;
      }
      installFailedPartial(newestFirst, epochRetryBudget);
    } finally {
      if (activeSearch?.controller === cancellation) activeSearch = null;
    }
  }

  /** Drop stale numbering, then spend the one retry against the live pane. */
  function invalidate(epochRetryBudget: number): void {
    clear();
    if (epochRetryBudget > 0 && query().length > 0) {
      void searchNow(epochRetryBudget - 1);
    } else {
      setFailed(true);
    }
  }

  /** Reveal only inside the match's epoch. A deep row is pulled in first, with
   *  token and epoch rechecked after that await so stale work cannot scroll. */
  async function reveal(match: FindMatch, epochRetryBudget: number): Promise<void> {
    const mine = token;
    const renderer = opts.renderer();
    if (!renderer) return;
    const anchor = renderer.backfillAnchor();
    if (!anchor) return;
    if (anchor.gridEpoch !== match.epoch) { invalidate(epochRetryBudget); return; }
    // Viewport matches need no scrollback jump.
    if (match.row >= anchor.total) return;
    if (match.row < anchor.sbBase) {
      const ok = await opts.backfill()?.ensureRowPainted(match.row);
      if (disposed || mine !== token) return;
      // A rejected/evicted pull gets at most the remaining epoch retry.
      if (!ok) {
        if (epochRetryBudget > 0) void searchNow(epochRetryBudget - 1);
        return;
      }
      if (paneEpoch() !== match.epoch) { invalidate(epochRetryBudget); return; }
    }
    opts.renderer()?.scrollToScrollbackRow(match.row);
  }

  function schedule(): void {
    clearTimeout(debounce ?? undefined);
    stopActiveSearch();
    debounce = setTimeout(() => {
      debounce = null;
      void searchNow();
    }, FIND_DEBOUNCE_MS);
  }

  return {
    open, query, matches, index, truncated, failed, caseSensitive, regex,
    openFind(): void { setOpen(true); },
    closeFind(): void {
      setOpen(false);
      if (debounce) { clearTimeout(debounce); debounce = null; }
      stopActiveSearch();
      setQueryRaw("");
      clear();
    },
    setQuery(next: string): void {
      setQueryRaw(next);
      if (next.length === 0) {
        if (debounce) { clearTimeout(debounce); debounce = null; }
        stopActiveSearch();
        clear();
        return;
      }
      schedule();
    },
    toggleCaseSensitive(): void { setCaseSensitive((value) => !value); if (query()) schedule(); },
    toggleRegex(): void { setRegex((value) => !value); if (query()) schedule(); },
    step(delta: number): void {
      const list = matches();
      if (list.length === 0) return;
      const next = ((index() - 1 + delta) % list.length + list.length) % list.length;
      setIndex(next + 1);
      publish(list, next + 1);
      void reveal(list[next]!, 1);
    },
    dispose(): void {
      disposed = true;
      if (debounce) { clearTimeout(debounce); debounce = null; }
      stopActiveSearch();
    },
  };
}

