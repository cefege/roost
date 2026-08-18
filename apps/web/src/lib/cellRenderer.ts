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
  spansText,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import { renderRow, rowHash, type FindHit } from "./cellRow.ts";
import type { TerminalCellGeometry } from "./terminalMouse.ts";

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

export interface RendererEpochSeq {
  grid_epoch: string | null;
  seq: number | null;
}

export type ReaderIntent = "live" | "reading";
export type ReaderIntentReason =
  | "native_scroll"
  | "wheel"
  | "touch"
  | "selection"
  | "find";

export const RENDERER_HOLD_SELECTION = 1;
export const RENDERER_HOLD_LINK = 2;

export interface LiveInteractionResult {
  reconciled: boolean;
  anchorChanged: boolean;
}

const NO_LIVE_INTERACTION_RESULT: LiveInteractionResult =
  Object.freeze({ reconciled: false, anchorChanged: false });

export type ReconcileBlockReason =
  | "reader_pending_frame"
  | "selection_hold"
  | "link_hold"
  | "selection_and_link_hold"
  | "predicted_cursor"
  | "pending_render"
  | "not_reconciled"
  | null;

export interface RendererTerminalModeSnapshot {
  alt_screen: boolean;
  cursor_keys_app: boolean;
  bracketed_paste: boolean;
}

export interface RendererPresentationSnapshot {
  captured_at_ms: number;
  canonical: RendererEpochSeq;
  reconciled: RendererEpochSeq;
  reader_intent: ReaderIntent;
  reader_reason: ReaderIntentReason | null;
  hold_mask: { selection: boolean; link: boolean };
  rows: { canonical: number | null; dom: number };
  mode: {
    canonical: RendererTerminalModeSnapshot | null;
    reconciled: RendererTerminalModeSnapshot | null;
  };
  cursor: {
    canonical: { visible: boolean; row: number; column: number } | null;
    dom: {
      visible: boolean | null;
      row: number | null;
      column: number | null;
      connected: boolean;
    };
  };
  cols: { canonical: number | null; dom: number | null };
  at_bottom: boolean;
}



export class CellGridRenderer {
  private frame: CellGridFrame | null = null;
  // Canonical frames continue advancing while explicit reading keeps the DOM
  // immutable. readerIntent, never incidental scroll geometry, decides which
  // side receives an accepted frame.
  private readerPendingFrame: CellGridFrame | null = null;
  private _readerIntent: ReaderIntent = "live";
  private _readerReason: ReaderIntentReason | null = null;
  // Selection and armed-link interactions are independent bits. Keeping them
  // composed lets prepareLiveInteraction clear both atomically and guarantees
  // that one admitted input causes at most one repair.
  private _holdMask = 0;
  private pendingRender = false;
  // Browser scroll events are asynchronous and renderer writes may coalesce.
  // A nonzero epoch exists only after an assignment actually changes scrollTop;
  // later no-op/clamped pins may retarget that same pending epoch to the final
  // browser value, but can never manufacture ownership on their own.
  private _nextOwnedScrollEpoch = 0;
  private _ownedScrollEpoch = 0;
  private _ownedScrollTop = 0;
  // Clearing a retained document Selection while the terminal textarea owns
  // focus can make Chromium reveal the editing surface at scrollTop=0 after
  // animation callbacks. The source owner brackets that scroll until it arrives
  // or a later admitted input / explicit reader gesture supersedes it.
  private _liveSelectionReleasePending = false;
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
  // Absolute history range represented by scrollbackEl. Canonical state may
  // advance under a hold, so it cannot be derived from this.frame then.
  private _paintedSbBase = 0;
  private _paintedScrollbackTotal = 0;
  // Reused duplicate/tail-validation stamps. Sized only on a full repair, so a
  // sparse delta allocates nothing regardless of pane count.
  private _dirtyMarks = new Uint32Array(0);
  private _dirtyMarkGeneration = 0;
  // Canonical frame acceptance and DOM reconciliation are deliberately
  // separate watermarks. Holds may advance the former while leaving the
  // latter unchanged; scalar storage keeps the frame hot path allocation-free.
  private _reconciledGridEpoch: string | null = null;
  private _reconciledSeq: number | null = null;
  private _reconciledAltScreen: boolean | null = null;
  private _reconciledCursorKeysApp: boolean | null = null;
  private _reconciledBracketedPaste: boolean | null = null;

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

    if (this._readerIntent === "reading" || this.readerPendingFrame) {
      this.readerPendingFrame = incoming;
      if (this._readerIntent === "live") this.pendingRender = true;
      return true;
    }
    this.frame = incoming;
    if (this.holding) {
      this.pendingRender = true;
      return true;
    }
    // Live intent is persistent. A resize may have moved the literal maximum
    // between layout and this frame; geometry alone must never turn output into
    // a reader freeze.
    this.renderFull(true);
    return true;
  }

  /** Apply one sparse delta. Only incoming.viewportRows is hashed/patched; held
   * rows outside that list are never visited on the normal frame hot path. */
  applyDeltaFrame(incoming: CellGridFrame): boolean {
    if (incoming.full) return false;
    let base = this.readerPendingFrame ?? this.frame;
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
    if (
      this._readerIntent === "live"
      && !this.holding
      && this._rowEls.length !== base.rows
    ) return false;

    const appended = incoming.scrollbackAppend;
    // applyDelta owns and mutates its base. On the first delta of a reading
    // interval, copy the frame shell/row coordinates so the model backing the
    // frozen DOM stays immutable. Span arrays remain shared and immutable.
    if (this._readerIntent === "reading" && !this.readerPendingFrame) {
      base = this._copyFrameForReader(base);
    }
    const folded = foldCellDelta(base, incoming);
    if (!folded) return false;
    if (this._readerIntent === "reading" || this.readerPendingFrame) {
      this.readerPendingFrame = folded;
      if (this._readerIntent === "live") this.pendingRender = true;
      return true;
    }
    this.frame = folded;
    if (this.holding) {
      this.pendingRender = true;
      return true;
    }
    if (appended.length > 0) this._appendScrollback(appended, true);
    this.renderDelta(dirty, scrolled);
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(true);
    this._recordPaintedHistory();
    this._markReconciledIfCurrent();
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

  /** True while ANY interaction hold freezes viewport/scrollback repaints. */
  private get holding(): boolean {
    return this._holdMask !== 0;
  }

  get readerIntent(): ReaderIntent {
    return this._readerIntent;
  }

  get readerReason(): ReaderIntentReason | null {
    return this._readerReason;
  }

  /** Public composed mask for owner diagnostics and atomic interaction cleanup. */
  get holdMask(): number {
    return this._holdMask;
  }

  /** Begin an explicit reading interval before a gesture can race a frame. */
  enterReading(reason: ReaderIntentReason): void {
    // Explicit intent always wins a still-pending lifecycle classification.
    this._liveSelectionReleasePending = false;
    this._readerIntent = "reading";
    this._readerReason = reason;
  }

  /** Freeze/thaw DOM repaints while selection owns either endpoint in this pane. */
  setSelectionHold(active: boolean): LiveInteractionResult {
    const held = (this._holdMask & RENDERER_HOLD_SELECTION) !== 0;
    if (held === active) return NO_LIVE_INTERACTION_RESULT;
    if (active) {
      this._holdMask |= RENDERER_HOLD_SELECTION;
      this.enterReading("selection");
      return NO_LIVE_INTERACTION_RESULT;
    }
    this._holdMask &= ~RENDERER_HOLD_SELECTION;
    return this._flushIfReleased();
  }

  /** Freeze/thaw DOM repaints while Cmd-hover keeps a terminal link stable. */
  setArmedHold(active: boolean): LiveInteractionResult {
    const held = (this._holdMask & RENDERER_HOLD_LINK) !== 0;
    if (held === active) return NO_LIVE_INTERACTION_RESULT;
    if (active) {
      this._holdMask |= RENDERER_HOLD_LINK;
      return NO_LIVE_INTERACTION_RESULT;
    }
    this._holdMask &= ~RENDERER_HOLD_LINK;
    return this._flushIfReleased();
  }

  /** Flush the latest canonical frame after every hold clears, but never cancel
   * a persistent explicit reading interval. */
  private _flushIfReleased(): LiveInteractionResult {
    if (this.holding || this._readerIntent === "reading") {
      return NO_LIVE_INTERACTION_RESULT;
    }
    return this._resumeLive(false);
  }

  private _copyFrameForReader(frame: CellGridFrame): CellGridFrame {
    return {
      ...frame,
      viewportRows: frame.viewportRows.map((row) => ({
        index: row.index,
        spans: row.spans,
      })),
      scrollbackRows: frame.scrollbackRows.slice(),
      scrollbackAppend: [],
    };
  }

  private _resumeLive(clearHolds: boolean): LiveInteractionResult {
    const before = this.backfillAnchor();
    this._readerIntent = "live";
    this._readerReason = null;
    if (clearHolds) this._holdMask = 0;
    if (this.holding) return NO_LIVE_INTERACTION_RESULT;

    if (this.readerPendingFrame) {
      this.frame = this.readerPendingFrame;
      this.readerPendingFrame = null;
      this.pendingRender = true;
    }

    let reconciled = false;
    const frame = this.frame;
    if (
      frame
      && (
        this.pendingRender
        || this._reconciledGridEpoch !== frame.gridEpoch
        || this._reconciledSeq !== frame.seq
      )
    ) {
      this.pendingRender = false;
      this._reconcileCanonical(true);
      reconciled = true;
    } else {
      this._pinToBottom(true);
    }

    const after = this.backfillAnchor();
    const anchorChanged = before?.gridEpoch !== after?.gridEpoch
      || before?.cols !== after?.cols
      || before?.total !== after?.total
      || before?.sbBase !== after?.sbBase;
    if (!reconciled && !anchorChanged) return NO_LIVE_INTERACTION_RESULT;
    return { reconciled, anchorChanged };
  }

  /** Reconcile a stale canonical frame without throwing away clean viewport
   * rows. The explicit-reading/hold path is cold, so it may inspect all rows;
   * normal deltas retain their sparse O(dirty) path. */
  private _reconcileCanonical(pinToBottom: boolean): void {
    const frame = this.frame;
    if (!frame) return;
    const sameEpoch = this._reconciledGridEpoch === frame.gridEpoch;
    let canExtendHistory = sameEpoch
      && this._paintedSbBase === frame.sbBase
      && this._paintedScrollbackTotal <= frame.scrollbackTotal;
    const appendOffset = this._paintedScrollbackTotal - frame.sbBase;
    if (
      canExtendHistory
      && (appendOffset < 0 || appendOffset > frame.scrollbackRows.length)
    ) canExtendHistory = false;
    if (canExtendHistory) {
      for (let i = appendOffset; i < frame.scrollbackRows.length; i++) {
        if (frame.scrollbackRows[i]!.index !== frame.sbBase + i) {
          canExtendHistory = false;
          break;
        }
      }
    }

    if (canExtendHistory) {
      if (appendOffset < frame.scrollbackRows.length) {
        this._appendScrollback(frame.scrollbackRows.slice(appendOffset), pinToBottom);
      } else {
        this._syncSpacer();
      }
    } else {
      // Grow the spacer to the incoming reserve before wiping history so the
      // browser cannot clamp a reader into a transiently collapsed scroll box.
      this._syncSpacer();
      this.scrollbackEl.replaceChildren();
      this._curBlock = null;
      this._curBlockRows = 0;
      this._appendScrollback(frame.scrollbackRows, pinToBottom);
    }

    if (!sameEpoch) {
      this.viewportEl.replaceChildren();
      this._rowEls = [];
      this._rowHashes = [];
    }
    this.renderViewportRepair();
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(pinToBottom);
    this._recordPaintedHistory();
    this._markReconciledIfCurrent();
  }

  /** Atomically return an admitted local PTY interaction to persistent live
   * rendering. Both interaction holds clear before the single stale check. */
  prepareLiveInteraction(): LiveInteractionResult {
    this._liveSelectionReleasePending = false;
    return this._resumeLive(true);
  }

  /** Bracket the native scroll Chromium emits when an admitted interaction
   * clears this pane's retained Selection while its terminal textarea is
   * focused. The matching scroll consumes it; every later admitted input or
   * explicit reader interaction clears an unused bracket first. */
  beginLiveSelectionRelease(): void {
    this._liveSelectionReleasePending = this._readerIntent === "live";
  }

  finishLiveSelectionRelease(): void {
    this._liveSelectionReleasePending = false;
  }

  /** Rebuild the whole grid from this.frame for a fresh mount or authoritative
   * reset. Held/reader reconciliation uses _reconcileCanonical to retain every
   * clean row identity. Scrollback blocks keep deep-history layout bounded. */
  private renderFull(followTail: boolean): void {
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
    this._appendScrollback(this.frame.scrollbackRows, followTail);
    this.renderViewportRepair();
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(followTail);
    this._recordPaintedHistory();
    this._markReconciledIfCurrent();
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
   *  a locked tail therefore leaves scrollHeight stale for the rest of the task,
   *  so even an explicit live pin can target a bottom that no longer exists.
   *  Only the tail can grow; every sealed block's placeholder is exact, so this
   *  costs at most SB_BLOCK rows of real layout per pane and leaves the
   *  deep-history win intact. Sealing is a no-op reflow precisely because the
   *  placeholder equals the block's real height. */
  private _appendScrollback(rows: readonly CellRow[], followTail: boolean): void {
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
    this._evictScrollback(followTail);
    this._syncSpacer();
  }

  /** Evict oldest whole content-visibility blocks once the held scrollback
   *  window exceeds MAX_HELD_SCROLLBACK_ROWS. Runs only while rendering follows
   *  the live tail; explicit readers retain every held row. Evicted rows stay
   *  fully recoverable: bumping sbBase keeps the held-window invariant
   *  (scrollbackRows.length === scrollbackTotal - sbBase) honest, so
   *  scrollbackBackfill's onUserScrollUp re-pulls exactly the evicted range.
   *
   *  dropped = the leading block's ACTUAL child count, not a hardcoded
   *  SB_BLOCK: every backfill prepend is < SB_BLOCK rows (scrollbackBackfill
   *  fetches with endRow = sbBase+1 to include the overlap row, then strips it
   *  via rows.slice(0,-1)), so a partial block at the head is the norm after
   *  any backfill cycle, not just the final chunk. Slicing by the real count
   *  keeps scrollbackRows aligned with the painted DOM regardless of size. */
  private _evictScrollback(followTail: boolean): void {
    if (!followTail) return;
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

  private _recordPaintedHistory(): void {
    if (!this.frame) {
      this._paintedSbBase = 0;
      this._paintedScrollbackTotal = 0;
      return;
    }
    this._paintedSbBase = this.frame.sbBase;
    this._paintedScrollbackTotal = this.frame.scrollbackTotal;
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
    this._recordPaintedHistory();
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

  /** Latest accepted canonical model watermark. A reader-pending repair is
   * canonical even though it has not changed the DOM. */
  canonicalEpochSeq(): RendererEpochSeq {
    const frame = this._canonicalFrame();
    return {
      grid_epoch: frame?.gridEpoch ?? null,
      seq: frame?.seq ?? null,
    };
  }

  /** Latest frame whose rows, mode, width, and authoritative cursor completed
   * DOM reconciliation. This is not a raster-paint claim. */
  reconciledEpochSeq(): RendererEpochSeq {
    return {
      grid_epoch: this._reconciledGridEpoch,
      seq: this._reconciledSeq,
    };
  }

  /** Why the canonical model is currently prevented from reconciling to DOM. */
  reconcileBlockReason(): ReconcileBlockReason {
    if (this.readerPendingFrame) return "reader_pending_frame";
    const selection = (this._holdMask & RENDERER_HOLD_SELECTION) !== 0;
    const link = (this._holdMask & RENDERER_HOLD_LINK) !== 0;
    if (selection && link) return "selection_and_link_hold";
    if (selection) return "selection_hold";
    if (link) return "link_hold";
    const frame = this.frame;
    if (frame && this.predictedCol !== null && this.predictedCol !== frame.cursorCol) {
      return "predicted_cursor";
    }
    if (this.pendingRender) return "pending_render";
    const canonical = this.canonicalEpochSeq();
    if (
      canonical.grid_epoch !== this._reconciledGridEpoch
      || canonical.seq !== this._reconciledSeq
    ) return "not_reconciled";
    return null;
  }

  /** Bounded scalar snapshot of canonical model versus reconciled DOM state. */
  presentationSnapshot(): RendererPresentationSnapshot {
    const canonical = this._canonicalFrame();
    const reconciledMode = this._reconciledAltScreen === null
      || this._reconciledCursorKeysApp === null
      || this._reconciledBracketedPaste === null
      ? null
      : {
        alt_screen: this._reconciledAltScreen,
        cursor_keys_app: this._reconciledCursorKeysApp,
        bracketed_paste: this._reconciledBracketedPaste,
      };
    return {
      captured_at_ms: Date.now(),
      canonical: this.canonicalEpochSeq(),
      reconciled: this.reconciledEpochSeq(),
      reader_intent: this._readerIntent,
      reader_reason: this._readerReason,
      hold_mask: {
        selection: (this._holdMask & RENDERER_HOLD_SELECTION) !== 0,
        link: (this._holdMask & RENDERER_HOLD_LINK) !== 0,
      },
      rows: {
        canonical: canonical?.rows ?? null,
        dom: this._rowEls.length,
      },
      mode: {
        canonical: canonical ? {
          alt_screen: canonical.altScreen,
          cursor_keys_app: canonical.cursorKeysApp,
          bracketed_paste: canonical.bracketedPaste,
        } : null,
        reconciled: reconciledMode,
      },
      cursor: {
        canonical: canonical ? {
          visible: canonical.cursorVisible,
          row: canonical.cursorRow,
          column: canonical.cursorCol,
        } : null,
        dom: {
          visible: this._paintedCursorVisible,
          row: this._paintedCursorVisible === true && this._paintedCursorRow >= 0
            ? this._paintedCursorRow
            : null,
          column: this._paintedCursorVisible === true && this._paintedCursorCol >= 0
            ? this._paintedCursorCol
            : null,
          connected: this.cursorEl.parentElement === this.viewportEl
            && this.container.isConnected !== false,
        },
      },
      cols: {
        canonical: canonical?.cols ?? null,
        dom: this._paintedCols,
      },
      at_bottom: this.atBottom(),
    };
  }

  private _canonicalFrame(): CellGridFrame | null {
    return this.readerPendingFrame ?? this.frame;
  }

  /** Commit the DOM watermark only from a completed reconciliation path.
   * Row correctness is established procedurally by renderViewportRepair or
   * renderDelta; the scalar checks cover the remaining mode/cursor surface. */
  private _markReconciledIfCurrent(): void {
    const frame = this.frame;
    if (
      !frame
      || this.readerPendingFrame
      || this.holding
      || this.pendingRender
      || this._rowEls.length !== frame.rows
      || this._paintedCols !== frame.cols
      || this._paintedAltScreen !== frame.altScreen
      || this._paintedCursorVisible !== frame.cursorVisible
      || (
        frame.cursorVisible
        && (
          (this.predictedCol !== null && this.predictedCol !== frame.cursorCol)
          || this._paintedCursorRow !== frame.cursorRow
          || this._paintedCursorCol !== frame.cursorCol
        )
      )
    ) return;
    this._reconciledGridEpoch = frame.gridEpoch;
    this._reconciledSeq = frame.seq;
    this._reconciledAltScreen = frame.altScreen;
    this._reconciledCursorKeysApp = frame.cursorKeysApp;
    this._reconciledBracketedPaste = frame.bracketedPaste;
  }

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
    this._markReconciledIfCurrent();
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
      c.dataset.visible = String(visible);
      c.style.display = visible ? "block" : "none";
    }
    if (!visible) return;
    if (this.frame.cursorRow !== this._paintedCursorRow) {
      this._paintedCursorRow = this.frame.cursorRow;
      c.dataset.row = String(this.frame.cursorRow);
      c.style.top = `${this.frame.cursorRow}lh`;
    }
    const col = this.predictedCol ?? this.frame.cursorCol;
    if (col !== this._paintedCursorCol) {
      this._paintedCursorCol = col;
      c.dataset.column = String(col);
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

  /** Canonical/model viewport text (one row per line). This can advance while
   *  reader/selection/link holds leave the reconciled DOM stale; it is never
   *  presentation or paint proof. */
  gridText(): string {
    const frame = this._canonicalFrame();
    if (!frame) return "";
    return frame.viewportRows
      .map((r) => spansText(r.spans))
      .join("\n");
  }

  /** Accepted model frame, or null before the first frame. A readerPendingFrame
   * may be newer, while a live interaction hold may leave this model ahead of
   * DOM; use canonicalEpochSeq and presentationSnapshot for explicit truth. */
  get currentFrame(): CellGridFrame | null { return this.frame; }

  /** Last `maxRows` scrollback lines as text (oldest→newest), capped so a 10k
   *  ring never drowns the keyterm signal. Recency-decayed by the caller. */
  scrollbackText(maxRows = 250): string {
    if (!this.frame) return "";
    const rows = this.frame.scrollbackRows;
    return rows
      .slice(Math.max(0, rows.length - maxRows))
      .map((r) => spansText(r.spans))
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

  /** Client-space geometry of the PAINTED grid, for pointer hit-testing, or
   *  null before the first frame / while the row box is unmeasurable.
   *
   *  The origin is `.cell-viewport`, NOT the `.wterm.cell-grid` scroll
   *  container: inside that container the history spacer and the append-only
   *  scrollback sheet both sit ABOVE the viewport, so the container's top is
   *  (painted history height − scrollTop) above row 1 — hundreds of px in any
   *  pane with scrollback. Hit-testing from the container reported a row that
   *  far DOWN the grid, and only looked right on a fresh alt-screen pane, where
   *  .cell-scrollback/.cell-sb-spacer are display:none.
   *
   *  Both cell dimensions are exact rather than probed: the row box is
   *  rowHeight() (cached, invalidated by zoom and by a late webfont swap), and
   *  CSS pins .cell-viewport to `cols * 1ch`, so the rect's own width divided by
   *  the frame's cols is the cell advance with no rounding to accumulate. */
  viewportCellGeometry(): TerminalCellGeometry | null {
    const frame = this._canonicalFrame();
    if (!frame) return null;
    const rowHeight = this.rowHeight();
    if (rowHeight <= 0) return null;
    const rect = this.viewportEl.getBoundingClientRect?.();
    if (!rect) return null;
    const cellWidth = rect.width / frame.cols;
    if (cellWidth <= 0) return null;
    return {
      left: rect.left,
      top: rect.top,
      cellWidth,
      rowHeight,
      cols: frame.cols,
      rows: frame.rows,
    };
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

  /** Explicit find navigation is a reader action. Enter reading before the one
   * owned write so its asynchronous scroll event cannot immediately cancel it. */
  scrollToScrollbackRow(absIndex: number): void {
    const rowH = this.rowHeight();
    if (rowH <= 0) return;
    this.enterReading("find");
    const top = this.spacerEl.offsetTop + absIndex * rowH;
    const max = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    this._writeScrollTop(Math.max(0, Math.min(top - this.container.clientHeight / 3, max)));
  }

  /** Name the region for assistive tech. The owner sets it from the session's
   *  display title, which is the only thing that distinguishes one pane's log
   *  from another's. */
  setAccessibleLabel(label: string): void {
    this.container.setAttribute("aria-label", label);
  }

  private _writeScrollTop(value: number): void {
    const before = this.container.scrollTop;
    if (before !== value) this.container.scrollTop = value;
    const after = this.container.scrollTop;
    if (after !== before) {
      if (this._ownedScrollEpoch === 0) {
        this._nextOwnedScrollEpoch += 1;
        this._ownedScrollEpoch = this._nextOwnedScrollEpoch;
      }
      this._ownedScrollTop = after;
    } else if (this._ownedScrollEpoch !== 0) {
      // Layout may clamp between two pins before their one coalesced event.
      this._ownedScrollTop = after;
    }
  }

  /** Pin only when requested and only when the live anchor actually moved. */
  private _pinToBottom(shouldPin: boolean): void {
    if (!shouldPin) return;
    const bottom = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    this._writeScrollTop(bottom);
  }

  /** Is the viewport at the literal maximum scroll offset? */
  atBottom(): boolean {
    const el = this.container;
    return el.scrollTop >= Math.max(0, el.scrollHeight - el.clientHeight);
  }

  /** Classify one active-surface scroll event. A matching renderer epoch is
   * consumed at its final coalesced position. Any mismatch clears ownership
   * before native intent is classified, so the next genuine scroll cannot be
   * swallowed by a stale pin. Clearing a retained Selection can first reveal
   * the focused off-screen textarea at scrollTop=0; that bracketed lifecycle
   * event re-pins and its resulting owned event is consumed normally. A genuine
   * return to literal bottom adopts pending canonical state synchronously. */
  handleScroll(): LiveInteractionResult {
    if (this._ownedScrollEpoch !== 0) {
      const owned = this.container.scrollTop === this._ownedScrollTop;
      this._ownedScrollEpoch = 0;
      if (owned) return NO_LIVE_INTERACTION_RESULT;
    }
    if (this._liveSelectionReleasePending && !this.atBottom()) {
      this._liveSelectionReleasePending = false;
      return this._resumeLive(false);
    }
    if (this.atBottom()) return this._resumeLive(false);
    // wheel/touch/selection/find listeners identify stronger explicit intent
    // before their native scroll event; do not degrade that reason to fallback.
    if (this._readerIntent === "live") this.enterReading("native_scroll");
    return NO_LIVE_INTERACTION_RESULT;
  }

  /** Container box changed (deck restyle, window resize, divider drag, keyboard
   * inset). Detect the OLD literal bottom from the previous box height. A frame
   * can arrive after layout but before ResizeObserver; resuming and reconciling
   * here closes that interleave without depending on a scroll event. */
  noteBoxResize(): LiveInteractionResult {
    const el = this.container;
    const h = el.clientHeight;
    const prev = this._lastBoxH;
    if (h > 0) this._lastBoxH = h;
    if (prev <= 0 || h <= 0 || h === prev) {
      return NO_LIVE_INTERACTION_RESULT;
    }
    const wasAtOldBottom =
      el.scrollTop >= Math.max(0, el.scrollHeight - Math.max(prev, h));
    if (!wasAtOldBottom) return NO_LIVE_INTERACTION_RESULT;
    // A pre-scroll wheel/touch, selection, or find interval is explicit and must
    // survive geometry. native_scroll may instead be the resize's own event.
    if (
      this._readerIntent === "reading"
      && this._readerReason !== "native_scroll"
    ) return NO_LIVE_INTERACTION_RESULT;
    return this._resumeLive(false);
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
    this._reconciledGridEpoch = null;
    this._reconciledSeq = null;
    this._reconciledAltScreen = null;
    this._reconciledCursorKeysApp = null;
    this._reconciledBracketedPaste = null;
    this._readerIntent = "live";
    this._readerReason = null;
    this._holdMask = 0;
    this._ownedScrollEpoch = 0;
    this._ownedScrollTop = 0;
    this._liveSelectionReleasePending = false;
    this.pendingRender = false;
    this._paintedSbBase = 0;
    this._paintedScrollbackTotal = 0;
    this._rowEls = [];
    this._rowHashes = [];
  }
}
