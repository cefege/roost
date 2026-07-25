// CellGridRenderer — paints a CellGridFrame into DOM (R11).
// The SPA's terminal in cell mode renders here instead of feeding bytes to
// @wterm/core. We NEVER reflow: rows are painted at the worker's grid width
// and surplus pane space is letterboxed (rows are cols-ch wide, container
// wider → margin). This is what kills the history-corruption class — there
// is no client-side VT re-parse at a new width.
//
// DOM shape (reuses .wterm CSS vars + scroll from styles/sidebar.css):
//   .wterm.cell-grid > .cell-scrollback (append-only, immutable rows)
//                    > .cell-viewport   (re-rendered each frame, ~rows els)
// Scrollback rows are immutable (append-only) so we append, never re-render
// them — deep history costs one paint per line, ever. The viewport (bounded
// at `rows`) re-renders wholesale per frame, which is cheap.
//
// Pure style mapping (spanStyle / ansi256ToCss) is exported + unit-tested;
// the DOM glue is verified live via /roost-smoke (no jsdom in this repo).

import { applyDelta, type CellGridFrame, type CellRow, type CellSpan } from "@roost/shared/cell";
import {
  CELL_BOLD, CELL_DIM, CELL_ITALIC, CELL_UNDERLINE, CELL_BLINK,
  CELL_REVERSE, CELL_INVISIBLE, CELL_STRIKE, DEFAULT_COLOR,
} from "@roost/shared/cell";

const SB_BLOCK = 250; // scrollback rows per content-visibility block. sizeBlock() writes each block's EXACT pixel placeholder, so this is perf tuning only — any positive value renders identically.

// Fallback height of one .cell-row, in px, for the window before the live
// probe can measure (detached container / no layout yet): --term-font-size 14px
// (theme-vars.css) × line-height 1.2 (.cell-grid, styles/sidebar.css). Every
// row is exactly one line box — .cell-row is white-space:pre (never wraps) and
// height:1.2em (a CAP, not a floor), and blank rows carry a space (renderRow),
// so nothing can collapse or grow one. CellGridRenderer.rowHeight() supersedes
// this the moment the pane has layout.
const DEFAULT_ROW_PX = 16.8;

// `contain-intrinsic-size: auto <length>` makes the browser REMEMBER a block's
// real rendered size and use that instead of our estimate on every subsequent
// skip. An engine without the two-value form drops the whole inline
// declaration — silently falling back to the stylesheet's flat estimate — so
// feature-detect once at module scope and write the bare length there.
const CIS_AUTO = typeof CSS !== "undefined" && CSS.supports?.("contain-intrinsic-size", "auto 1px") === true;

// Pin a scrollback block's content-visibility placeholder to its EXACT height.
// A skipped block reports contain-intrinsic-size, not its content — so with the
// stylesheet's flat estimate every partial block (backfill chunks, the open
// tail block) misstates scrollHeight, and the instant the block materializes
// (scrolled into view, or a parked pane revealed) it reflows to its real height
// and every row below it shifts. That reflow IS the "scroll jumps around"
// class. Exact placeholder ⇒ reveal is a layout no-op ⇒ scrollHeight never lies
// ⇒ syncScroll's derived position always lands where it computed.
/** The contain-intrinsic-size value for a block of `rows` rows at a measured
 *  row height of `rowH` px. `auto` is feature-gated (CIS_AUTO) because an
 *  engine without the two-value form drops the declaration outright; the
 *  parameter exists so BOTH branches are unit-testable. Pure. */
export function blockPlaceholder(rows: number, rowH: number, auto = CIS_AUTO): string {
  const px = (rows * (rowH > 0 ? rowH : DEFAULT_ROW_PX)).toFixed(2);
  return auto ? `auto ${px}px` : `${px}px`;
}
function sizeBlock(blk: HTMLElement, rows: number, rowH: number): void {
  blk.style.setProperty("contain-intrinsic-size", blockPlaceholder(rows, rowH));
}

/** Slack for "at the bottom", in px. scrollTop is fractional on hi-DPI while
 *  scrollHeight/clientHeight are integers, so an exact pin can still measure
 *  1-2px short. Well under one row (1.2 × 13px = 15.6px), so a genuine one-line
 *  scroll-up is never mistaken for the bottom. Shared with lib/smoke.ts's
 *  renderProbe so the harness and the renderer agree by construction. */
export const BOTTOM_EPSILON_PX = 4;

// Per-renderer cap on held scrollback rows. CellGridRenderer._evictScrollback
// trims oldest whole content-visibility blocks once the held window grows past
// this — the client-side fix for long-uptime DOM growth (.cell-scrollback was
// append-only, so live nodes climbed ~500/min without bound while the server's
// ring stayed bounded at 10k). MUST be ≤ the server's 10k wtermCore ring so
// backfill can always re-supply evicted rows. 2000 ≈ 8 blocks / ~40 screens of
// 50-row immediate scroll-up before a backfill round-trip. Single tuning knob
// — lower if DOM headroom under many parked panes is still high.
export const MAX_HELD_SCROLLBACK_ROWS = 2000;

// xterm 256-palette → CSS. 0..15 map to the themed --term-color-N vars;
// 16..231 are the 6×6×6 cube; 232..255 are the 24-step grayscale ramp.
export function ansi256ToCss(n: number): string {
  if (n < 16) return `var(--term-color-${n})`;
  if (n >= 232) { const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`; }
  const c = n - 16;
  const steps = [0, 95, 135, 175, 215, 255];
  const r = steps[Math.floor(c / 36) % 6];
  const g = steps[Math.floor(c / 6) % 6];
  const b = steps[c % 6];
  return `rgb(${r},${g},${b})`;
}

function colorCss(color: number, rgb: number | undefined, isFg: boolean): string {
  if (rgb !== undefined) {
    return `#${((rgb >>> 0) & 0xffffff).toString(16).padStart(6, "0")}`;
  }
  if (color === DEFAULT_COLOR) return isFg ? "var(--term-fg)" : "var(--term-bg)";
  return ansi256ToCss(color);
}

/** Inline CSS for one span. reverse swaps fg/bg; invisible hides text.
 *  Pure — unit-tested in cellRenderer.test.ts. */
export function spanStyle(s: CellSpan): string {
  const reverse = (s.flags & CELL_REVERSE) !== 0;
  let fg = colorCss(s.fg, s.fgRgb, true);
  let bg = colorCss(s.bg, s.bgRgb, false);
  if (reverse) { const t = fg; fg = bg; bg = t; }
  const parts: string[] = [`color:${fg}`];
  // Only emit background when non-default (or reversed) — keeps the DOM lean
  // and lets the container --term-bg show through for blank cells.
  if (s.bg !== DEFAULT_COLOR || s.bgRgb !== undefined || reverse) parts.push(`background:${bg}`);
  if (s.flags & CELL_BOLD) parts.push("font-weight:bold");
  if (s.flags & CELL_DIM) parts.push("opacity:0.6");
  if (s.flags & CELL_ITALIC) parts.push("font-style:italic");
  const deco: string[] = [];
  if (s.flags & CELL_UNDERLINE) deco.push("underline");
  if (s.flags & CELL_STRIKE) deco.push("line-through");
  if (deco.length) parts.push(`text-decoration:${deco.join(" ")}`);
  if (s.flags & CELL_INVISIBLE) parts.push("visibility:hidden");
  if (s.flags & CELL_BLINK) parts.push("animation:cell-blink 1s step-end infinite");
  return parts.join(";");
}

function renderRow(row: CellRow, doc: Document): HTMLElement {
  const el = doc.createElement("div");
  el.className = "cell-row";
  if (row.spans.length === 0) {
    el.appendChild(doc.createTextNode(" ")); // keep blank rows tall
    return el;
  }
  for (const s of row.spans) {
    const span = doc.createElement("span");
    span.setAttribute("style", spanStyle(s));
    span.textContent = s.text;
    el.appendChild(span);
  }
  return el;
}

/** Concatenated text of a row's spans. */
export function rowText(row: CellRow): string {
  return row.spans.map((s) => s.text).join("");
}

/** Visual identity of a row: text + per-span style. Rows with equal sigs
 *  paint identically, so the viewport diff can skip them (renderViewport). */
export function rowSig(row: CellRow): string {
  let s = "";
  for (const sp of row.spans) s += sp.text + "\u0001" + spanStyle(sp) + "\u0002";
  return s;
}

/** Merge an incoming FULL frame onto the held frame when its scrollback
 *  (possibly a tail, sbBase > 0 — see types.ts) verifiably EXTENDS the held
 *  window: same width, non-shrinking, overlap present, and boundary-row text
 *  identical. Scrollback lines are immutable once pushed, so text identity
 *  proves no ring eviction / reflow shifted the shared prefix (text differs
 *  the instant a line changes index). Returns the merged frame + the newly
 *  appended rows, or null when only a full rebuild is safe (width change,
 *  shrink, or a gap the tail doesn't cover — the backfill controller fills
 *  the remaining [0, sbBase) hole). Pure; unit-tested in cellRenderer.test.ts. */
export function mergeFullFrame(
  base: CellGridFrame,
  incoming: CellGridFrame,
): { frame: CellGridFrame; appended: CellRow[] } | null {
  if (incoming.cols !== base.cols) return null;
  if (incoming.scrollbackTotal < base.scrollbackTotal) return null;
  if (base.scrollbackTotal === 0) {
    if (incoming.sbBase !== 0) return null; // held nothing; tail would start with a hole
    return { frame: incoming, appended: incoming.scrollbackRows };
  }
  const boundaryAbs = base.scrollbackTotal - 1;
  const inIdx = boundaryAbs - incoming.sbBase;
  if (inIdx < 0) return null; // viewer missed more rows than the tail covers
  const inRow = incoming.scrollbackRows[inIdx];
  const baseRow = base.scrollbackRows[boundaryAbs - base.sbBase];
  if (!inRow || !baseRow || rowText(inRow) !== rowText(baseRow)) return null;
  const appended = incoming.scrollbackRows.slice(inIdx + 1);
  return {
    frame: { ...incoming, scrollbackRows: base.scrollbackRows.concat(appended), sbBase: base.sbBase },
    appended,
  };
}

export class CellGridRenderer {
  private frame: CellGridFrame | null = null;
  // While the user has text selected over this pane, repainting the viewport
  // (replaceChildren) would destroy the selection — the reason selection was
  // unusable in a live alt-screen TUI. selectionHold freezes DOM writes;
  // pendingRender remembers that frames arrived so release can reconcile.
  // armedHold is the SAME freeze for a different interaction: Cmd-hovering a
  // terminal file link (see terminal-links onArmedHoverChange) so the wrapped
  // <a> isn't rebuilt out from under the cursor. Either hold freezes.
  private selectionHold = false;
  private armedHold = false;
  private pendingRender = false;
  // Scrollback rows are packed into content-visibility "blocks" of SB_BLOCK rows.
  // Off-screen blocks skip layout/paint, so this class's own scrollHeight reads
  // (atBottom / syncScroll) + reveal reflow stay O(history / SB_BLOCK), not
  // O(history) — the fix for the pane-switch freeze (a deep-history pane used to
  // relayout every line on reveal). The last block stays open until it fills;
  // full blocks are immutable (append-only model).
  private _curBlock: HTMLElement | null = null;
  private _curBlockRows = 0;
  private readonly scrollbackEl: HTMLElement;
  private readonly viewportEl: HTMLElement;
  private readonly cursorEl: HTMLElement;
  // Ghost cursors — remote viewers' cursor positions, same viewport space as the
  // real cursor (ch/lh units → grid-aligned, letterbox/scroll-immune). Replaces
  // byte-mode's pixel-math GhostCursorOverlay; fed via setGhosts().
  private readonly ghostsEl: HTMLElement;
  private readonly doc: Document;
  // Viewport diff cache: painted row elements + their rowSig, in order.
  // renderViewport re-renders ONLY rows whose sig changed — idle frames and
  // cursor-only deltas cost zero DOM writes (the old replaceChildren rebuilt
  // every row on every frame: ~1.5k nodes/3s per idle pane, the deck-wide
  // background churn). renderFull/dispose reset both.
  private _rowEls: HTMLElement[] = [];
  private _rowSigs: string[] = [];
  // ── scroll intent ─────────────────────────────────────────────────────
  // The pane's scroll position is a DECLARED value in absolute scrollback-row
  // space, re-derived after every mutation — never a pixel scrollTop that has
  // to survive a rebuild, a park→reveal container resize, an alt-screen
  // display:none collapse or a browser scroll-anchor adjustment. It survived
  // none of those, which is why the pane kept landing mid-history.
  // "tail" = follow the live bottom. "anchor" = keep absolute scrollback row
  // `row` at `off` px below the top of the visible area. Row-space anchors are
  // invariant under prepend, eviction and rebuild — that is what kills the
  // whole jump class.
  private _intent: { kind: "tail" } | { kind: "anchor"; row: number; off: number } = { kind: "tail" };
  private _selfTop = -1;  // last scrollTop WE wrote — see isSelfScroll()
  private _selfWant = -1; // the value we ASKED for, before the browser clamped
  private _rowH = 0;      // measured px height of one .cell-row; 0 = not measured yet

  constructor(private readonly container: HTMLElement) {
    this.doc = container.ownerDocument;
    container.classList.add("wterm", "cell-grid");
    this.scrollbackEl = this.doc.createElement("div");
    this.scrollbackEl.className = "cell-scrollback";
    this.viewportEl = this.doc.createElement("div");
    this.viewportEl.className = "cell-viewport";
    // Anchor for the absolute-positioned cursor + ghost overlays.
    this.viewportEl.style.position = "relative";
    this.cursorEl = this.doc.createElement("div");
    this.cursorEl.className = "cell-cursor";
    this.ghostsEl = this.doc.createElement("div");
    this.ghostsEl.className = "cell-ghosts";
    container.appendChild(this.scrollbackEl);
    container.appendChild(this.viewportEl);
    // A late webfont swap changes the line box under us — drop the cached row
    // height so the next derivation re-measures instead of anchoring on stale px.
    void this.doc.fonts?.ready?.then(() => { this._rowH = 0; });
  }

  /** Remote viewers' cursors (ghost cursors). Rendered in the viewport at
   *  ch/lh grid coords — same space as the real cursor. Re-attached after every
   *  renderViewport (replaceChildren wipes overlays). */
  setGhosts(ghosts: ReadonlyMap<string, { x: number; y: number; label?: string }>): void {
    const boxes: HTMLElement[] = [];
    for (const [id, g] of ghosts) {
      const box = this.doc.createElement("div");
      box.className = "cell-ghost";
      box.dataset.operatorId = id;
      box.title = g.label ?? id;
      box.style.transform = `translate(${g.x}ch, ${g.y}lh)`;
      boxes.push(box);
    }
    this.ghostsEl.replaceChildren(...boxes);
    if (this.ghostsEl.parentElement !== this.viewportEl) this.viewportEl.appendChild(this.ghostsEl);
  }

  /** Apply a full or delta frame. A delta before any full frame is dropped
   *  (the worker sends a full on attach/reconnect, so we self-heal). */
  apply(incoming: CellGridFrame): void {
    this._adoptUserScroll();
    if (incoming.full) {
      const base = this.frame;
      // Fast path: same-width reveal / re-claim. Full frames carry only a
      // scrollback TAIL (sbBase); when it verifiably extends the held window
      // (mergeFullFrame), keep the held history + append the new rows — no
      // replaceChildren, scroll position preserved, and another viewer's
      // attach can't wipe this viewer's backfilled depth.
      const merged = base ? mergeFullFrame(base, incoming) : null;
      if (merged) {
        this.frame = merged.frame;
        if (this.holding) { this.pendingRender = true; return; }
        this._appendScrollback(merged.appended);
        this.renderViewport(incoming.altScreen ? 0 : merged.appended.length);
        this.setGridWidth();
        this._syncAltScreen();
        this.syncScroll();
        return;
      }
      // Slow path: fresh mount / width change / reset / uncovered gap →
      // rebuild from the incoming frame verbatim (block-packed, so even deep
      // history lays out cheaply). sbBase > 0 leaves a [0, sbBase) hole the
      // backfill controller fills via prependScrollback.
      this.frame = incoming;
      if (this.holding) { this.pendingRender = true; return; }
      this.renderFull();
      return;
    }
    if (!this.frame) return; // no base yet — wait for a full frame
    const appended = incoming.scrollbackAppend;
    this.frame = applyDelta(this.frame, incoming);
    // Mid-hold (selection / Cmd-hover): fold frames into this.frame but freeze
    // the DOM — the rebuild on release reconciles to the latest (field docs above).
    if (this.holding) { this.pendingRender = true; return; }
    // Scrollback is append-only — paint just the new lines.
    this._appendScrollback(appended);
    this.renderViewport(this.frame.altScreen ? 0 : appended.length);
    this.setGridWidth();
    this._syncAltScreen();
    this.syncScroll();
  }

  /** Alt-screen (claude fullscreen / vim / htop) OWNS the viewport — there is no
   *  scrollback in that mode. The worker ships frame.altScreen; without honoring
   *  it the stale pre-alt scrollback sheet stays in the DOM above the viewport
   *  ("historic junk on top") and the wheel scrolls up into it. Toggle a class
   *  so CSS hides scrollback + locks scroll while alt is active; leaving alt
   *  restores both. */
  private _syncAltScreen(): void {
    this.container.classList.toggle("alt-active", this.frame?.altScreen === true);
  }

  /** True while ANY hold is active — freezes viewport/scrollback repaints. */
  private get holding(): boolean {
    return this.selectionHold || this.armedHold;
  }

  /** Freeze/thaw DOM repaints. Held while the user has text selected over this
   *  pane (see CellTerminal's selectionchange wiring); releasing flushes the
   *  latest folded frame so the pane snaps back to live. */
  setSelectionHold(active: boolean): void {
    if (this.selectionHold === active) return;
    this.selectionHold = active;
    this._flushIfReleased();
  }

  /** Freeze/thaw DOM repaints while the user Cmd-hovers a terminal file link
   *  (terminal-links onArmedHoverChange). Same freeze as selection; keeps the
   *  wrapped <a> from being rebuilt out from under the cursor. */
  setArmedHold(active: boolean): void {
    if (this.armedHold === active) return;
    this.armedHold = active;
    this._flushIfReleased();
  }

  /** After any hold clears, rebuild from the latest folded frame so the pane
   *  snaps back to live. No-op while another hold is still active. */
  private _flushIfReleased(): void {
    this._adoptUserScroll();
    if (this.holding || !this.pendingRender) return;
    this.pendingRender = false;
    this.renderFull(); // rebuild scrollback + viewport from the latest frame
  }

  /** Rebuild the whole grid from this.frame — fresh mount / width change / reset
   *  / hold-release, where the painted DOM can't be reused. Cheap even for deep
   *  history: scrollback packs into content-visibility blocks (_appendScrollback),
   *  so the browser's layout stays O(blocks). */
  private renderFull(): void {
    if (!this.frame) return;
    this._rowH = 0; // container may have been resized/re-fonted since the last measure
    this.scrollbackEl.replaceChildren();
    this._curBlock = null;
    this._curBlockRows = 0;
    this.viewportEl.replaceChildren();
    this._rowEls = [];
    this._rowSigs = [];
    this._appendScrollback(this.frame.scrollbackRows);
    this.renderViewport();
    this.setGridWidth();
    this._syncAltScreen();
    // Without this the landing spot after a wipe-and-rebuild is whatever the
    // browser's scrollTop clamp happens to produce — the bimodal mid-history
    // reveal. The intent decides instead.
    this.syncScroll();
  }

  /** Append rows to the scrollback, packing them into content-visibility "blocks"
   *  of SB_BLOCK. Off-screen blocks skip layout/paint, so a reveal reflow or a
   *  scrollHeight read walks ~history/SB_BLOCK blocks, not every line — the fix
   *  for the pane-switch freeze. Rows are immutable + append-only, so the last
   *  block stays open until it fills. */
  private _appendScrollback(rows: readonly CellRow[]): void {
    for (const r of rows) {
      if (!this._curBlock || this._curBlockRows >= SB_BLOCK) {
        if (this._curBlock) sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
        const blk = this.doc.createElement("div");
        blk.className = "cell-block";
        this.scrollbackEl.appendChild(blk);
        this._curBlock = blk;
        this._curBlockRows = 0;
      }
      this._curBlock.appendChild(renderRow(r, this.doc));
      this._curBlockRows++;
    }
    if (this._curBlock) sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
    this._evictScrollback();
  }

  /** Evict oldest whole content-visibility blocks once the held scrollback
   *  window exceeds MAX_HELD_SCROLLBACK_ROWS. Runs only while the pane is
   *  FOLLOWING the tail (the unbounded streaming case); while an anchor holds
   *  the view in history the freeze keeps history intact. Evicted rows stay fully
   *  recoverable: bumping sbBase keeps the held-window invariant
   *  (scrollbackRows.length === scrollbackTotal - sbBase) honest, so
   *  scrollbackBackfill's onUserScrollUp re-pulls exactly the evicted range.
   *
   *  dropped = the leading block's ACTUAL child count, not a hardcoded
   *  SB_BLOCK: every backfill prepend is < SB_BLOCK rows (scrollbackBackfill
   *  fetches with endRow = sbBase+1 to include the overlap row, then strips it
   *  via rows.slice(0,-1)), so a partial block at the head is the norm after
   *  any backfill cycle, not just the final chunk. Slicing by the real count
   *  keeps scrollbackRows aligned with the painted DOM regardless of size. */
  private _evictScrollback(): void {
    if (!this.following()) return;
    while (this.frame && this.frame.scrollbackRows.length > MAX_HELD_SCROLLBACK_ROWS) {
      // Leading child is a .cell-block (possibly partial from backfill); the
      // open tail block _curBlock is always last, so firstElementChild is never it.
      const lead = this.scrollbackEl.firstElementChild as HTMLElement | null;
      if (!lead) break;
      const dropped = lead.children.length;
      if (dropped === 0) break; // defensive — never spin on an empty block
      lead.remove();
      this.frame = {
        ...this.frame,
        scrollbackRows: this.frame.scrollbackRows.slice(dropped),
        sbBase: this.frame.sbBase + dropped,
      };
    }
    // Removing leading blocks shrinks scrollHeight under the viewport. No
    // distance-from-bottom arithmetic: the intent is in row space and sbBase
    // is already updated, so re-deriving lands on the same content.
    this.syncScroll();
  }

  /** Backfill splice: insert older history rows ABOVE the painted scrollback
   *  (lazy-history attach — full frames carry only a tail, the controller in
   *  scrollbackBackfill.ts pulls [0, sbBase) and lands it here). `rows` must
   *  end exactly at the held window's first row; misaligned splices are
   *  dropped (epoch moved — the next reframe restarts the backfill). Scroll is
   *  re-derived from the intent after the frame's sbBase drops by rows.length,
   *  so a tail-following AND an anchored reader both see zero movement — an
   *  absolute row anchor needs no compensation, which is the whole point.
   *  Prepended blocks are complete/immutable — _curBlock (the append tail) is
   *  untouched. Applies even mid-hold: no existing node is rebuilt, so a live
   *  selection survives, and a hold-release renderFull rebuilds from the
   *  merged frame. */
  prependScrollback(rows: readonly CellRow[]): void {
    this._adoptUserScroll();
    if (!this.frame || rows.length === 0) return;
    if (rows[rows.length - 1]!.index + 1 !== this.frame.sbBase) return;
    const rowH = this.rowHeight();
    const frag = this.doc.createDocumentFragment();
    let blk: HTMLElement | null = null;
    let blkRows = 0;
    for (const r of rows) {
      if (!blk || blkRows >= SB_BLOCK) {
        if (blk) sizeBlock(blk, blkRows, rowH);
        blk = this.doc.createElement("div");
        blk.className = "cell-block";
        frag.appendChild(blk);
        blkRows = 0;
      }
      blk.appendChild(renderRow(r, this.doc));
      blkRows++;
    }
    if (blk) sizeBlock(blk, blkRows, rowH);
    this.scrollbackEl.prepend(frag);
    // Frame first: syncScroll reads frame.sbBase to place the anchor, so it
    // must already reflect the prepend.
    this.frame = {
      ...this.frame,
      scrollbackRows: rows.concat(this.frame.scrollbackRows),
      sbBase: this.frame.sbBase - rows.length,
    };
    this.syncScroll();
  }

  /** Backfill validation surface: how deep the hole is (sbBase), the width
   *  the held rows were rendered at, and the first held row's text — the
   *  overlap row a get-scrollback-cells response must reproduce before the
   *  controller splices (see scrollbackBackfill.ts). null = no frame yet. */
  backfillAnchor(): { sbBase: number; cols: number; total: number; firstHeldText: string | null } | null {
    if (!this.frame) return null;
    const first = this.frame.scrollbackRows[0];
    return {
      sbBase: this.frame.sbBase,
      cols: this.frame.cols,
      total: this.frame.scrollbackTotal,
      firstHeldText: first ? rowText(first) : null,
    };
  }

  // `scrolled` = rows that left the viewport top for scrollback this frame
  // (authoritative from the frame: scrollbackAppend length / scrollbackTotal
  // delta; 0 in alt-screen). Dropping those elements shifts the stacked rows
  // below up for free, so a scrolling viewport re-renders only its NEW tail
  // rows. Pure reuse hint — the sig-checked diff below still validates every
  // row, so a wrong count costs re-renders, never wrong pixels.
  private renderViewport(scrolled = 0): void {
    if (!this.frame) return;
    const rows = this.frame.viewportRows;
    const k = Math.min(scrolled, this._rowEls.length);
    for (let i = 0; i < k; i++) this._rowEls[i]!.remove();
    if (k > 0) { this._rowEls.splice(0, k); this._rowSigs.splice(0, k); }
    for (let i = 0; i < rows.length; i++) {
      const sig = rowSig(rows[i]!);
      if (i < this._rowEls.length) {
        if (this._rowSigs[i] === sig) continue; // unchanged row → zero DOM writes
        const el = renderRow(rows[i]!, this.doc);
        this._rowEls[i]!.replaceWith(el);
        this._rowEls[i] = el;
        this._rowSigs[i] = sig;
      } else {
        const el = renderRow(rows[i]!, this.doc);
        // Append after the existing rows (before the cursor overlay when
        // attached, so overlays keep sitting at the tail).
        this.viewportEl.insertBefore(el, this.cursorEl.parentElement === this.viewportEl ? this.cursorEl : null);
        this._rowEls.push(el);
        this._rowSigs.push(sig);
      }
    }
    while (this._rowEls.length > rows.length) {
      this._rowEls.pop()!.remove();
      this._rowSigs.pop();
    }
    // The diff never wipes the overlays — attach once if missing.
    if (this.cursorEl.parentElement !== this.viewportEl) this.viewportEl.appendChild(this.cursorEl);
    if (this.ghostsEl.parentElement !== this.viewportEl) this.viewportEl.appendChild(this.ghostsEl);
    this.updateCursor();
  }

  // Predicted-cursor override (ConditionalCursorMove): when predictive echo
  // is showing type-ahead, the cursor sits at the PREDICTED column, not the
  // authoritative one — so the caret leads the echoed chars. null =
  // use the authoritative position. Set via setPredictedCursor().
  private predictedCol: number | null = null;
  setPredictedCursor(col: number | null): void {
    this.predictedCol = col;
    this.updateCursor();
  }

  // Block cursor overlay at the worker's reported (row,col) — or the predicted
  // column when predictive echo overrides it. lh/ch units pin it to the
  // monospace grid; blink + look live in CSS (.cell-cursor). Hidden when the
  // program hides the cursor (cursorVisible=false).
  private updateCursor(): void {
    if (!this.frame) return;
    const c = this.cursorEl;
    if (!this.frame.cursorVisible) { c.style.display = "none"; return; }
    c.style.display = "block";
    c.style.top = `${this.frame.cursorRow}lh`;
    c.style.left = `${this.predictedCol ?? this.frame.cursorCol}ch`;
  }

  // Pin the painted width to the grid's cols (ch units, monospace) so a
  // wider pane letterboxes (margin) instead of stretching — no reflow.
  private setGridWidth(): void {
    if (!this.frame) return;
    this.container.style.setProperty("--cell-cols", String(this.frame.cols));
  }

  /** Text of the last NON-blank viewport line. Used to tell a shell prompt
   *  (ends in $ / % / # / ❯ / ➜) from a full-screen TUI (claude/vim/htop,
   *  whose bottom line never does) — see CellTerminal's launch-Claude FAB. */
  viewportTail(): string {
    if (!this.frame) return "";
    const rows = this.frame.viewportRows;
    for (let i = rows.length - 1; i >= 0; i--) {
      const text = rowText(rows[i]!);
      if (text.trim() !== "") return text;
    }
    return "";
  }

  /** Visible viewport as text (one row per line) — exactly what's painted on
   *  screen now. Source for keyterm extraction (keytermContext.ts), weight 1.0:
   *  what you're looking at while you speak. */
  gridText(): string {
    if (!this.frame) return "";
    return this.frame.viewportRows
      .map((r) => rowText(r))
      .join("\n");
  }

  /** The latest applied CellGridFrame, or null before the first frame.
   *  Source for terminal preview thumbnails (terminalPreview.ts). */
  get currentFrame(): CellGridFrame | null { return this.frame; }

  /** Last `maxRows` scrollback lines as text (oldest→newest), capped so a 10k
   *  ring never drowns the keyterm signal. Recency-decayed by the caller. */
  scrollbackText(maxRows = 250): string {
    if (!this.frame) return "";
    const rows = this.frame.scrollbackRows;
    return rows
      .slice(Math.max(0, rows.length - maxRows))
      .map((r) => rowText(r))
      .join("\n");
  }

  /** The viewport element (position:relative) — overlay host for the cursor
   *  and the predictive-echo layer. */
  get predictionHost(): HTMLElement { return this.viewportEl; }

  // ── scroll intent: derive, never remember pixels ──────────────────────

  /** Measured px height of one .cell-row, or 0 while unmeasurable (detached
   *  container, no layout yet). Probe mirrors CellTerminal.measureCell — one
   *  measurement convention in the codebase, not two. Invalidated by
   *  renderFull() and by a late webfont swap (constructor's fonts.ready hook). */
  rowHeight(): number {
    if (this._rowH > 0) return this._rowH;
    const p = this.doc.createElement("div");
    p.className = "cell-row";
    p.style.position = "absolute";
    p.style.visibility = "hidden";
    p.textContent = " ";
    this.viewportEl.appendChild(p);
    const h = p.getBoundingClientRect?.().height ?? 0;
    p.remove();
    if (h > 0) this._rowH = h;
    return this._rowH;
  }

  /** The ONLY writer of the container's scroll position in this file. Clamps,
   *  writes instantly (behavior:"instant" so an inherited scroll-behavior:smooth
   *  can never turn a re-derivation into a visible crawl), then records what the
   *  browser actually took. */
  private _setScrollTop(v: number): void {
    const el = this.container;
    const top = Math.max(0, Math.min(v, Math.max(0, el.scrollHeight - el.clientHeight)));
    // A programmatic scrollTo cancels compositor-driven momentum even when it
    // writes the value already there, so a per-frame "corrective" write that
    // corrects nothing still kills a trackpad fling. Skip it.
    if (Math.abs(el.scrollTop - top) > 0.5) el.scrollTo({ top, behavior: "instant" });
    this._selfWant = v;
    this._selfTop = el.scrollTop; // read back — the browser may clamp further
  }

  /** True when the container's scroll position is still the value WE wrote.
   *  Scroll events are delivered asynchronously, so every programmatic write —
   *  and every browser clamp we corrected in the same frame — echoes back later
   *  looking exactly like a user gesture. This is how the listener tells them
   *  apart, and it replaced CellTerminal's `lastTop` delta heuristic. */
  isSelfScroll(): boolean {
    const el = this.container;
    if (Math.abs(el.scrollTop - this._selfTop) <= 1) return true;
    // A layout change AFTER our write re-clamps the position and the browser
    // reports it as a scroll event we did not cause — the deck's SECOND slot
    // commit on a spotlit tab switch (deckOps.ts selectTabOp re-commits with a
    // 40px-taller box), or a content-visibility block materialising on reveal.
    // It is still our write: we asked for at least the maximum the container
    // can now offer, and that is exactly where we sit. A real gesture from a
    // pinned-at-max container always moves AWAY from max, so this can never
    // swallow one.
    const max = Math.max(0, el.scrollHeight - el.clientHeight);
    return this._selfWant >= max && Math.abs(el.scrollTop - max) <= 1;
  }

  /** Adopt the container's live position as intent when the user moved it since
   *  our last write. MUST run BEFORE any DOM mutation in the same task: once we
   *  have mutated, a browser clamp is indistinguishable from a gesture
   *  (renderFull empties the container and scrollTop clamps to 0), and adopting
   *  there anchors the pane at the top of history. Scroll events are delivered
   *  asynchronously, so without this every frame that lands mid-gesture derives
   *  from a pre-gesture intent and yanks the view back. */
  private _adoptUserScroll(): void {
    if (!this.frame || this.frame.altScreen) return;
    if (this.isSelfScroll()) return;
    this.captureScrollIntent();
  }

  /** Is the pane following the live tail? Replaces CellTerminal's `stick`. */
  following(): boolean { return this._intent.kind === "tail"; }

  /** The declared position, in absolute scrollback-row space. Read-only view
   *  for the smoke backdoor's scroll regression probe. `_intent` is replaced
   *  wholesale, never mutated, so handing it out cannot alias state. */
  scrollIntent(): Readonly<{ kind: "tail" } | { kind: "anchor"; row: number; off: number }> {
    return this._intent;
  }

  /** Declare tail-following and land there now (pane reveal, jump-to-latest). */
  followTail(): void {
    this._intent = { kind: "tail" };
    this.syncScroll();
  }

  /** Turn a genuine user gesture into an intent. The ONLY place a scroll event
   *  becomes state — everything else derives. */
  captureScrollIntent(): void {
    if (!this.frame || this.frame.altScreen) return; // alt has no scrollback
    if (this.atBottom()) { this._intent = { kind: "tail" }; return; }
    const h = this.rowHeight();
    if (h <= 0) return; // unmeasurable → keep the current intent rather than guess
    // scrollbackEl.offsetTop = the container's padding-top: .wterm is
    // position:relative (styles/sidebar.css) so it IS the offset parent.
    const top = this.container.scrollTop - (this.scrollbackEl.offsetTop ?? 0);
    const k = Math.max(0, Math.floor(top / h));
    this._intent = { kind: "anchor", row: this.frame.sbBase + k, off: top - k * h };
  }

  /** Re-derive the pixel position from the declared intent. THE single
   *  corrective path: every mutation that can move content under the viewport
   *  (apply / renderFull / evict / prepend) ends here, and CellTerminal calls it
   *  on container resize. Alt-screen has no scrollback and locks scroll, so it
   *  is a no-op there — leaving alt is covered by apply()'s trailing call, which
   *  runs after _syncAltScreen() has already cleared the flag. */
  syncScroll(): void {
    if (!this.frame || this.frame.altScreen) return;
    // A rebuild — or a catch-up tail deeper than SB_SNAPSHOT_MAX_CATCHUP_ROWS —
    // can leave the declared row outside the held window. Clamping the
    // DERIVATION (k = max(0, ...)) pins the pixel at the window top, so every
    // backfill prepend slides 250 rows of content under a stationary viewport:
    // the "lurches back down one chunk at a time" report. Re-declare to the
    // oldest reachable row instead — prepends then land above it and the reader
    // holds still.
    if (this._intent.kind === "anchor" && this._intent.row < this.frame.sbBase) {
      this._intent = { kind: "anchor", row: this.frame.sbBase, off: 0 };
    }
    if (this._intent.kind === "tail") { this._setScrollTop(this.container.scrollHeight); return; }
    const h = this.rowHeight();
    if (h <= 0) { this._setScrollTop(this.container.scrollHeight); return; }
    const k = Math.max(0, this._intent.row - this.frame.sbBase);
    this._setScrollTop((this.scrollbackEl.offsetTop ?? 0) + k * h + this._intent.off);
  }

  /** Is the viewport scrolled to the bottom (live tail visible)? */
  atBottom(): boolean {
    const el = this.container;
    return el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_EPSILON_PX;
  }

  /** Within one viewport of the top of the painted scrollback — the backfill
   *  controller's "pull deeper" trigger. Draining the whole ring the moment the
   *  user leaves the bottom built 250 rows of DOM per frame while they were
   *  mid-gesture; pulling only on approach keeps history reachable without the
   *  sustained jank. */
  nearHistoryTop(): boolean {
    if (!this.frame || this.frame.altScreen) return false;
    const el = this.container;
    return el.scrollTop - (this.scrollbackEl.offsetTop ?? 0) < el.clientHeight;
  }

  dispose(): void {
    this.scrollbackEl.remove();
    this.viewportEl.remove();
    this.frame = null;
    this._rowEls = [];
    this._rowSigs = [];
  }
}
