// CellGridRenderer paints immutable worker-width cell rows without client-side
// VT reflow. Scrollback is append-only while normal deltas patch only dirty
// viewport rows. DOM/history and diagnostic helpers live in adjacent modules.
import {
  applyDelta as foldCellDelta,
  cloneCellGridFrame,
  deltaViewportShift,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import { renderRow, rowHash, type FindHit } from "./cellRow.ts";
import {
  DEFAULT_CELL_ROW_PX as DEFAULT_ROW_PX,
  SCROLLBACK_BLOCK_ROWS as SB_BLOCK,
  blockPlaceholder,
  cellGridText,
  cellScrollbackText,
  createCellRendererElements,
  createGhostElements,
  measureCellRowHeight,
  paintedRowAt,
  sizeScrollbackBlock as sizeBlock,
  paintCellGridWidth,
  terminalViewportCellGeometry,
  syncAlternateScreen,
} from "./cellRendererDom.ts";
import {
  MAX_HELD_SCROLLBACK_ROWS,
  NO_LIVE_INTERACTION_RESULT,
  RENDERER_HOLD_LINK,
  RENDERER_HOLD_SELECTION,
  createRendererPaintPresentation,
  createRendererPresentationSnapshot,
  type BackfillAnchor,
  type LiveInteractionResult,
  type ReaderAnchor,
  readerAnchorAtScroll,
  type ReaderIntent,
  type ReaderIntentReason,
  type ReconcileBlockReason,
  type RendererEpochSeq,
  type RendererPaintPresentation,
  type RendererPresentationSnapshot,
} from "./cellRendererPresentation.ts";
import type { TerminalCellGeometry } from "./terminalMouse.ts";
export { blockPlaceholder } from "./cellRendererDom.ts";
export {
  MAX_HELD_SCROLLBACK_ROWS,
  RENDERER_HOLD_LINK,
  RENDERER_HOLD_SELECTION,
} from "./cellRendererPresentation.ts";
export type {
  BackfillAnchor,
  LiveInteractionResult,
  ReaderAnchor,
  ReaderIntent,
  ReaderIntentReason,
  ReconcileBlockReason,
  RendererEpochSeq,
  RendererPaintPresentation,
  RendererPresentationSnapshot,
  RendererTerminalModeSnapshot,
} from "./cellRendererPresentation.ts";
export class CellGridRenderer {
  private frame: CellGridFrame | null = null;
  // Canonical frames advance while explicit reading keeps the DOM immutable.
  private readerPendingFrame: CellGridFrame | null = null;
  private _readerIntent: ReaderIntent = "live";
  private _readerReason: ReaderIntentReason | null = null;
  // Global reader anchor retained across incompatible full-frame repairs.
  private _readerAnchor: ReaderAnchor | null = null;
  private _readerAnchorNeedsRestore = false;
  // Selection and armed-link holds compose into one atomic repaint gate.
  private _holdMask = 0;
  private pendingRender = false;
  // Renderer-owned scroll writes carry one asynchronous event epoch.
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
  private _replaceViewportOnReconcile = false;
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
  // Painted history is presentation-owned and may be disjoint after a
  // viewport-only repair skipped output. The DOM carries an exact-height gap
  // for every unpainted run; row objects remain ordered by global index.
  private _paintedRows: CellRow[] = [];
  private _scrollbackLayoutEnd = 0;
  private _gapRows = 0;
  private _paintedGapRowHeight = 0;
  private _tailGapEl: HTMLElement | null = null;
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
  private readonly container: HTMLElement;
  private onFirstReconcile: (() => void) | undefined;
  private onReconcile: (() => void) | undefined;
  constructor(
    container: HTMLElement,
    onFirstReconcile?: () => void,
    onReconcile?: () => void,
  ) {
    this.container = container;
    this.onFirstReconcile = onFirstReconcile;
    this.onReconcile = onReconcile;
    const elements = createCellRendererElements(container);
    this.doc = elements.doc;
    this.spacerEl = elements.spacer;
    this.scrollbackEl = elements.scrollback;
    this.viewportEl = elements.viewport;
    this.cursorEl = elements.cursor;
    this.cursorEl.dataset.blink = "false";
    this.ghostsEl = elements.ghosts;
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
      this._resizeHistoryPlaceholders(rowH);
      this._syncSpacer();
      this._pinToBottom(wasAtBottom);
    });
  }
  /** Enable or disable the focused-pane cursor blink presentation policy. */
  setCursorBlinkEnabled(enabled: boolean): void {
    const value = String(enabled);
    if (this.cursorEl.dataset.blink === value) return;
    this.cursorEl.dataset.blink = value;
  }
  /** Remote viewers' cursors (ghost cursors). Rendered in the viewport at
   *  ch/lh grid coords — same space as the real cursor. Re-attached after every
   *  renderViewport (replaceChildren wipes overlays). */
  setGhosts(ghosts: ReadonlyMap<string, { x: number; y: number; label?: string }>): void {
    const boxes = createGhostElements(this.doc, ghosts);
    this.ghostsEl.replaceChildren(...boxes);
    if (this.ghostsEl.parentElement !== this.viewportEl) this.viewportEl.appendChild(this.ghostsEl);
  }
  // ─── frame application ───
  /** Compatibility dispatcher for non-hot-path callers. CellTerminal names the
   * full-repair versus sparse-delta contract explicitly. */
  apply(incoming: CellGridFrame): boolean {
    return incoming.full
      ? this.applyFullFrame(incoming)
      : this.applyDeltaFrame(incoming);
  }
  /** Apply an authoritative full repair. Compatible viewport-only checkpoints
   * retain presentation-owned immutable history; incompatible reader repairs
   * reset immediately so backfill can page the new epoch toward its anchor. */
  applyFullFrame(incoming: CellGridFrame): boolean {
    if (!incoming.full || incoming.viewportRows.length !== incoming.rows) return false;
    for (let i = 0; i < incoming.viewportRows.length; i++) {
      if (incoming.viewportRows[i]!.index !== i) return false;
    }
    if (this._dirtyMarks.length !== incoming.rows) {
      this._dirtyMarks = new Uint32Array(incoming.rows);
      this._dirtyMarkGeneration = 0;
    }
    const owned = cloneCellGridFrame(incoming);
    const retainsHistory = this._mergePaintedHistoryInto(owned, true);
    if (retainsHistory) this._replaceViewportOnReconcile = true;
    if (this._readerIntent === "reading" || this.readerPendingFrame) {
      if ((this._readerReason === "find" || this.holding) && !retainsHistory) {
        if (!this._readerAnchorNeedsRestore) this._captureReaderAnchor();
        if (this._readerAnchor && owned.scrollbackTotal > 0) {
          this._readerAnchor.row = Math.min(
            this._readerAnchor.row,
            owned.scrollbackTotal - 1,
          );
        } else if (owned.scrollbackTotal === 0) {
          this._readerAnchor = null;
        }
        this._readerAnchorNeedsRestore = this._readerAnchor !== null;
        this.readerPendingFrame = owned;
        this.pendingRender = true;
        return true;
      }
      if (retainsHistory) {
        this.readerPendingFrame = owned;
        if (this._readerIntent === "live") this.pendingRender = true;
        return true;
      }
      if (!this._readerAnchorNeedsRestore) this._captureReaderAnchor();
      if (this._readerAnchor && owned.scrollbackTotal > 0) {
        this._readerAnchor.row = Math.min(
          this._readerAnchor.row,
          owned.scrollbackTotal - 1,
        );
      } else if (owned.scrollbackTotal === 0) {
        this._readerAnchor = null;
      }
      if (!this._readerAnchor) {
        this.readerPendingFrame = owned;
        if (this._readerIntent === "live") this.pendingRender = true;
        return true;
      }
      this._readerAnchorNeedsRestore = this._readerAnchor !== null;
      this.readerPendingFrame = null;
      this.pendingRender = false;
      this.frame = owned;
      this.renderFull(false);
      return true;
    }
    this.frame = owned;
    if (this.holding) {
      this.pendingRender = true;
      return true;
    }
    // Live intent is persistent. A resize may have moved the literal maximum
    // between layout and this frame; geometry alone must never turn output into
    // a reader freeze.
    this._reconcileCanonical(true);
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
      base = cloneCellGridFrame(base);
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
  /** Keep alternate-screen ownership reflected in the presentation container. */
  private _syncAltScreen(): void {
    this._paintedAltScreen = syncAlternateScreen(
      this.container,
      this.frame,
      this._paintedAltScreen,
    );
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
  // ─── reader-intent holds ───
  /** Begin an explicit reading interval before a gesture can race a frame. */
  enterReading(reason: ReaderIntentReason): void {
    // Explicit intent always wins a still-pending lifecycle classification.
    this._liveSelectionReleasePending = false;
    this._readerIntent = "reading";
    this._readerReason = reason;
    this._captureReaderAnchor();
  }
  private _captureReaderAnchor(): void {
    if (this._readerIntent !== "reading" || this._scrollbackLayoutEnd <= 0) {
      this._readerAnchor = null;
      return;
    }
    const rowHeight = this.rowHeight();
    if (rowHeight <= 0) return;
    this._readerAnchor = readerAnchorAtScroll(
      this.container.scrollTop,
      this.spacerEl.offsetTop,
      rowHeight,
      this._scrollbackLayoutEnd,
    );
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
  private _resumeLive(clearHolds: boolean, explicit = false): LiveInteractionResult {
    if (!explicit && this._readerReason === "find") return NO_LIVE_INTERACTION_RESULT;
    const before = this.backfillAnchor();
    this._readerIntent = "live";
    this._readerReason = null;
    if (clearHolds) this._holdMask = 0;
    this._readerAnchor = null;
    this._readerAnchorNeedsRestore = false;
    if (this.holding) return NO_LIVE_INTERACTION_RESULT;
    if (this.readerPendingFrame) {
      this._mergePaintedHistoryInto(this.readerPendingFrame, false);
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
  /** Retain presentation-owned immutable rows in a newer canonical shell.
   * A viewport-only full has no row array of its own, while a held delta may
   * already carry an unpainted suffix. Only that suffix survives beside the
   * rows proven painted in this grid/width. */
  private _mergePaintedHistoryInto(frame: CellGridFrame, _viewportOnly: boolean): boolean {
    if (
      this._reconciledGridEpoch === null
      || this._reconciledGridEpoch !== frame.gridEpoch
      || this._paintedCols !== frame.cols
      || this._paintedAltScreen !== frame.altScreen
      || frame.scrollbackTotal < this._scrollbackLayoutEnd
    ) return false;

    const suffix: CellRow[] = [];
    let next = this._scrollbackLayoutEnd;
    for (const row of frame.scrollbackRows) {
      if (row.index < this._scrollbackLayoutEnd) continue;
      if (row.index !== next || row.index >= frame.scrollbackTotal) return false;
      suffix.push(row);
      next = row.index + 1;
    }
    if (frame.scrollbackRows.length > 0 && next !== frame.scrollbackTotal) return false;
    frame.scrollbackRows = this._paintedRows.concat(suffix);
    frame.sbBase = this._paintedSbBase;
    return true;
  }
  /** Reconcile a stale canonical frame without throwing away clean viewport
   * rows. The explicit-reading/hold path is cold, so it may inspect all rows;
   * normal deltas retain their sparse O(dirty) path. */
  private _reconcileCanonical(pinToBottom: boolean): void {
    const frame = this.frame;
    if (!frame) return;
    const sameGrid = this._reconciledGridEpoch === frame.gridEpoch
      && this._paintedCols === frame.cols
      && this._paintedAltScreen === frame.altScreen;
    let canExtendHistory = sameGrid
      && this._paintedSbBase === frame.sbBase
      && this._scrollbackLayoutEnd <= frame.scrollbackTotal
      && this._paintedRows.length <= frame.scrollbackRows.length;
    if (canExtendHistory) {
      for (let i = 0; i < this._paintedRows.length; i++) {
        const painted = this._paintedRows[i]!;
        const held = frame.scrollbackRows[i]!;
        if (painted.index !== held.index || painted.spans !== held.spans) {
          canExtendHistory = false;
          break;
        }
      }
    }
    if (!canExtendHistory) {
      this.renderFull(pinToBottom);
      return;
    }
    if (this._paintedRows.length < frame.scrollbackRows.length) {
      this._appendScrollback(
        frame.scrollbackRows.slice(this._paintedRows.length),
        pinToBottom,
      );
    } else {
      this._syncSpacer();
    }
    this._extendScrollbackGap(frame.scrollbackTotal);
    frame.scrollbackRows = this._paintedRows.slice();
    frame.sbBase = this._paintedSbBase;
    if (this._replaceViewportOnReconcile) {
      this.viewportEl.replaceChildren();
      this._rowEls = [];
      this._rowHashes = [];
      this._replaceViewportOnReconcile = false;
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
    return this._resumeLive(true, true);
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

  /** Rebuild the whole grid from this.frame for a fresh mount or incompatible
   * reset. Compatible full checkpoints use _reconcileCanonical instead. */
  private renderFull(followTail: boolean): void {
    if (!this.frame) return;
    // Reserve the incoming frame's [0, sbBase) hole BEFORE wiping painted
    // content: the scroll maximum must never transiently collapse below
    // scrollTop, or the browser clamps the reader into blank reserved space.
    this._syncSpacer();
    this._rowH = 0;
    this.scrollbackEl.replaceChildren();
    this._curBlock = null;
    this._curBlockRows = 0;
    this._tailGapEl = null;
    this._paintedRows = [];
    this._gapRows = 0;
    this._paintedSbBase = this.frame.sbBase;
    this._scrollbackLayoutEnd = this.frame.sbBase;
    this.viewportEl.replaceChildren();
    this._rowEls = [];
    this._rowHashes = [];
    this._replaceViewportOnReconcile = false;
    this._appendScrollback(this.frame.scrollbackRows, followTail);
    this._extendScrollbackGap(this.frame.scrollbackTotal);
    this.frame.scrollbackRows = this._paintedRows.slice();
    this.frame.sbBase = this._paintedSbBase;
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
   * Only the tail can grow; every sealed block's exact placeholder keeps this
   * bounded without changing layout.
   */
  private _appendScrollback(rows: readonly CellRow[], followTail: boolean): void {
    this._curBlock?.style.setProperty("overflow-anchor", "none");
    for (const r of rows) {
      if (r.index < this._scrollbackLayoutEnd) continue;
      if (r.index > this._scrollbackLayoutEnd) this._extendScrollbackGap(r.index);
      this._tailGapEl = null;
      if (!this._curBlock || this._curBlockRows >= SB_BLOCK) {
        this._sealCurrentBlock();
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
      this._paintedRows.push(r);
      this._scrollbackLayoutEnd = r.index + 1;
    }
    if (this._curBlock) sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
    this._evictScrollback(followTail);
    this._syncSpacer();
  }
  private _sealCurrentBlock(): void {
    if (!this._curBlock) return;
    sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
    this._curBlock.style.removeProperty("overflow-anchor");
    this._curBlock.style.removeProperty("content-visibility");
    this._curBlock = null;
    this._curBlockRows = 0;
  }

  /** Re-pin block placeholders and exact-height gaps after font metrics move. */
  private _resizeHistoryPlaceholders(rowH: number): void {
    for (const child of this.scrollbackEl.children) {
      const el = child as HTMLElement;
      if (el.className === "cell-sb-gap") {
        const rows = Number(el.dataset.endRow) - Number(el.dataset.startRow);
        el.style.setProperty("height", blockPlaceholder(rows, rowH));
      } else {
        sizeBlock(el, el.children.length, rowH);
      }
    }
    this._paintedGapRowHeight = rowH;
  }

  /** Extend the represented global history range without inventing rows. */
  private _extendScrollbackGap(end: number): void {
    if (end <= this._scrollbackLayoutEnd) return;
    this._sealCurrentBlock();
    const start = this._scrollbackLayoutEnd;
    const added = end - start;
    this._gapRows += added;
    let gap = this._tailGapEl;
    if (!gap) {
      gap = this.doc.createElement("div");
      gap.className = "cell-sb-gap";
      gap.dataset.startRow = String(start);
      gap.style.setProperty("overflow-anchor", "none");
      this.scrollbackEl.appendChild(gap);
      this._tailGapEl = gap;
    }
    gap.dataset.endRow = String(end);
    gap.style.setProperty("height", blockPlaceholder(
      Number(gap.dataset.endRow) - Number(gap.dataset.startRow),
      this.rowHeight(),
    ));
    this._scrollbackLayoutEnd = end;
  }

  /** Evict the exact excess, trimming the leading block rather than over-evicting. */
  private _evictScrollback(followTail: boolean): void {
    if (!followTail || !this.frame) return;
    while (this._paintedRows.length > MAX_HELD_SCROLLBACK_ROWS) {
      this._collapseLeadingGaps();
      const lead = this.scrollbackEl.firstElementChild as HTMLElement | null;
      if (!lead || lead.className !== "cell-block") break;
      const dropped = Math.min(this._paintedRows.length - MAX_HELD_SCROLLBACK_ROWS, lead.children.length);
      if (dropped === 0) break;
      const nextBase = this._paintedRows[dropped - 1]!.index + 1;
      if (dropped === lead.children.length) {
        lead.remove();
      } else {
        for (let index = 0; index < dropped; index++) lead.firstElementChild?.remove();
        sizeBlock(lead, lead.children.length, this.rowHeight());
      }
      this._paintedRows.splice(0, dropped);
      this.frame.scrollbackRows.splice(0, dropped);
      this.frame.sbBase = nextBase;
      this._paintedSbBase = nextBase;
    }
    this._collapseLeadingGaps();
  }

  private _collapseLeadingGaps(): void {
    if (!this.frame) return;
    for (;;) {
      const lead = this.scrollbackEl.firstElementChild as HTMLElement | null;
      if (!lead || lead.className !== "cell-sb-gap") return;
      const start = Number(lead.dataset.startRow);
      const end = Number(lead.dataset.endRow);
      if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return;
      this._gapRows -= end - start;
      if (this._tailGapEl === lead) this._tailGapEl = null;
      lead.remove();
      this.frame.sbBase = Math.max(this.frame.sbBase, end);
      this._paintedSbBase = this.frame.sbBase;
    }
  }

  private _recordPaintedHistory(): void {
    if (!this.frame) {
      this._paintedSbBase = 0;
      return;
    }
    this._paintedSbBase = this.frame.sbBase;
  }

  /** Prepend a contiguous fetched page that meets sbBase; anchoring handles position. */
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
    this._paintedRows = rows.concat(this._paintedRows);
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
    if (rowH > 0 && rowH !== this._paintedGapRowHeight) {
      this._resizeHistoryPlaceholders(rowH);
    }
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
  /** Anchor awaiting history materialization after an incompatible full. */
  readerAnchorForBackfill(): ReaderAnchor | null {
    if (!this._readerAnchorNeedsRestore || this._readerIntent !== "reading") return null;
    return this._readerAnchor ? { ...this._readerAnchor } : null;
  }

  /** Restore only the still-current anchor and only after its row is painted. */
  restoreReaderAnchor(anchor: ReaderAnchor): boolean {
    const current = this.readerAnchorForBackfill();
    if (
      !current
      || current.row !== anchor.row
      || current.offsetPx !== anchor.offsetPx
      || !this._paintedRow(anchor.row)
    ) return false;
    const rowH = this.rowHeight();
    if (rowH <= 0) return false;
    const max = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    const target = this.spacerEl.offsetTop + anchor.row * rowH + anchor.offsetPx;
    this._writeScrollTop(Math.max(0, Math.min(target, max)));
    this._readerAnchorNeedsRestore = false;
    return true;
  }

  private _paintedRow(index: number): CellRow | null {
    return paintedRowAt(this._paintedRows, index);
  }

  /** Bounded paint probe consumed by the smoke-only adapter. */
  paintPresentation(rowLimit?: number): RendererPaintPresentation {
    if (this._readerIntent === "reading" && !this._readerAnchorNeedsRestore) {
      this._captureReaderAnchor();
    }
    return createRendererPaintPresentation({
      paintedRows: this._paintedRows,
      readerAnchor: this._readerAnchor,
      paintedSpacerHeight: this._paintedSpacerHeight,
      gapRows: this._gapRows,
      rowHeight: this.rowHeight(),
      defaultRowHeight: DEFAULT_ROW_PX,
      rowLimit,
    });
  }

  /** Sequence of the latest accepted canonical frame. */
  canonicalFrameSeq(): number {
    return this.readerPendingFrame?.seq ?? this.frame?.seq ?? 0;
  }

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
    return createRendererPresentationSnapshot({
      canonical: this._canonicalFrame(),
      canonicalWatermark: this.canonicalEpochSeq(),
      reconciledWatermark: this.reconciledEpochSeq(),
      readerIntent: this._readerIntent,
      readerReason: this._readerReason,
      holdMask: this._holdMask,
      domRows: this._rowEls.length,
      reconciledAltScreen: this._reconciledAltScreen,
      reconciledCursorKeysApp: this._reconciledCursorKeysApp,
      reconciledBracketedPaste: this._reconciledBracketedPaste,
      paintedCursorVisible: this._paintedCursorVisible,
      paintedCursorRow: this._paintedCursorRow,
      paintedCursorCol: this._paintedCursorCol,
      cursorConnected: this.cursorEl.parentElement === this.viewportEl
        && this.container.isConnected !== false,
      paintedCols: this._paintedCols,
      atBottom: this.atBottom(),
    });
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
    const firstReconcile = this._reconciledGridEpoch === null
      && this._reconciledSeq === null;
    this._reconciledGridEpoch = frame.gridEpoch;
    this._reconciledSeq = frame.seq;
    this._reconciledAltScreen = frame.altScreen;
    this._reconciledCursorKeysApp = frame.cursorKeysApp;
    this._reconciledBracketedPaste = frame.bracketedPaste;
    if (firstReconcile) {
      const callback = this.onFirstReconcile;
      this.onFirstReconcile = undefined;
      callback?.();
    }
    this.onReconcile?.();
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

  // Pin the painted width to the grid's cols so wider panes letterbox.
  private setGridWidth(): void {
    this._paintedCols = paintCellGridWidth(
      this.container,
      this.frame,
      this._paintedCols,
    );
  }

  /** Canonical/model viewport text (one row per line). This can advance while
   *  reader/selection/link holds leave the reconciled DOM stale; it is never
   *  presentation or paint proof. */
  gridText(): string {
    return cellGridText(this._canonicalFrame());
  }

  /** Accepted model frame, or null before the first frame. A readerPendingFrame
   * may be newer, while a live interaction hold may leave this model ahead of
   * DOM; use canonicalEpochSeq and presentationSnapshot for explicit truth. */
  get currentFrame(): CellGridFrame | null { return this.frame; }

  /** Last `maxRows` scrollback lines as text (oldest→newest), capped so a 10k
   *  ring never drowns the keyterm signal. Recency-decayed by the caller. */
  scrollbackText(maxRows = 250): string {
    return cellScrollbackText(this.frame, maxRows);
  }

  /** The viewport element (position:relative) — overlay host for the cursor
   *  and the predictive-echo layer. */
  get predictionHost(): HTMLElement { return this.viewportEl; }

  /** Measured px height of one cell row, cached until font metrics change. */
  rowHeight(): number {
    if (this._rowH > 0) return this._rowH;
    const height = measureCellRowHeight(this.doc, this.viewportEl);
    if (height > 0) this._rowH = height;
    return this._rowH;
  }

  /** Client-space geometry of the painted viewport grid for pointer hit-testing. */
  viewportCellGeometry(): TerminalCellGeometry | null {
    const frame = this._canonicalFrame();
    if (!frame) return null;
    return terminalViewportCellGeometry(
      frame,
      this.viewportEl,
      this.rowHeight(),
    );
  }

  /** Drop the cached row height so the next read re-measures. The terminal-zoom
   *  preference changes the cell box without resizing the container, so nothing
   *  else would invalidate it — and a stale height leaves every block
   *  placeholder and the spacer sized for the old font. */
  invalidateRowHeight(): void {
    this._rowH = 0;
    this._paintedSpacerHeight = "";
    this._paintedGapRowHeight = 0;
  }

  /** Scrollback rows are immutable and append-only, so their painted element is
   *  built once — including whatever highlights were current at the time. */
  private _renderScrollbackRow(row: CellRow): HTMLElement {
    const hits = this._findHits.get(row.index);
    const activeCol = this._activeHit?.row === row.index ? this._activeHit.col : undefined;
    const el = renderRow(row, this.doc, hits, activeCol);
    el.dataset.rowIndex = String(row.index);
    return el;
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

  /** Replace one painted scrollback row in place by its global index. */
  private _repaintScrollbackRow(absIndex: number): void {
    const row = this._paintedRow(absIndex);
    if (!row) return;
    for (const blk of this.scrollbackEl.children) {
      if ((blk as HTMLElement).className !== "cell-block") continue;
      for (const child of blk.children) {
        if ((child as HTMLElement).dataset.rowIndex === String(absIndex)) {
          child.replaceWith(this._renderScrollbackRow(row));
          return;
        }
      }
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

  // ─── scroll ownership ───
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
    // Ignore placeholder-layout clamp events while backfill restores the held row.
    if (this._readerAnchorNeedsRestore) return NO_LIVE_INTERACTION_RESULT;
    if (this._readerIntent === "reading" && this._readerReason === "find") {
      this._captureReaderAnchor();
      return NO_LIVE_INTERACTION_RESULT;
    }
    if (this._liveSelectionReleasePending && !this.atBottom()) {
      this._liveSelectionReleasePending = false;
      return this._resumeLive(false);
    }
    if (this.atBottom() && this._readerReason !== "find") return this._resumeLive(false);
    // wheel/touch/selection/find listeners identify stronger explicit intent
    // before their native scroll event; do not degrade that reason to fallback.
    if (this._readerIntent === "live") this.enterReading("native_scroll");
    this._captureReaderAnchor();
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
    this._replaceViewportOnReconcile = false;
    this._paintedSbBase = 0;
    this._paintedRows = [];
    this._scrollbackLayoutEnd = 0;
    this._gapRows = 0;
    this._tailGapEl = null;
    this._readerAnchor = null;
    this._readerAnchorNeedsRestore = false;
    this._rowEls = [];
    this._rowHashes = [];
  }
}
