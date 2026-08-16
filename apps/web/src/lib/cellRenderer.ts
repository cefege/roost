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
// them — deep history costs one paint per line, ever. Full repairs rebuild the
// bounded viewport explicitly; normal deltas inspect and patch only the dirty
// row indices carried on the wire.

import {
  applyDelta as foldCellDelta,
  deltaViewportShift,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import { renderRow, rowText, rowHash, type FindHit } from "./cellRow.ts";

const SB_BLOCK = 250; // scrollback rows per content-visibility block. sizeBlock() writes each block's EXACT pixel placeholder, so this is perf tuning only — any positive value renders identically.

// Fallback height of one .cell-row, in px, for the window before the live
// probe can measure (detached container / no layout yet): --term-font-size 14px
// (theme-vars.css) × line-height 1.2 (.cell-grid, styles/sidebar.css). Every
// row is exactly one line box — .cell-row is white-space:pre (never wraps) and
// height:1.2em (a CAP, not a floor), and blank rows carry a space (renderRow),
// so nothing can collapse or grow one. CellGridRenderer.rowHeight() supersedes
// this the moment the pane has layout.
const DEFAULT_ROW_PX = 16.8;

// Pin a scrollback block's content-visibility placeholder to its EXACT height.
// A skipped block reports contain-intrinsic-size, not its content — so with the
// stylesheet's flat estimate every partial block (backfill chunks, the open
// tail block) misstates scrollHeight, and the instant the block materializes
// (scrolled into view, or a parked pane revealed) it reflows to its real height
// and every row below it shifts. Exact placeholders make those reveals layout
// no-ops, leaving native browser scroll anchoring stable.
//
// The value is deliberately a BARE length, never `auto <length>`: `auto` tells
// the browser to REMEMBER a block's last rendered size and use that instead of
// this value on every later skip. A block that grows while it is skipped — every
// append to a parked deck pane's open tail block, and any append while the user
// is scrolled far up — then keeps its stale remembered height, understating
// scrollHeight. Materializing it on reveal snaps it back to the truth, which
// moves the scroll maximum out from under a pane that was pinned to the bottom
// (measured: a 250-row block remembered at 29 rows reported 487.11px instead of
// 4199.22px; revealing it grew scrollHeight by exactly that 3712px difference
// and latched atBottom() false). rows × the MEASURED row height is already
// exact — 250 × 16.796875 = 4199.21875 against a real 4199.21875 — so there is
// nothing for the browser's memory to improve on.
/** The contain-intrinsic-size value for a block of `rows` rows at a measured
 *  row height of `rowH` px. Pure. */
export function blockPlaceholder(rows: number, rowH: number): string {
  return `${(rows * (rowH > 0 ? rowH : DEFAULT_ROW_PX)).toFixed(2)}px`;
}
function sizeBlock(blk: HTMLElement, rows: number, rowH: number): void {
  blk.style.setProperty("contain-intrinsic-size", blockPlaceholder(rows, rowH));
}


// Per-renderer cap on held scrollback rows. CellGridRenderer._evictScrollback
// trims oldest whole content-visibility blocks once the held window grows past
// this — the client-side fix for long-uptime DOM growth (.cell-scrollback was
// append-only, so live nodes climbed ~500/min without bound while the server's
// ring stayed bounded at 10k). MUST be ≤ the server's 10k wtermCore ring so
// backfill can always re-supply evicted rows. 2000 ≈ 8 blocks / ~40 screens of
// 50-row immediate scroll-up before a backfill round-trip. Single tuning knob
// — lower if DOM headroom under many parked panes is still high.
export const MAX_HELD_SCROLLBACK_ROWS = 2000;

/** Immutable grid identity and absolute range used to validate one history page. */
export interface BackfillAnchor {
  sbBase: number;
  cols: number;
  total: number;
  gridEpoch: string;
}



export class CellGridRenderer {
  private frame: CellGridFrame | null = null;
  // A repair can arrive while the reader is inspecting history. Keep the
  // painted frame immutable until they deliberately return to its bottom;
  // live frames continue folding here so claim seqs stay current.
  private readerPendingFrame: CellGridFrame | null = null;
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
  private pendingPinToBottom = false;
  // Scrollback rows are packed into content-visibility "blocks" of SB_BLOCK rows.
  // Off-screen blocks skip layout/paint, so scrollHeight reads and reveal reflow
  // stay O(history / SB_BLOCK), not O(history) — the fix for the pane-switch
  // freeze (a deep-history pane used to relayout every line on reveal). The last
  // block stays open until it fills; full blocks are immutable (append-only model).
  private _curBlock: HTMLElement | null = null;
  private _curBlockRows = 0;
  private readonly spacerEl: HTMLElement;
  private readonly scrollbackEl: HTMLElement;
  private readonly viewportEl: HTMLElement;
  private readonly cursorEl: HTMLElement;
  // Ghost cursors — remote viewers' cursor positions, same viewport space as the
  // real cursor (ch/lh units → grid-aligned, letterbox/scroll-immune). Replaces
  // byte-mode's pixel-math GhostCursorOverlay; fed via setGhosts().
  private readonly ghostsEl: HTMLElement;
  private readonly doc: Document;
  // Viewport diff cache: painted row elements + their rowHash, in order.
  // renderViewport re-renders ONLY rows whose hash changed — idle frames and
  // cursor-only deltas cost zero DOM writes (the old replaceChildren rebuilt
  // every row on every frame: ~1.5k nodes/3s per idle pane, the deck-wide
  // background churn). renderFull/dispose reset both.
  private _rowEls: HTMLElement[] = [];
  private _rowHashes: number[] = [];
  private _rowH = 0;      // measured px height of one .cell-row; 0 = not measured yet
  private _lastBoxH = 0;  // clientHeight at the last box observation (constructor + noteBoxResize)
  // Find highlights, keyed by ABSOLUTE row index (the space the worker's match
  // rows and PbCellRow.index share). Empty by default, so the whole feature
  // costs one Map lookup per painted row when nothing is being searched.
  private _findHits: ReadonlyMap<number, FindHit[]> = new Map();
  private _activeHit: { row: number; col: number } | null = null;
  // Cached DOM state: a cursor-only delta should update model/ACK state without
  // repeating identical class/style writes.
  private _paintedCols: number | null = null;
  private _paintedAltScreen: boolean | null = null;
  private _paintedCursorVisible: boolean | null = null;
  private _paintedCursorRow = -1;
  private _paintedCursorCol = -1;
  private _paintedSpacerHeight = "";
  // Reused duplicate/tail-validation stamps. Sized only on a full repair, so a
  // sparse delta allocates nothing regardless of pane count.
  private _dirtyMarks = new Uint32Array(0);
  private _dirtyMarkGeneration = 0;

  constructor(private readonly container: HTMLElement) {
    this.doc = container.ownerDocument;
    container.classList.add("wterm", "cell-grid");
    // role="log" gives the grid an IMPLICIT polite live region: a screen reader
    // announces appended output without us managing announcements, and without
    // the aria-live="polite" flood a streaming pane would otherwise produce.
    container.setAttribute("role", "log");
    this.spacerEl = this.doc.createElement("div");
    this.spacerEl.className = "cell-sb-spacer";
    this.spacerEl.style.setProperty("height", "0px");
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
    container.appendChild(this.spacerEl);
    container.appendChild(this.scrollbackEl);
    container.appendChild(this.viewportEl);
    this._lastBoxH = container.clientHeight;
    // A late webfont swap changes the line box under us — drop the cached row
    // height so the next derivation re-measures instead of anchoring on stale px,
    // and re-pin every block already in the DOM to the fresh height. Without the
    // re-pin those blocks would keep a pre-swap placeholder for the life of the
    // pane (only renderFull re-runs sizeBlock across everything), skewing
    // scrollHeight — and the placeholder is a bare length now, so the browser
    // no longer self-corrects it when a block materializes.
    // Re-sizing the placeholders moves scrollHeight, so sample the bottom BEFORE
    // and pin after — same sample-then-pin discipline as apply()/renderFull().
    void this.doc.fonts?.ready?.then(() => {
      const wasAtBottom = this.atBottom();
      this._rowH = 0;
      const rowH = this.rowHeight();
      if (rowH <= 0) return;
      for (const blk of this.scrollbackEl.children)
        sizeBlock(blk as HTMLElement, blk.children.length, rowH);
      this._syncSpacer();
      this._pinToBottom(wasAtBottom);
    });
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

  /** Compatibility dispatcher for non-hot-path callers. CellTerminal names the
   * full-repair versus sparse-delta contract explicitly. */
  apply(incoming: CellGridFrame): boolean {
    return incoming.full
      ? this.applyFullFrame(incoming)
      : this.applyDeltaFrame(incoming);
  }

  /** Apply an authoritative full repair. */
  applyFullFrame(incoming: CellGridFrame): boolean {
    if (!incoming.full || incoming.viewportRows.length !== incoming.rows) return false;
    for (let i = 0; i < incoming.viewportRows.length; i++) {
      if (incoming.viewportRows[i]!.index !== i) return false;
    }
    if (this._dirtyMarks.length !== incoming.rows) {
      this._dirtyMarks = new Uint32Array(incoming.rows);
      this._dirtyMarkGeneration = 0;
    }

    const wasAtBottom = this.atBottom();
    if (this.readerPendingFrame) {
      this.readerPendingFrame = incoming;
      return true;
    }
    const current = this.frame;
    const epochChanged = current !== null && current.gridEpoch !== incoming.gridEpoch;
    const freezeReader = current !== null && !wasAtBottom;
    if (freezeReader) {
      this.readerPendingFrame = incoming;
      return true;
    }
    this.frame = incoming;
    const pinToBottom = wasAtBottom || epochChanged;
    if (this.holding) {
      this.pendingRender = true;
      this.pendingPinToBottom ||= pinToBottom;
      return true;
    }
    this.renderFull(pinToBottom);
    return true;
  }

  /** Apply one sparse delta. Only incoming.viewportRows is hashed/patched; held
   * rows outside that list are never visited on the normal frame hot path. */
  applyDeltaFrame(incoming: CellGridFrame): boolean {
    if (incoming.full) return false;
    const base = this.readerPendingFrame ?? this.frame;
    if (!base
      || incoming.gridEpoch !== base.gridEpoch
      || incoming.cols !== base.cols
      || incoming.rows !== base.rows
      || incoming.altScreen !== base.altScreen
      || base.viewportRows.length !== base.rows
      || incoming.scrollbackRows.length !== 0) return false;

    const dirty = incoming.viewportRows;
    let generation = (this._dirtyMarkGeneration + 1) >>> 0;
    if (generation === 0) {
      this._dirtyMarks.fill(0);
      generation = 1;
    }
    this._dirtyMarkGeneration = generation;
    for (const row of dirty) {
      if (!Number.isInteger(row.index) || row.index < 0 || row.index >= base.rows
        || this._dirtyMarks[row.index] === generation) return false;
      this._dirtyMarks[row.index] = generation;
    }
    // A scrollback append is not by itself proof that the viewport shifted:
    // immutable history can advance while the visible grid stays unchanged.
    // Shift/reuse only across the exact held-head boundary; then every newly
    // exposed tail row must still be authoritative.
    const scrolled = deltaViewportShift(base, incoming);
    for (let i = base.rows - scrolled; i < base.rows; i++) {
      if (this._dirtyMarks[i] !== generation) return false;
    }
    if (!this.readerPendingFrame && !this.holding && this._rowEls.length !== base.rows) {
      return false;
    }

    const wasAtBottom = this.atBottom();
    const appended = incoming.scrollbackAppend;
    const folded = foldCellDelta(base, incoming);
    if (!folded) return false;
    if (this.readerPendingFrame) {
      this.readerPendingFrame = folded;
      return true;
    }
    this.frame = folded;
    if (this.holding) {
      this.pendingRender = true;
      return true;
    }
    if (appended.length > 0) this._appendScrollback(appended, wasAtBottom);
    this.renderDelta(dirty, scrolled);
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(wasAtBottom);
    return true;
  }

  /** Alt-screen (claude fullscreen / vim / htop) OWNS the viewport — there is no
   *  scrollback in that mode. The worker ships frame.altScreen; without honoring
   *  it the stale pre-alt scrollback sheet stays in the DOM above the viewport
   *  ("historic junk on top") and the wheel scrolls up into it. Toggle a class
   *  so CSS hides scrollback + locks scroll while alt is active; leaving alt
   *  restores both. */
  private _syncAltScreen(): void {
    const active = this.frame?.altScreen === true;
    if (active === this._paintedAltScreen) return;
    this._paintedAltScreen = active;
    this.container.classList.toggle("alt-active", active);
  }

  /** True while ANY hold is active — freezes viewport/scrollback repaints. */
  private get holding(): boolean {
    return this.selectionHold || this.armedHold;
  }

  /** Freeze/thaw DOM repaints. Held while the user has text selected over this
   *  pane (see CellTerminal's selectionchange wiring). true means releasing
   *  this hold also reconciled a pending off-bottom reader frame. */
  setSelectionHold(active: boolean): boolean {
    if (this.selectionHold === active) return false;
    this.selectionHold = active;
    return this._flushIfReleased();
  }

  /** Freeze/thaw DOM repaints while the user Cmd-hovers a terminal file link
   *  (terminal-links onArmedHoverChange). Same freeze as selection; keeps the
   *  wrapped <a> from being rebuilt out from under the cursor. */
  setArmedHold(active: boolean): boolean {
    if (this.armedHold === active) return false;
    this.armedHold = active;
    return this._flushIfReleased();
  }

  /** Flush the latest held frame after every interaction hold clears. */
  private _flushIfReleased(): boolean {
    if (this.holding) return false;
    if (this.readerPendingFrame) return this.releaseReaderFreezeAtBottom();
    if (!this.pendingRender) return false;
    const pinToBottom = this.pendingPinToBottom || this.atBottom();
    this.pendingRender = false;
    this.pendingPinToBottom = false;
    this.renderFull(pinToBottom);
    return false;
  }

  /** Rebuild the whole grid from this.frame — fresh mount / width change / reset
   *  / hold-release, where the painted DOM can't be reused. Cheap even for deep
   *  history: scrollback packs into content-visibility blocks (_appendScrollback),
   *  so the browser's layout stays O(blocks). */
  private renderFull(wasAtBottom: boolean): void {
    if (!this.frame) return;
    // Reserve the incoming frame's [0, sbBase) hole BEFORE wiping painted
    // content: the scroll maximum must never transiently collapse below
    // scrollTop, or the browser clamps the reader into blank reserved space
    // (and the resulting scroll event triggers a top-down backfill drain).
    // Runs again with the fresh row height at the end of _appendScrollback.
    this._syncSpacer();
    this._rowH = 0; // container may have been resized/re-fonted since the last measure
    this.scrollbackEl.replaceChildren();
    this._curBlock = null;
    this._curBlockRows = 0;
    this.viewportEl.replaceChildren();
    this._rowEls = [];
    this._rowHashes = [];
    this._appendScrollback(this.frame.scrollbackRows, wasAtBottom);
    this.renderViewportRepair();
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(wasAtBottom);
  }

  /** Append rows to the scrollback, packing them into content-visibility "blocks"
   *  of SB_BLOCK. Off-screen blocks skip layout/paint, so a reveal reflow or a
   *  scrollHeight read walks ~history/SB_BLOCK blocks, not every line — the fix
   *  for the pane-switch freeze. Rows are immutable + append-only, so the last
   *  block stays open until it fills. The mutable tail is never a browser anchor:
   *  Chromium otherwise follows a one-pixel-above-bottom stream without any
   *  application scroll write.
   *
   *  The OPEN tail block also opts OUT of content-visibility until it seals: a
   *  skipped subtree contributes its last-evaluated intrinsic size, and that is
   *  re-evaluated at rendering-lifecycle time, not when we append. Appending into
   *  a locked tail therefore leaves scrollHeight stale for the rest of the task —
   *  so apply()'s pre-mutation atBottom() and _pinToBottom() both read a bottom
   *  that no longer exists and bottom-follow silently latches off (reproduced on
   *  a parked deck pane: scrollTop froze at the park-time maximum while rows kept
   *  arriving). Only the tail can grow; every sealed block's placeholder is exact,
   *  so this costs at most SB_BLOCK rows of real layout per pane and leaves the
   *  deep-history win intact. Sealing is a no-op reflow precisely because the
   *  placeholder equals the block's real height. */
  private _appendScrollback(rows: readonly CellRow[], wasAtBottom: boolean): void {
    this._curBlock?.style.setProperty("overflow-anchor", "none");
    for (const r of rows) {
      if (!this._curBlock || this._curBlockRows >= SB_BLOCK) {
        if (this._curBlock) {
          sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
          this._curBlock.style.removeProperty("overflow-anchor");
          this._curBlock.style.removeProperty("content-visibility");
        }
        const blk = this.doc.createElement("div");
        blk.className = "cell-block";
        blk.style.setProperty("overflow-anchor", "none");
        blk.style.setProperty("content-visibility", "visible");
        this.scrollbackEl.appendChild(blk);
        this._curBlock = blk;
        this._curBlockRows = 0;
      }
      this._curBlock.appendChild(this._renderScrollbackRow(r));
      this._curBlockRows++;
    }
    if (this._curBlock) sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
    this._evictScrollback(wasAtBottom);
    this._syncSpacer();
  }

  /** Evict oldest whole content-visibility blocks once the held scrollback
   *  window exceeds MAX_HELD_SCROLLBACK_ROWS. Runs only when the render began
   *  at the literal bottom; readers inspecting history retain every held row.
   *  Evicted rows stay fully recoverable: bumping sbBase keeps the held-window
   *  invariant (scrollbackRows.length === scrollbackTotal - sbBase) honest, so
   *  scrollbackBackfill's onUserScrollUp re-pulls exactly the evicted range.
   *
   *  dropped = the leading block's ACTUAL child count, not a hardcoded
   *  SB_BLOCK: every backfill prepend is < SB_BLOCK rows (scrollbackBackfill
   *  fetches with endRow = sbBase+1 to include the overlap row, then strips it
   *  via rows.slice(0,-1)), so a partial block at the head is the norm after
   *  any backfill cycle, not just the final chunk. Slicing by the real count
   *  keeps scrollbackRows aligned with the painted DOM regardless of size. */
  private _evictScrollback(wasAtBottom: boolean): void {
    if (!wasAtBottom) return;
    while (this.frame && this.frame.scrollbackRows.length > MAX_HELD_SCROLLBACK_ROWS) {
      // Leading child is a .cell-block (possibly partial from backfill); the
      // open tail block _curBlock is always last, so firstElementChild is never it.
      const lead = this.scrollbackEl.firstElementChild as HTMLElement | null;
      if (!lead) break;
      const dropped = lead.children.length;
      if (dropped === 0) break; // defensive — never spin on an empty block
      lead.remove();
      // The renderer owns this.frame outright (applyDelta consumes and returns
      // it), so trim in place instead of rebuilding the frame + row array.
      this.frame.scrollbackRows.splice(0, dropped);
      this.frame.sbBase += dropped;
    }
    // The final pin belongs to apply/renderFull. This method only trims rows.
  }

  /** Insert an explicitly fetched contiguous history page above the painted
   * window. The last row must meet sbBase exactly; epoch/range validation lives
   * in scrollbackBackfill.ts. Native anchoring owns every non-bottom position. */
  prependScrollback(rows: readonly CellRow[]): void {
    const wasAtBottom = this.atBottom();
    if (!this.frame || rows.length === 0) return;
    if (rows[rows.length - 1]!.index + 1 !== this.frame.sbBase) return;
    // The tail does not mutate during a prepend, so restore it as an anchor
    // before the browser lays out the new blocks above the inspected history.
    this._curBlock?.style.removeProperty("overflow-anchor");
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
      blk.appendChild(this._renderScrollbackRow(r));
      blkRows++;
    }
    if (blk) sizeBlock(blk, blkRows, rowH);
    this.scrollbackEl.prepend(frag);
    // Not the hot path (once per backfill chunk, not per frame), so this keeps
    // the plain rebuild rather than an unshift with a `rows`-sized arg spread.
    this.frame = {
      ...this.frame,
      scrollbackRows: rows.concat(this.frame.scrollbackRows),
      sbBase: this.frame.sbBase - rows.length,
    };
    this._syncSpacer();
    this._pinToBottom(wasAtBottom);
  }

  /** Reserve the unpainted [0, sbBase) history as truthful pixel space. A page
   * prepend shrinks it by the exact rows painted; eviction grows it likewise. */
  private _syncSpacer(): void {
    const rows = this.frame ? this.frame.sbBase : 0;
    const rowH = this.rowHeight();
    const height = `${(rows * (rowH > 0 ? rowH : DEFAULT_ROW_PX)).toFixed(2)}px`;
    if (height === this._paintedSpacerHeight) return;
    this._paintedSpacerHeight = height;
    this.spacerEl.style.setProperty("height", height);
  }

  /** Immutable identity and absolute range for demand-driven history paging. */
  backfillAnchor(): BackfillAnchor | null {
    if (!this.frame) return null;
    return {
      sbBase: this.frame.sbBase,
      cols: this.frame.cols,
      total: this.frame.scrollbackTotal,
      gridEpoch: this.frame.gridEpoch,
    };
  }

  /** Seq of the last applied frame (applyDelta carries the delta's seq through),
   *  reported on viewport claims so the worker can skip a redundant snapshot. */
  heldFrameSeq(): number { return this.readerPendingFrame?.seq ?? this.frame?.seq ?? 0; }

  /** Full viewport reconciliation is reserved for authoritative repairs, hold
   * release, and explicit find-highlight changes. Normal deltas never call it. */
  private renderViewportRepair(): void {
    if (!this.frame) return;
    const rows = this.frame.viewportRows;
    const vpBase = this.frame.scrollbackTotal;
    for (let i = 0; i < rows.length; i++) {
      const hits = this._findHits.get(vpBase + i);
      const activeCol = this._activeHit?.row === vpBase + i ? this._activeHit.col : undefined;
      const hash = rowHash(rows[i]!, hits, activeCol);
      if (i < this._rowEls.length) {
        if (this._rowHashes[i] === hash) continue;
        const el = renderRow(rows[i]!, this.doc, hits, activeCol);
        this._rowEls[i]!.replaceWith(el);
        this._rowEls[i] = el;
        this._rowHashes[i] = hash;
      } else {
        const el = renderRow(rows[i]!, this.doc, hits, activeCol);
        this.viewportEl.insertBefore(el, this.cursorEl.parentElement === this.viewportEl ? this.cursorEl : null);
        this._rowEls.push(el);
        this._rowHashes.push(hash);
      }
    }
    while (this._rowEls.length > rows.length) {
      this._rowEls.pop()!.remove();
      this._rowHashes.pop();
    }
    this.attachViewportOverlays();
  }

  /** Shift scroll-reused nodes, compare only authoritative dirty rows, then
   * build any newly exposed tail. No untouched held row is read or hashed. */
  private renderDelta(dirtyRows: readonly CellRow[], scrolled: number): void {
    if (!this.frame) return;
    const rows = this.frame.viewportRows;
    const shifted = Math.min(scrolled, this._rowEls.length);
    for (let i = 0; i < shifted; i++) this._rowEls[i]!.remove();
    if (shifted > 0) {
      this._rowEls.splice(0, shifted);
      this._rowHashes.splice(0, shifted);
    }

    const vpBase = this.frame.scrollbackTotal;
    for (const row of dirtyRows) {
      const index = row.index;
      if (index >= this._rowEls.length) continue;
      const hits = this._findHits.get(vpBase + index);
      const activeCol = this._activeHit?.row === vpBase + index ? this._activeHit.col : undefined;
      const hash = rowHash(row, hits, activeCol);
      if (this._rowHashes[index] === hash) continue;
      const el = renderRow(row, this.doc, hits, activeCol);
      this._rowEls[index]!.replaceWith(el);
      this._rowEls[index] = el;
      this._rowHashes[index] = hash;
    }

    // Validation in applyDeltaFrame guarantees every missing tail index arrived
    // in dirtyRows. Read it from the already-folded canonical model once here.
    while (this._rowEls.length < rows.length) {
      const index = this._rowEls.length;
      const row = rows[index]!;
      const hits = this._findHits.get(vpBase + index);
      const activeCol = this._activeHit?.row === vpBase + index ? this._activeHit.col : undefined;
      const el = renderRow(row, this.doc, hits, activeCol);
      this.viewportEl.insertBefore(el, this.cursorEl.parentElement === this.viewportEl ? this.cursorEl : null);
      this._rowEls.push(el);
      this._rowHashes.push(rowHash(row, hits, activeCol));
    }
    this.attachViewportOverlays();
  }

  private attachViewportOverlays(): void {
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
    if (this.predictedCol === col) return;
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
    const visible = this.frame.cursorVisible;
    if (visible !== this._paintedCursorVisible) {
      this._paintedCursorVisible = visible;
      c.style.display = visible ? "block" : "none";
    }
    if (!visible) return;
    if (this.frame.cursorRow !== this._paintedCursorRow) {
      this._paintedCursorRow = this.frame.cursorRow;
      c.style.top = `${this.frame.cursorRow}lh`;
    }
    const col = this.predictedCol ?? this.frame.cursorCol;
    if (col !== this._paintedCursorCol) {
      this._paintedCursorCol = col;
      c.style.left = `${col}ch`;
    }
  }

  // Pin the painted width to the grid's cols (ch units, monospace) so a
  // wider pane letterboxes (margin) instead of stretching — no reflow.
  private setGridWidth(): void {
    if (!this.frame || this.frame.cols === this._paintedCols) return;
    this._paintedCols = this.frame.cols;
    this.container.style.setProperty("--cell-cols", String(this.frame.cols));
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

  /** Drop the cached row height so the next read re-measures. The terminal-zoom
   *  preference changes the cell box without resizing the container, so nothing
   *  else would invalidate it — and a stale height leaves every block
   *  placeholder and the spacer sized for the old font. */
  invalidateRowHeight(): void {
    this._rowH = 0;
    this._paintedSpacerHeight = "";
  }

  /** Scrollback rows are immutable and append-only, so their painted element is
   *  built once — including whatever highlights were current at the time. */
  private _renderScrollbackRow(row: CellRow): HTMLElement {
    const hits = this._findHits.get(row.index);
    const activeCol = this._activeHit?.row === row.index ? this._activeHit.col : undefined;
    return renderRow(row, this.doc, hits, activeCol);
  }

  /** Install the find result set. Viewport rows pick the change up through the
   *  row hash on the next paint; scrollback rows are immutable, so the affected
   *  ones are replaced here — bounded by the server's match cap. */
  setFindHighlights(hits: ReadonlyMap<number, FindHit[]>, active: { row: number; col: number } | null): void {
    const affected = new Set<number>();
    for (const row of this._findHits.keys()) affected.add(row);
    for (const row of hits.keys()) affected.add(row);
    if (this._activeHit) affected.add(this._activeHit.row);
    if (active) affected.add(active.row);
    this._findHits = hits;
    this._activeHit = active;
    if (!this.frame) return;
    for (const row of affected) {
      if (row < this.frame.scrollbackTotal) this._repaintScrollbackRow(row);
    }
    this.renderViewportRepair();
  }

  /** Replace one painted scrollback row in place. Walks the blocks accumulating
   *  their real child counts rather than assuming SB_BLOCK: a backfill prepend
   *  creates PARTIAL leading blocks, so index arithmetic on a fixed block size
   *  would land on the wrong line. Bounded by ~history/SB_BLOCK blocks. */
  private _repaintScrollbackRow(absIndex: number): void {
    if (!this.frame) return;
    let offset = absIndex - this.frame.sbBase;
    const row = this.frame.scrollbackRows[offset];
    if (offset < 0 || !row) return; // outside the painted window — spacer, not DOM
    for (const blk of this.scrollbackEl.children) {
      const count = blk.children.length;
      if (offset < count) {
        blk.children[offset]!.replaceWith(this._renderScrollbackRow(row));
        return;
      }
      offset -= count;
    }
  }

  /** The SECOND and LAST scrollTop writer, and the ONLY one besides
   *  _pinToBottom. It runs exclusively from an explicit user gesture (find
   *  next/prev, clicking a result) — NEVER from apply(), a resize, a reveal, a
   *  hold release, or any frame path. That distinction is exactly what keeps the
   *  L11 scroll-lurch class closed. Exact because _syncSpacer reserves
   *  [0, sbBase) in pixels, so an absolute scrollback index maps to a fixed
   *  content offset for the life of the epoch. One assignment, no smooth-scroll,
   *  no scrollIntoView (which would also scroll ancestors), no correction pass. */
  scrollToScrollbackRow(absIndex: number): void {
    const rowH = this.rowHeight();
    if (rowH <= 0) return;
    const top = this.spacerEl.offsetTop + absIndex * rowH;
    const max = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    this.container.scrollTop = Math.max(0, Math.min(top - this.container.clientHeight / 3, max));
  }

  /** Name the region for assistive tech. The owner sets it from the session's
   *  display title, which is the only thing that distinguishes one pane's log
   *  from another's. */
  setAccessibleLabel(label: string): void {
    this.container.setAttribute("aria-label", label);
  }

  /** The sole production scrollTop writer. Native scrolling and browser
   *  anchoring own every non-bottom position; a render only follows output when
   *  it began at the literal maximum. */
  private _pinToBottom(wasAtBottom: boolean): void {
    if (!wasAtBottom) return;
    const bottom = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    if (this.container.scrollTop !== bottom) this.container.scrollTop = bottom;
  }

  /** Is the viewport at the literal maximum scroll offset? */
  atBottom(): boolean {
    const el = this.container;
    return el.scrollTop >= Math.max(0, el.scrollHeight - el.clientHeight);
  }

  /** Reconcile a frozen off-bottom reader only after they reach the bottom of
   * the grid they were actually reading. Until then no streaming frame mutates
   * its DOM or scroll geometry. */
  releaseReaderFreezeAtBottom(): boolean {
    if (!this.readerPendingFrame || this.holding || !this.atBottom()) return false;
    this.frame = this.readerPendingFrame;
    this.readerPendingFrame = null;
    this.pendingRender = false;
    this.pendingPinToBottom = false;
    this.renderFull(true);
    return true;
  }

  /** Container box changed (deck restyle, window resize, divider drag, keyboard
   *  inset). A reader at the OLD box's literal bottom follows to the new bottom;
   *  anyone else is untouched. max(prev, next) covers both directions: a shrink
   *  leaves scrollTop below the new larger maximum, a grow clamps scrollTop onto
   *  the new bottom — both read as at-bottom against the larger of the two. */
  noteBoxResize(): void {
    const el = this.container;
    const h = el.clientHeight;
    const prev = this._lastBoxH;
    if (h > 0) this._lastBoxH = h;
    if (prev <= 0 || h <= 0 || h === prev) return;
    const wasAtBottom = el.scrollTop >= Math.max(0, el.scrollHeight - Math.max(prev, h));
    this._pinToBottom(wasAtBottom);
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
    this.spacerEl.remove();
    this.scrollbackEl.remove();
    this.viewportEl.remove();
    this.frame = null;
    this.readerPendingFrame = null;
    this.pendingPinToBottom = false;
    this._rowEls = [];
    this._rowHashes = [];
  }
}
