// Find-in-scrollback state machine for one pane: debounced query → one in-flight
// SessionsSearchScrollback per pane → highlights on the renderer → jump to the
// active match.
//
// The search runs on the WORKER's grid because the client holds at most
// MAX_HELD_SCROLLBACK_ROWS of the worker's retained history. A match can
// therefore name a row that is reserved-but-unpainted, so a jump first pulls that
// row in (ScrollbackBackfill.ensureRowPainted) and re-searches if the epoch moved.
//
// EPOCH FENCE. A match row means nothing outside the grid numbering it was found
// in: a core rebuild re-pins the scrollback origin, so a row from the previous
// epoch routinely falls back inside the new epoch's valid range while naming
// unrelated history. Every leg is therefore fenced — the request names the epoch
// this pane displays, the worker refuses to answer from any other, the reply
// stamps the epoch it served, and each hit carries that stamp so highlighting and
// jumping fail CLOSED. Dropping a stale hit costs the reader one keystroke;
// jumping to a row the index no longer names shows them someone else's output and
// calls it their match.
//
// Owner: CellTerminal (one controller per pane); the UI is TerminalFindBar.tsx.

import { createSignal } from "solid-js";
import { coordClient } from "../connect.ts";
import type { CellGridRenderer } from "./cellRenderer.ts";
import type { FindHit } from "./cellRow.ts";
import type { ScrollbackBackfill } from "./scrollbackBackfill.ts";

export const FIND_DEBOUNCE_MS = 300;
// What we ASK for; the worker clamps to its own ceiling and reports `truncated`.
// Sending 0 would be read as "zero matches", and the worker-leg frame schema
// requires a positive value.
export const FIND_MAX_MATCHES = 500;

export interface FindMatch {
  row: number;
  col: number;
  len: number;
  preview: string;
  /** Grid numbering `row` is an index into. Usable only while the pane's
   *  authoritative frame still names this epoch. */
  epoch: string;
}

export interface TerminalFind {
  open: () => boolean;
  query: () => string;
  matches: () => readonly FindMatch[];
  /** 1-based position of the active match, 0 when there is none. */
  index: () => number;
  truncated: () => boolean;
  /** A rejected regex, or a failed RPC — shown on the input, never as a toast. */
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

  /** Current grid numbering of this pane's authoritative frame, "" before the
   *  first frame lands. */
  function paneEpoch(): string {
    return opts.renderer()?.backfillAnchor()?.gridEpoch ?? "";
  }

  /** Install highlights, fenced on the epoch the hits were found in. One
   *  comparison covers the whole set: the worker never mixes two numberings into
   *  one reply, so a list belongs to exactly one epoch. Painting rows from a
   *  numbering the pane no longer holds would mark unrelated lines. */
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

  /** One search. `refence` is the budget for re-running against a grid that
   *  re-numbered under this attempt — exactly one re-search per user action, so a
   *  resize storm can never turn find into a polling loop. */
  async function run(refence = 1): Promise<void> {
    const q = query();
    const mine = ++token;
    if (q.length === 0) { clear(); return; }
    // Fence the scan to the numbering this pane DISPLAYS. "" only before the
    // first frame lands: the worker then binds the scan to its current epoch and
    // names it in the reply, which is what every hit is stamped with.
    const asked = paneEpoch();
    let res;
    try {
      res = await coordClient.sessionsSearchScrollback({
        sessionId: opts.sessionId,
        gridEpoch: asked,
        query: q,
        caseSensitive: caseSensitive(),
        regex: regex(),
        maxMatches: FIND_MAX_MATCHES,
      });
    } catch {
      if (disposed || mine !== token) return;
      // The worker refuses an epoch it no longer serves. When OUR epoch has moved
      // too the refusal is stale news — the pane reframed mid-flight — so ask
      // again about the grid now on screen instead of blaming the query.
      if (refence > 0 && paneEpoch() !== asked) { void run(refence - 1); return; }
      // A bad regex and an unreachable worker look the same to the user here:
      // the input tints and the count reads 0. Nothing is silently wrong.
      setMatches([]);
      setIndex(0);
      setFailed(true);
      publish([], 0);
      return;
    }
    if (disposed || mine !== token) return;
    // The worker scans NEWEST-first so truncation keeps the matches nearest the
    // live tail (and its viewport-row segment lands last). Re-sort into reading
    // order here so "next" walks DOWN the history like any editor, then start on
    // the LAST entry — the newest match, closest to where the reader already is,
    // which also avoids an immediate deep backfill pull.
    const epoch = res.gridEpoch;
    const list: FindMatch[] = res.matches
      .map((m) => ({ row: Number(m.row), col: m.col, len: m.len, preview: m.preview, epoch }))
      .sort((a, b) => (a.row - b.row) || (a.col - b.col));
    setMatches(list);
    setTruncated(res.truncated);
    setFailed(false);
    const active = list.length;
    setIndex(active);
    publish(list, active);
    if (active > 0) void reveal(list[active - 1]!, refence);
  }

  /** Drop a result set whose numbering the pane no longer holds. Every hit in it
   *  shares that epoch, so one stale hit condemns the set; clearing also takes
   *  the highlights down, which is the only way stale marks stop being painted.
   *  Spends the caller's `refence` budget on re-asking the same question about
   *  the grid now on screen. */
  function invalidate(refence: number): void {
    setMatches([]);
    setIndex(0);
    setTruncated(false);
    publish([], 0);
    if (refence > 0 && query().length > 0) void run(refence - 1);
  }

  /** Land the reader on a match. Fails CLOSED on a re-numbered grid: a hit found
   *  in another epoch names a row that no longer exists as such, and after a
   *  rebuild re-pins the scrollback origin it frequently lands INSIDE the new
   *  valid range — so `row < total` proves nothing on its own. A row inside the
   *  unpainted [0, sbBase) region is pulled in first; that pull awaits, so both
   *  the search token and the epoch are re-checked before the one owned scroll
   *  write — a newer query must not be dragged to an older query's row. */
  async function reveal(match: FindMatch, refence: number): Promise<void> {
    const mine = token;
    const renderer = opts.renderer();
    if (!renderer) return;
    const anchor = renderer.backfillAnchor();
    if (!anchor) return;
    if (anchor.gridEpoch !== match.epoch) { invalidate(refence); return; }
    // Alt-screen and live-viewport matches are already on screen; there is
    // nothing above them to scroll to.
    if (match.row >= anchor.total) return;
    if (match.row < anchor.sbBase) {
      const ok = await opts.backfill()?.ensureRowPainted(match.row);
      if (disposed || mine !== token) return;
      // The pull gave up: either the row is genuinely evicted or the page it
      // needed was rejected. Re-search once, then stop — never loop.
      if (!ok) { if (refence > 0) void run(refence - 1); return; }
      if (paneEpoch() !== match.epoch) { invalidate(refence); return; }
    }
    opts.renderer()?.scrollToScrollbackRow(match.row);
  }

  function schedule(): void {
    clearTimeout(debounce ?? undefined);
    debounce = setTimeout(() => { debounce = null; void run(); }, FIND_DEBOUNCE_MS);
  }

  return {
    open, query, matches, index, truncated, failed, caseSensitive, regex,
    openFind(): void { setOpen(true); },
    closeFind(): void {
      setOpen(false);
      if (debounce) { clearTimeout(debounce); debounce = null; }
      token++;
      setQueryRaw("");
      clear();
    },
    setQuery(next: string): void {
      setQueryRaw(next);
      if (next.length === 0) { token++; clear(); return; }
      schedule();
    },
    toggleCaseSensitive(): void { setCaseSensitive((v) => !v); if (query()) schedule(); },
    toggleRegex(): void { setRegex((v) => !v); if (query()) schedule(); },
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
      token++;
      if (debounce) { clearTimeout(debounce); debounce = null; }
    },
  };
}
