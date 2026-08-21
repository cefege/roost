// Clickable links overlay for the cell renderer. OSC 8 producer hyperlinks are
// NOT discovered here: the core authors them per cell and cellRow.ts paints them
// as anchors at exactly those cells. This module owns the two INFERRED
// categories — regex URLs and file paths — and the arm / hover / click behaviour
// every anchor shares, painted or wrapped. It attaches a MutationObserver to
// the rendered row container, scans the rows the renderer replaced, and wraps
// inferred matches in <a> elements.
//
// URL regex is adapted verbatim from a terminal emulator's default regex
// (Oniguruma → ECMAScript).
// Same scheme list, same `(?<![,.])` trailing-punctuation lookbehind,
// same `[(\[]\w*[)\]]` bracketed-suffix branch so URLs with a
// matching paren pair stay intact (Wikipedia-style trailing `_(disambig)`).
//
// Modifier-gated activation mirrors a common terminal-emulator highlight
// mode: bare clicks fall through to text
// selection; Meta-down toggles a root attribute that flips the anchors
// to `pointer-events: auto` via the .wterm-link CSS rule. cmd/ctrl
// click then opens through the native <a> — middle-click + accessibility
// come for free.

import { computeRowLinks } from "./terminal-links.detect.ts";
import type {
  PaintedLink, ResolveFile, RowLinkInput, RowLinkSegment,
} from "./terminal-links.detect.ts";
import {
  LINK_KEY_ATTR, ROW_COLUMNS_ATTR, ROW_HAS_LINKS_ATTR,
  TERMINAL_LINK_CLASS as LINK_CLASS,
} from "../lib/cellRow.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
import { terminalLinkModifierKey } from "../lib/browserPlatform.ts";

// Re-exported so the public import path (CellTerminal, terminal-links.test.ts)
// resolves unchanged after the pure detection moved to ./terminal-links.detect.ts.
export { computeRowLinks };
export type { PaintedLink, ResolveFile, RowLinkInput, RowLinkSegment };

// The canonical cell renderer emits one .cell-row per terminal row.
const ROW_SELECTOR = ".cell-row";

// Marks a row whose inferred-link scan has already run. NOT "this row has an
// anchor": rows arrive from the renderer already carrying painted OSC 8 anchors,
// and those must not suppress the row's regex / file-path scan.
const SCANNED_ATTR = "data-linkified";

// CSS rule lives in apps/web/src/styles/sidebar.css alongside the
// .wterm container styles; we inject it lazily on first attach so
// callers don't have to remember.
const CSS_INJECTED = Symbol.for("roost.wterm-link.css");
function _injectCssOnce(): void {
  if ((globalThis as Record<symbol, unknown>)[CSS_INJECTED]) return;
  (globalThis as Record<symbol, unknown>)[CSS_INJECTED] = true;
  const style = document.createElement("style");
  style.setAttribute("data-roost", "wterm-link");
  style.textContent = `
.${LINK_CLASS} {
  color: inherit;
  text-decoration: none;
  pointer-events: none;
  cursor: text;
}
.wterm[data-link-armed="1"] .${LINK_CLASS} {
  pointer-events: auto;
  text-decoration: underline;
  text-underline-offset: 2px;
  cursor: pointer;
}
/* File links pick up the accent so they read as "opens in Roost", not the web. */
.wterm[data-link-armed="1"] .${LINK_CLASS}[data-kind="file"] {
  text-decoration-color: var(--md-primary, currentColor);
}
.wterm-link-hint {
  position: fixed;
  z-index: 2147483000;
  display: none;
  max-width: 60vw;
  padding: 3px 8px;
  border-radius: var(--md-shape-sm, 6px);
  background: var(--surface-2);
  color: var(--text-hi);
  border: 1px solid var(--border-subtle);
  box-shadow: var(--md-elev-3);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  line-height: 1.4;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  pointer-events: none;
}
`;
  document.head.appendChild(style);
}

/** A painted anchor plus the element itself, so a loser can be dissolved. */
type PaintedAnchor = PaintedLink & { el: HTMLElement };

// Painted anchors are DIRECT children of the row (cellRow.ts appends them to the
// row element), so summing child text lengths in order yields their exact
// row-local code-unit offsets with no tree walk. The ROW_HAS_LINKS_ATTR gate
// keeps ordinary rows — the overwhelming majority, and every row of a full
// scan — on one O(1) attribute read.
function _paintedLinks(row: HTMLElement): PaintedAnchor[] | undefined {
  if (!row.hasAttribute(ROW_HAS_LINKS_ATTR)) return undefined;
  const out: PaintedAnchor[] = [];
  let offset = 0;
  for (const child of row.childNodes) {
    const len = (child.textContent ?? "").length;
    if (child instanceof HTMLElement && child.hasAttribute(LINK_KEY_ATTR)) {
      out.push({
        el: child,
        start: offset,
        end: offset + len,
        uri: child.getAttribute("href") ?? "",
        key: child.getAttribute(LINK_KEY_ATTR) ?? "",
      });
    }
    offset += len;
  }
  return out;
}

/** The row's painted GRID OCCUPANCY, or -1 when it cannot be read. -1 never
 *  equals `cols`, so an unstamped row simply never soft-wrap-joins — the
 *  conservative answer, because a bogus join fabricates links across unrelated
 *  rows. NEVER textContent.length: that counts UTF-16 code units, and one
 *  column is neither (2 columns per CJK ideograph, 2 per ZWJ emoji cluster). */
function _rowColumns(row: HTMLElement): number {
  const stamped = row.getAttribute(ROW_COLUMNS_ATTR);
  if (stamped === null) return -1;
  const columns = Number.parseInt(stamped, 10);
  return Number.isInteger(columns) ? columns : -1;
}

// DOM applier: run the pure algorithm over the rows' text, painted links and
// column occupancy, then wrap each row-local segment in an <a>. Text spans
// crossing the renderer's per-cell <span>s collapse into one <a>; <a> uses
// `color: inherit` so cells keep their colors via the inner spans.
function _linkifyRows(rows: HTMLElement[], cols: number, resolveFile?: ResolveFile, githubOwnerRepo?: string): void {
  const painted: Array<PaintedAnchor[] | undefined> = [];
  const inputs: RowLinkInput[] = [];
  for (const row of rows) {
    const links = _paintedLinks(row);
    painted.push(links);
    inputs.push({ text: row.textContent ?? "", columns: _rowColumns(row), links });
  }
  const segments = computeRowLinks(inputs, cols, resolveFile, githubOwnerRepo);
  if (segments.length === 0) return;
  // Group segments by row so each row's nodes are walked once.
  const byRow = new Map<number, RowLinkSegment[]>();
  for (const seg of segments) {
    const list = byRow.get(seg.row);
    if (list) list.push(seg); else byRow.set(seg.row, [seg]);
  }
  for (const [rowIdx, segs] of byRow) {
    const row = rows[rowIdx];
    // Idempotency guard. Our own extractContents mutates row DOM which
    // re-fires the MutationObserver on the next rAF; without this we'd nest
    // <a> in <a> every frame. The renderer replaces a row element outright on
    // any real update, so a row still carrying this mark is already scanned and
    // unchanged since.
    if (row.hasAttribute(SCANNED_ATTR)) continue;
    // A painted producer link that LOST to an overlapping inferred match (the
    // `ls --hyperlink` file:/// → resolvable /file/… case) is absent from
    // `segments`, so the winner's range still contains its anchor and
    // _wrapRange would nest <a> in <a>. Dissolve it first; its Text nodes
    // survive the unwrap in place, so the offsets computed below stay exact.
    for (const link of painted[rowIdx] ?? []) {
      if (!segs.some((s) => s.start < link.end && link.start < s.end)) continue;
      link.el.replaceWith(...Array.from(link.el.childNodes));
    }
    const nodes: Array<{ node: Text; start: number; end: number }> = [];
    let offset = 0;
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const t = n as Text;
      nodes.push({ node: t, start: offset, end: offset + t.data.length });
      offset += t.data.length;
    }
    // Reverse (segs already ascending by start) so earlier offsets stay valid
    // as this row's nodes split.
    for (let i = segs.length - 1; i >= 0; i--) {
      _wrapRange(row, nodes, segs[i].start, segs[i].end, segs[i].url, segs[i].kind, segs[i].hint);
    }
    row.setAttribute(SCANNED_ATTR, "1");
  }
}

function _wrapRange(
  row: HTMLElement,
  nodes: Array<{ node: Text; start: number; end: number }>,
  start: number,
  end: number,
  url: string,
  kind?: "file",
  hint?: string,
): void {
  // Find the first and last text node overlapping (start, end).
  const startNode = nodes.find(n => n.end > start && n.start <= start);
  const endNode = nodes.find(n => n.end >= end && n.start < end);
  if (!startNode || !endNode) return;
  const range = document.createRange();
  const startOffset = start - startNode.start;
  const endOffset = end - endNode.start;
  // Bounds-check against the CURRENT live DOM length, not the stale
  // recorded .end — a cell frame applying mid-linkify mutates the Text
  // node, shrinking .length below the computed offset. Without this
  // guard, setEnd/setStart throws IndexSizeError → spa.uncaught (234
  // crashes/day in production logs). Bail = link skipped, next frame
  // re-scans (same behavior as the !startNode/!endNode guard above).
  if (startOffset < 0 || startOffset > startNode.node.length) return;
  if (endOffset < 0 || endOffset > endNode.node.length) return;
  range.setStart(startNode.node, startOffset);
  range.setEnd(endNode.node, endOffset);
  const anchor = document.createElement("a");
  anchor.className = LINK_CLASS;
  anchor.href = url;
  anchor.tabIndex = -1;
  if (hint) anchor.dataset.hint = hint;
  if (kind === "file") {
    // Internal file-viewer nav — intercepted by the container click handler
    // (opens the FileViewerSheet via the router, not a new browser tab).
    anchor.dataset.kind = "file";
  } else {
    anchor.target = "_blank";
    anchor.rel = "noopener noreferrer";
    anchor.dataset.hint = anchor.dataset.hint ?? url;
  }
  try {
    // surroundContents only works when start and end are the SAME text node.
    // A match crossing a mid-string style change spans multiple <span>s
    // (rowToSpans opens a new span per SGR change), so surroundContents throws
    // InvalidStateError and the link is lost. extractContents pulls the
    // multi-node range out (splitting the boundary text nodes, cloning the
    // partially-covered spans) into a fragment; insertNode drops the anchor in
    // its place. The inner spans ride along inside the <a>, and `.wterm-link`
    // has `color: inherit` so their colors survive.
    if (startNode.node === endNode.node) {
      range.surroundContents(anchor);
    } else {
      anchor.appendChild(range.extractContents());
      range.insertNode(anchor);
    }
  } catch {
    // Defensive: an unexpected boundary state — bail, next frame re-scans.
  }
  // `row` is unused after the wrap but kept in the signature for symmetry
  // with future hit-test-style fallbacks.
  void row;
}

/** Attachment controls for a terminal linkifier instance. */
export interface TerminalLinkAttachment {
  releaseInteraction(): void;
  dispose(): void;
}

/** Attach the linkifier to a wterm container. */
export interface TerminalLinkOpts {
  /** Resolve a file path from output → internal `/file/…` href (or null). */
  resolveFile?: ResolveFile;
  /** Open an internal file href via the router (not a new browser tab). */
  onOpenFile?: (href: string) => void;
  /** Session's GitHub "owner/repo" getter — enables bare #N / commit-SHA links.
   *  A getter (not a value) so the scan picks it up when it resolves post-mount. */
  githubOwnerRepo?: () => string | undefined;
  /** Called with `true` while the link modifier is held AND the pointer is over
   *  the terminal (i.e. the user is about to Cmd-click a link), `false` otherwise.
   *  CellTerminal wires this to the renderer's repaint-hold so the live viewport
   *  stops rebuilding rows — otherwise the just-wrapped <a> is destroyed every
   *  16ms frame and the cursor flickers pointer↔text under the hover. */
  onArmedHoverChange?: (active: boolean) => void;
}

export function attachTerminalLinks(
  container: HTMLElement,
  opts: TerminalLinkOpts = {},
): TerminalLinkAttachment {
  _injectCssOnce();

  // Modifier-gated activation. The centralized platform map keeps the same
  // Command-on-macOS / Control-elsewhere behavior used by shortcut labels.
  const modKey = terminalLinkModifierKey();
  // Repaint-hold coordination. The pane's cell renderer rebuilds every visible
  // row each frame (replaceChildren), destroying our wrapped <a> — so while the
  // user Cmd-hovers a link the cursor flickers pointer↔text as the anchor comes
  // and goes. We ask the renderer to freeze repaints, but only while armed AND
  // the pointer is over the pane (the brief moment before a Cmd-click); a Cmd
  // press for any other reason must not stall live output.
  let armed = false;
  let pointerInside = false;
  let holding = false;
  const recomputeHold = (): void => {
    const next = armed && pointerInside;
    if (next === holding) return;
    holding = next;
    opts.onArmedHoverChange?.(next);
    // Freeze only when pointer is inside (user is about to click a link).
    // But always force a full scan when armed flips on — rows may have
    // been rebuilt since the last scan, and the next scan (rAF) must see
    // the latest text before the renderer freezes.
    if (armed) { fullScanNeeded = true; scheduleScan(); }
  };
  const onKeyDown = (e: KeyboardEvent): void => {
    if (e.key !== modKey) return;
    container.setAttribute("data-link-armed", "1");
    armed = true;
    recomputeHold();
  };
  const onKeyUp = (e: KeyboardEvent): void => {
    if (e.key !== modKey) return;
    container.removeAttribute("data-link-armed");
    armed = false;
    recomputeHold();
  };
  // Floating hint: while armed, hovering a link shows where it
  // goes ("Open foo.ts:42" for files, the URL for links) so a click is never a
  // surprise. One shared element, positioned under the hovered anchor.
  let hintEl: HTMLDivElement | null = null;
  const hideHint = (): void => { if (hintEl) hintEl.style.display = "none"; };
  const releaseInteraction = (): void => {
    container.removeAttribute("data-link-armed");
    armed = false;
    pointerInside = false;
    recomputeHold();
    hideHint();
  };
  const onBlur = releaseInteraction;
  const onPointerEnter = (): void => { pointerInside = true; recomputeHold(); };
  const onPointerLeave = (): void => { pointerInside = false; recomputeHold(); };
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("blur", onBlur);
  container.addEventListener("mouseenter", onPointerEnter);
  container.addEventListener("mouseleave", onPointerLeave);

  const showHint = (anchor: HTMLElement): void => {
    const text = anchor.dataset.hint;
    if (!text) return;
    if (!hintEl) {
      hintEl = document.createElement("div");
      hintEl.className = "wterm-link-hint";
      document.body.appendChild(hintEl);
    }
    hintEl.textContent = text;
    const r = anchor.getBoundingClientRect();
    hintEl.style.left = `${Math.round(r.left)}px`;
    hintEl.style.top = `${Math.round(r.bottom + 4)}px`;
    hintEl.style.display = "block";
  };
  const onOver = (e: MouseEvent): void => {
    if (container.getAttribute("data-link-armed") !== "1") return;
    const a = (e.target as Element | null)?.closest?.("a." + LINK_CLASS) as HTMLElement | null;
    if (a) showHint(a); else hideHint();
  };
  const onOut = (e: MouseEvent): void => {
    if ((e.target as Element | null)?.closest?.("a." + LINK_CLASS)) hideHint();
  };
  // Intercept clicks on file links (armed → pointer-events:auto → anchor is the
  // target) and route them into Roost's file viewer instead of a browser nav.
  const onClick = (e: MouseEvent): void => {
    const a = (e.target as Element | null)?.closest?.(`a.${LINK_CLASS}[data-kind="file"]`) as HTMLAnchorElement | null;
    if (!a) return; // url links fall through to the native <a target=_blank>
    e.preventDefault();
    const href = a.getAttribute("href");
    if (href && opts.onOpenFile) opts.onOpenFile(href);
    hideHint();
  };
  container.addEventListener("mouseover", onOver);
  container.addEventListener("mouseout", onOut);
  container.addEventListener("click", onClick);

  // Coalesce mutations into one rAF-scoped scan per frame, mirroring
  // a per-frame linkifier. MutationObserver
  // fires synchronously inside the microtask queue; rAF defers work to
  // after the browser has applied wterm's row writes.
  //
  // INCREMENTAL: the cell renderer diffs row-by-row (cellRenderer.ts
  // renderViewport), so mutation records name exactly the rows that changed —
  // linkify only those (plus their soft-wrap group) instead of regex-scanning
  // every scrollback row on every frame (was 55% of total main-thread CPU on
  // a streaming pane). Rows persist across frames now, so their anchors
  // persist too; a replaced row arrives anchor-free and gets re-linkified.
  let scanScheduled = false;
  // Pending scan's handle plus WHICH scheduler issued it, so the disposer and
  // the visibilitychange recovery cancel the right one.
  let scanHandle = 0;
  let scanHandleIsIdle = false;
  let fullScanNeeded = true; // initial attach pass
  const dirtyRows = new Set<HTMLElement>();
  const DIRTY_LIMIT = 300; // structural rebuild → cheaper to rescan everything

  // requestIdleCallback keeps the second DOM pass off the frame the renderer is
  // painting. WebKit has no rIC (playwright.config.ts runs webkit-iphone), so
  // rAF stays the fallback. The 250ms timeout bounds how long a busy main
  // thread can defer links arming.
  type IdleScheduler = {
    requestIdleCallback?: (cb: () => void, opts?: { timeout: number }) => number;
    cancelIdleCallback?: (handle: number) => void;
  };
  const idleWindow = window as Window & IdleScheduler;
  const _cancelScan = (): void => {
    if (!scanHandle) return;
    if (scanHandleIsIdle) idleWindow.cancelIdleCallback?.(scanHandle);
    else cancelAnimationFrame(scanHandle);
    scanHandle = 0;
  };

  const _rowOf = (n: Node | null): HTMLElement | null => {
    const el = n instanceof HTMLElement ? n : n?.parentElement ?? null;
    return el?.closest<HTMLElement>(ROW_SELECTOR) ?? null;
  };
  const _noteAdded = (n: Node): void => {
    if (!(n instanceof HTMLElement)) { const r = _rowOf(n); if (r) dirtyRows.add(r); return; }
    if (n.matches(ROW_SELECTOR)) { dirtyRows.add(n); return; }
    // A container appended wholesale (e.g. a scrollback .cell-block): take its
    // rows — bounded by the append size, never the whole history.
    for (const r of n.querySelectorAll<HTMLElement>(ROW_SELECTOR)) dirtyRows.add(r);
  };

  // Soft-wrap group expansion: a row whose text exactly fills `cols` continues
  // into the NEXT row, so a URL can span rows (computeRowLinks groups on this).
  // Walk prev/next across .cell-block and scrollback→viewport boundaries.
  const _prevRow = (el: HTMLElement): HTMLElement | null => {
    const sib = el.previousElementSibling;
    if (sib?.matches(ROW_SELECTOR)) return sib as HTMLElement;
    const parent = el.parentElement;
    if (!parent) return null;
    let scope: Element | null = null;
    if (parent.classList.contains("cell-block")) scope = parent.previousElementSibling;
    else if (parent.classList.contains("cell-viewport"))
      scope = parent.parentElement?.querySelector(".cell-scrollback")?.lastElementChild ?? null;
    const last = scope?.lastElementChild;
    return last?.matches(ROW_SELECTOR) ? (last as HTMLElement) : null;
  };
  const _nextRow = (el: HTMLElement): HTMLElement | null => {
    const sib = el.nextElementSibling;
    if (sib?.matches(ROW_SELECTOR)) return sib as HTMLElement;
    const parent = el.parentElement;
    if (!parent) return null;
    let scope: Element | null = null;
    if (parent.classList.contains("cell-block")) {
      scope = parent.nextElementSibling;
      // last block → first viewport row
      if (!scope) scope = parent.parentElement?.parentElement?.querySelector(".cell-viewport") ?? null;
    }
    const first = scope?.firstElementChild;
    return first?.matches(ROW_SELECTOR) ? (first as HTMLElement) : null;
  };
  /** The only rows a live stream can have touched: the viewport, plus the
   *  newest scrollback block the append tail writes into. Document order is
   *  preserved (scrollback before viewport) so soft-wrap grouping still works
   *  across that boundary. Empty for the legacy byte renderer, whose caller
   *  then falls back to the unbounded query. */
  const _hotRows = (): HTMLElement[] => {
    const out: HTMLElement[] = [];
    const newestBlock = container.querySelector(".cell-scrollback")?.lastElementChild;
    if (newestBlock) for (const r of newestBlock.querySelectorAll<HTMLElement>(ROW_SELECTOR)) out.push(r);
    const viewport = container.querySelector(".cell-viewport");
    if (viewport) for (const r of viewport.querySelectorAll<HTMLElement>(ROW_SELECTOR)) out.push(r);
    return out;
  };


  const scan = (): void => {
    scanScheduled = false;
    scanHandle = 0;
    if (!isPageVisible()) return;
    // Grid width from the cell renderer's --cell-cols var (set on this same
    // container, cellRenderer.ts::setGridWidth). Drives soft-wrap grouping so
    // a URL split across rows is detected as one logical line.
    const colsRaw = container.style.getPropertyValue("--cell-cols");
    const cols = colsRaw ? parseInt(colsRaw, 10) || 0 : 0;
    const ownerRepo = opts.githubOwnerRepo?.();
    const dirtyOverflow = dirtyRows.size > DIRTY_LIMIT && !fullScanNeeded;
    const hot = dirtyOverflow ? _hotRows() : [];
    const hotSet = new Set(hot);
    const hotStreamOverflow = dirtyOverflow
      && hot.length > 0
      && !Array.from(dirtyRows).some((row) => row.isConnected && !hotSet.has(row));
    if (fullScanNeeded || hotStreamOverflow) {
      // Bound hot-stream overflow to its live tail. History materialization has
      // connected cold rows and must fall through so loaded backfill receives
      // inferred URL/file anchors.
      dirtyRows.clear();
      const rows = hotStreamOverflow ? hot : Array.from(container.querySelectorAll<HTMLElement>(ROW_SELECTOR));
      fullScanNeeded = hotStreamOverflow;
      _linkifyRows(rows, cols, opts.resolveFile, ownerRepo);
      return;
    }
    if (dirtyRows.size === 0) return;
    const dirty = Array.from(dirtyRows);
    dirtyRows.clear();
    const visited = new Set<HTMLElement>();
    for (const seed of dirty) {
      if (!seed.isConnected || visited.has(seed)) continue;
      // Expand to the seed's full soft-wrap group (consecutive full-width rows).
      let first = seed;
      while (cols > 0) {
        const p = _prevRow(first);
        if (!p || visited.has(p) || _rowColumns(p) !== cols) break;
        first = p;
      }
      const group: HTMLElement[] = [];
      let cur: HTMLElement | null = first;
      while (cur) {
        group.push(cur);
        visited.add(cur);
        if (cols <= 0 || _rowColumns(cur) !== cols) break; // row doesn't wrap on
        cur = _nextRow(cur);
        if (!cur || visited.has(cur)) break;
      }
      _linkifyRows(group, cols, opts.resolveFile, ownerRepo);
    }
  };
  const scheduleScan = (): void => {
    if (scanScheduled) return;
    scanScheduled = true;
    const requestIdle = idleWindow.requestIdleCallback;
    if (requestIdle) {
      scanHandleIsIdle = true;
      scanHandle = requestIdle(scan, { timeout: 250 });
    } else {
      scanHandleIsIdle = false;
      scanHandle = requestAnimationFrame(scan);
    }
  };
  const obs = new MutationObserver((muts) => {
    // Hidden-tab gate: skip the per-mutation row bookkeeping entirely while
    // hidden — flag a full scan instead so nothing is missed. The queued rAF
    // fires when the tab becomes visible again (scan itself is also gated).
    if (!isPageVisible()) { fullScanNeeded = true; scheduleScan(); return; }
    for (const m of muts) {
      if (m.type === "characterData") { const r = _rowOf(m.target); if (r) dirtyRows.add(r); continue; }
      // childList INSIDE a row (span-level change) → that row; row-level
      // replacements arrive as addedNodes on the row's container.
      const tr = _rowOf(m.target);
      if (tr) dirtyRows.add(tr);
      for (const n of m.addedNodes) _noteAdded(n);
    }
    scheduleScan();
  });
  obs.observe(container, { childList: true, characterData: true, subtree: true });
  // Initial pass for whatever's already rendered.
  scheduleScan();

  // Visibility recovery. A rAF queued while the tab is hidden can be DROPPED by
  // the browser (not merely deferred) after a long-backgrounded / throttled /
  // slept tab — leaving scanScheduled===true with no pending callback. Every
  // later scheduleScan() then no-ops, so the cell renderer's per-frame row
  // rebuilds (renderViewport replaceWith) destroy <a> anchors with no
  // re-linkify → Cmd arms but there is no inferred link to click. The
  // claim/sync/health paths already re-evaluate on visibilitychange
  // (pageVisible.ts); the linkifier was the only one that didn't. On becoming
  // visible: cancel any stale/deferred frame, reset the latch, force a full scan.
  const onVisChange = (): void => {
    if (!isPageVisible()) return;
    _cancelScan();
    scanScheduled = false;
    fullScanNeeded = true;
    scheduleScan();
  };
  document.addEventListener("visibilitychange", onVisChange);

  const dispose = (): void => {
    obs.disconnect();
    _cancelScan();
    document.removeEventListener("visibilitychange", onVisChange);
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
    window.removeEventListener("blur", onBlur);
    container.removeEventListener("mouseover", onOver);
    container.removeEventListener("mouseout", onOut);
    container.removeEventListener("mouseenter", onPointerEnter);
    container.removeEventListener("mouseleave", onPointerLeave);
    container.removeEventListener("click", onClick);
    releaseInteraction();
    hintEl?.remove();
    hintEl = null;
  };
  return { releaseInteraction, dispose };
}
