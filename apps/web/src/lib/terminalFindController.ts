// Bounded find-in-scrollback state for one terminal pane.
// A debounced cancellable page chain accumulates newest-first matches within
// one grid epoch; deep matches backfill before reveal. CellTerminal owns it.

import { createSignal } from "solid-js";
import { diag } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";
import type { CellGridRenderer } from "./cellRenderer.ts";
import type { FindHit } from "./cellRow.ts";
import type { FindMatch } from "./terminalFindPaging.ts";
import {
  runTerminalFindPageChain,
  type OlderMatchPage,
} from "./terminalFindPageChain.ts";
import {
  preferredTerminalFindIndex,
  type TerminalFindPreferredMatch,
  type TerminalFindQueryOptions,
} from "./terminalFindHandoff.ts";
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
  setQuery(next: string, options?: TerminalFindQueryOptions): void;
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
  let preferredMatch: TerminalFindPreferredMatch | null = null;
  // Cursor onto matches older than the published window, set whenever a page
  // stopped at the match cap. Stepping back past the oldest match spends it.
  let olderPage: OlderMatchPage | null = null;

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
    olderPage = null;
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
    const active = preferredTerminalFindIndex(list, paneEpoch(), preferredMatch);
    preferredMatch = null;
    setIndex(active);
    publish(list, active);
    if (active > 0) void reveal(list[active - 1]!, epochRetryBudget);
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

  /** Choose the match the next publication activates: the newest of a freshly
   *  slid page, or the one the user was already parked on when it added none. */
  function preferNewestOf(
    slid: readonly FindMatch[],
    parked: readonly FindMatch[],
  ): void {
    let choice: FindMatch | null = null;
    for (const match of slid) {
      if (choice === null || match.row > choice.row) choice = match;
    }
    choice ??= parked[0] ?? null;
    preferredMatch = choice === null
      ? null
      : { gridEpoch: choice.epoch, row: BigInt(choice.row), col: choice.col };
  }

  /** Run one chain; the retry budget covers one grid re-numbering through
   *  reveal. `resume` slides onto rows a match cap left unscanned, keeping the
   *  matches already published under it. */
  async function searchNow(
    epochRetryBudget = 1,
    resume: OlderMatchPage | null = null,
  ): Promise<void> {
    const q = query();
    const carried = resume === null ? [] : matches();
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
    try {
      const outcome = await runTerminalFindPageChain({
        sessionId: opts.sessionId,
        searchId,
        query: q,
        caseSensitive: caseSensitive(),
        regex: regex(),
        epoch: resume?.epoch ?? paneEpoch(),
        resume,
        signal: cancellation.signal,
        paneEpoch,
        current: () => currentSearch(mine, cancellation),
      });
      if (outcome.kind === "abandoned") return;
      if (!currentSearch(mine, cancellation)) return;
      if (outcome.kind === "epoch-changed") {
        retryAfterEpochChange(mine, cancellation, epochRetryBudget);
        return;
      }
      olderPage = outcome.older;
      const list = [...carried, ...outcome.matches];
      if (resume !== null) preferNewestOf(outcome.matches, carried);
      installResult(
        list,
        outcome.truncated || (outcome.failed && list.length > 0),
        outcome.failed,
        epochRetryBudget,
      );
    } finally {
      if (activeSearch?.controller === cancellation) activeSearch = null;
    }
  }

  /** Spend the older-rows cursor a match cap handed back, so a needle with
   *  more hits than one page holds stays fully navigable. */
  async function extendOlderMatches(): Promise<void> {
    const page = olderPage;
    if (page === null) return;
    // Consumed up front so a second keypress cannot start the same page twice.
    olderPage = null;
    diag("scrollback.find_slide_older", {
      sid: opts.sessionId,
      before_row: Number(page.beforeRow),
      pages_used: page.pagesUsed,
      held_matches: matches().length,
    });
    await searchNow(1, page);
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
      preferredMatch = null;
      clear();
    },
    setQuery(next: string, options: TerminalFindQueryOptions = {}): void {
      preferredMatch = options.preferredMatch ?? null;
      if (options.literal) {
        setRegex(false);
        setCaseSensitive(options.caseSensitive ?? false);
      }
      setQueryRaw(next);
      if (next.length === 0) {
        if (debounce) { clearTimeout(debounce); debounce = null; }
        stopActiveSearch();
        clear();
        return;
      }
      schedule();
    },
    toggleCaseSensitive(): void {
      preferredMatch = null;
      setCaseSensitive((value) => !value);
      if (query()) schedule();
    },
    toggleRegex(): void {
      preferredMatch = null;
      setRegex((value) => !value);
      if (query()) schedule();
    },
    step(delta: number): void {
      const list = matches();
      if (list.length === 0) return;
      // The published window ends at the oldest match a capped page reached,
      // so stepping back past it fetches older rows instead of wrapping.
      if (delta < 0 && index() === 1 && olderPage !== null) {
        void extendOlderMatches();
        return;
      }
      const next = ((index() - 1 + delta) % list.length + list.length) % list.length;
      setIndex(next + 1);
      publish(list, next + 1);
      void reveal(list[next]!, 1);
    },
    dispose(): void {
      disposed = true;
      preferredMatch = null;
      if (debounce) { clearTimeout(debounce); debounce = null; }
      stopActiveSearch();
    },
  };
}

