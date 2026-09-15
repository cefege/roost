// CellGridRenderer paints immutable worker-width cell rows without client-side
// VT reflow. Scrollback is append-only while normal deltas patch only dirty
// viewport rows. DOM/history and diagnostic helpers live in adjacent modules.
import {
  cloneCellGridFrame,
  foldCellDeltaBatch,
  spansText,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import { renderRow, rowHash, type FindHit } from "./cellRow.ts";
import {
  cellHistoryInsertionIndex,
  hasCellHistoryRange,
  hasContiguousCellHistoryRows,
  missingCellHistoryRange,
  missingCellHistoryRanges,
  type CellHistoryRange,
} from "./cellHistoryRanges.ts";
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
  isPositionOnlyReaderReason,
  rendererReconcileBlockReason,
  sameScrollbackRow,
  transitionedViewportRows,
  visibleHistoryRowRange,
  type BackfillAnchor,
  type LiveInteractionResult,
  type ReaderAnchor,
  readerAnchorAtScroll,
  type ReaderIntent,
  type ReaderIntentReason,
  type ReconcileBlockReason,
  type RendererEpochSeq,
  type RendererIncidentObserver,
  type RendererPaintPresentation,
  type RendererPresentationSnapshot,
  type RendererProjection,
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
  private readerPendingFrameRetainsHistory = true;
  private _readerIntent: ReaderIntent = "live";
  private _readerReason: ReaderIntentReason | null = null;
  private _readerAnchor: ReaderAnchor | null = null;
  private _holdMask = 0;
  private pendingRender = false;
  private _nextOwnedScrollEpoch = 0;
  private _ownedScrollEpoch = 0;
  private _ownedScrollTop = 0;
  // Scroll maximum last observed with a position; a smaller one means a clamp.
  private _lastScrollMax = 0;
  // A pending selection-release scroll is consumed before native reader intent.
  private _liveSelectionReleasePending = false;
  // A position-only park can be clamped to bottom with no second scroll event.
  private _bottomParkSettleEpoch = 0;
  private _curBlock: HTMLElement | null = null;
  private _curBlockRows = 0;
  private readonly spacerEl: HTMLElement;
  private readonly scrollbackEl: HTMLElement;
  private readonly viewportEl: HTMLElement;
  private readonly cursorEl: HTMLElement;
  // Ghost cursors share the viewport overlay host.
  private readonly ghostsEl: HTMLElement;
  private readonly doc: Document;
  // Viewport row elements and hashes make ordinary deltas O(dirty rows).
  private _rowEls: HTMLElement[] = [];
  private _rowHashes: number[] = [];
  private _rowH = 0;
  private _lastBoxH = 0;
  // Find hits are keyed in the worker's absolute row space.
  private _findHits: ReadonlyMap<number, FindHit[]> = new Map();
  private _activeHit: { row: number; col: number } | null = null;
  private _paintedCols: number | null = null;
  private _paintedAltScreen: boolean | null = null;
  private _paintedCursorVisible: boolean | null = null;
  private _paintedCursorRow = -1;
  private _paintedCursorCol = -1;
  private _paintedSpacerHeight = "";
  private _paintedSbBase = 0;
  // The DOM may retain disjoint immutable rows separated by exact-height gaps.
  private _paintedRows: CellRow[] = [];
  private _scrollbackLayoutEnd = 0;
  private _gapRows = 0;
  private _paintedGapRowHeight = 0;
  private _tailGapEl: HTMLElement | null = null;
  // Canonical and reconciled watermarks remain separate across holds.
  private _reconciledGridEpoch: string | null = null;
  private _reconciledSeq: number | null = null;
  private _reconciledAltScreen: boolean | null = null;
  private _reconciledCursorKeysApp: boolean | null = null;
  private _reconciledBracketedPaste: boolean | null = null;
  private readonly container: HTMLElement;
  private onFirstReconcile: (() => void) | undefined;
  private onReconcile: (() => void) | undefined;
  /** Set only by an armed terminal incident recorder; null in production. */
  incidentObserver: RendererIncidentObserver | null = null;
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
    // Font swaps invalidate exact history placeholders; preserve a bottom placement.
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
  /** Attach remote cursor overlays to the viewport. */
  setGhosts(ghosts: ReadonlyMap<string, { x: number; y: number; label?: string }>): void {
    const boxes = createGhostElements(this.doc, ghosts);
    this.ghostsEl.replaceChildren(...boxes);
    if (this.ghostsEl.parentElement !== this.viewportEl) this.viewportEl.appendChild(this.ghostsEl);
  }
  /** Apply either an authoritative full or one sparse delta. */
  apply(incoming: CellGridFrame): boolean {
    return incoming.full
      ? this.applyFullFrame(incoming)
      : this.applyDeltaFrames([incoming]);
  }
  /** Apply an authoritative full while preserving a compatible painted history. */
  applyFullFrame(incoming: CellGridFrame): boolean {
    if (this.incidentObserver?.armed === true) this.incidentObserver.observe("pre_apply", "full");
    if (!incoming.full || incoming.viewportRows.length !== incoming.rows) return false;
    for (let i = 0; i < incoming.viewportRows.length; i++) {
      if (incoming.viewportRows[i]!.index !== i) return false;
    }
    const owned = cloneCellGridFrame(incoming);
    const retainsHistory = this._canRetainPaintedHistory(owned);
    if (this._readerIntent === "reading" || this.readerPendingFrame) {
      this.readerPendingFrame = owned;
      this.readerPendingFrameRetainsHistory = retainsHistory;
      if (this._readerIntent === "live") this.pendingRender = true;
      if (isPositionOnlyReaderReason(this._readerReason)) this._settleBottomPark();
      return true;
    }
    const previousFrame = this.frame;
    this.frame = owned;
    if (this.holding) {
      this.pendingRender = true;
      return true;
    }
    this._reconcileCanonical(true, false, previousFrame);
    return true;
  }
  applyDeltaFrames(deltas: readonly CellGridFrame[]): boolean {
    if (this.incidentObserver?.armed === true) this.incidentObserver.observe("pre_apply", "delta");
    const base = this.readerPendingFrame ?? this.frame;
    if (
      !base
      || base.viewportRows.length !== base.rows
      || (
        this._readerIntent === "live"
        && !this.holding
        && this._rowEls.length !== base.rows
      )
    ) return false;
    const batch = foldCellDeltaBatch(base, deltas);
    if (!batch) return false;
    const { frame, dirtyRows, scrollbackAppend, viewportShift } = batch;
    if (this._readerIntent === "reading" || this.readerPendingFrame) {
      this.readerPendingFrame = frame;
      if (this._readerIntent === "live") this.pendingRender = true;
      if (isPositionOnlyReaderReason(this._readerReason)) this._settleBottomPark();
      return true;
    }
    const wasAtBottom = this._atBottomOrOwnedPlacement();
    this.frame = frame;
    if (this.holding) {
      this.pendingRender = true;
      return true;
    }
    this._extendScrollbackGap(frame.scrollbackTotal);
    if (
      scrollbackAppend.length > 0
      && !this.insertHistoryPage(scrollbackAppend, true)
    ) {
      this.renderFull(wasAtBottom);
      return true;
    }
    this.renderDelta(dirtyRows, viewportShift);
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(wasAtBottom);
    this._markReconciledIfCurrent();
    return true;
  }
  private _syncAltScreen(): void {
    this._paintedAltScreen = syncAlternateScreen(
      this.container,
      this.frame,
      this._paintedAltScreen,
    );
  }
  private get holding(): boolean {
    return this._holdMask !== 0;
  }
  get readerIntent(): ReaderIntent {
    return this._readerIntent;
  }
  get readerReason(): ReaderIntentReason | null {
    return this._readerReason;
  }
  get holdMask(): number {
    return this._holdMask;
  }
  enterReading(reason: ReaderIntentReason): void {
    this._liveSelectionReleasePending = false;
    if (
      reason === "selection"
      && this._readerIntent === "reading"
      && this._readerReason !== null
      && this._readerReason !== "selection"
    ) {
      this._captureReaderAnchor();
      return;
    }
    this._readerIntent = "reading";
    this._readerReason = reason;
    this._captureReaderAnchor();
  }
  /** End a find interval without moving the view: the park keeps its position
   *  and becomes an ordinary scroll park any resume can release. */
  endFindReading(): void {
    if (this._readerReason === "find") this._readerReason = "native_scroll";
  }
  private _settleBottomPark(): void {
    const epoch = ++this._bottomParkSettleEpoch;
    const settle = (): void => {
      if (
        epoch !== this._bottomParkSettleEpoch
        || this._readerIntent !== "reading"
        || !isPositionOnlyReaderReason(this._readerReason)
        || this.holding
        || !this.atBottom()
      ) return;
      this._resumeLive(false);
    };
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(settle);
      return;
    }
    queueMicrotask(settle);
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
  private _flushIfReleased(): LiveInteractionResult {
    if (this.holding) return NO_LIVE_INTERACTION_RESULT;
    // A release resumes the selection park the hold itself created, and any
    // park once the box has no scroll range: there, no scroll event can exist
    // and no anchor is reachable. A park with range keeps its interval —
    // reaching the bottom, or the next frame's settle, resumes that one.
    const noRange = this.container.scrollHeight <= this.container.clientHeight;
    if (
      this._readerIntent === "reading"
      && this._readerReason !== "selection"
      && !noRange
    ) return NO_LIVE_INTERACTION_RESULT;
    return this._resumeLive(false, noRange);
  }
  private _resumeLive(clearHolds: boolean, explicit = false): LiveInteractionResult {
    if (!explicit && this._readerReason === "find") return NO_LIVE_INTERACTION_RESULT;
    if (clearHolds) this._holdMask = 0;
    // A surviving hold outranks the resume, and reader state must stay truthful
    // under it: `live`/null with a set mask hides the real block reason and
    // un-mutes the foreground-stall watchdog into a redial the mask refreezes.
    if (this.holding) return NO_LIVE_INTERACTION_RESULT;
    const pinOnResume = explicit || this._readerReason === "selection";
    const before = this.backfillAnchor();
    this._readerIntent = "live";
    this._readerReason = null;
    this._readerAnchor = null;
    let previousFrame: CellGridFrame | null = null;
    if (this.readerPendingFrame) {
      previousFrame = this.frame;
      this.frame = this.readerPendingFrame;
      this.readerPendingFrame = null;
      this.pendingRender = true;
      this.readerPendingFrameRetainsHistory = true;
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
      this._reconcileCanonical(true, pinOnResume, previousFrame);
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
  private _canRetainPaintedHistory(frame: CellGridFrame): boolean {
    if (
      this._reconciledGridEpoch === null
      || this._reconciledGridEpoch !== frame.gridEpoch
      || this._paintedCols !== frame.cols
      || this._rowEls.length !== frame.rows
      || this._paintedAltScreen !== frame.altScreen
    ) return false;
    return frame.scrollbackTotal >= this._scrollbackLayoutEnd;
  }
  private _promoteTransitionedViewportRows(
    previousFrame: CellGridFrame | null,
  ): boolean {
    const rows = transitionedViewportRows(previousFrame, this.frame);
    if (rows === null) return true;
    // A compatible checkpoint retains rows visible in the old viewport.
    // Authoritative history must agree; insertion fills only missing subranges.
    return this._insertAuthoritativeHistory(rows, false);
  }
  private _reconcileCanonical(
    followTail: boolean,
    forcePin = false,
    previousFrame: CellGridFrame | null = null,
  ): void {
    const frame = this.frame;
    if (!frame) return;
    const sameGrid = this._reconciledGridEpoch === frame.gridEpoch
      && this._paintedCols === frame.cols
      && this._rowEls.length === frame.rows
      && this._paintedAltScreen === frame.altScreen;
    const shouldPin = forcePin || (followTail && this._atBottomOrOwnedPlacement());
    if (!sameGrid || frame.scrollbackTotal < this._scrollbackLayoutEnd) {
      this.renderFull(followTail, shouldPin);
      return;
    }
    this._extendScrollbackGap(frame.scrollbackTotal);
    const authoritativeHistory = frame.full ? frame.scrollbackRows : frame.scrollbackAppend;
    if (
      !this._promoteTransitionedViewportRows(previousFrame)
      || !this._insertAuthoritativeHistory(authoritativeHistory, followTail)
    ) {
      this.renderFull(followTail, shouldPin);
      return;
    }
    this._syncSpacer();
    this.renderViewportRepair();
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(shouldPin);
    this._markReconciledIfCurrent();
  }
  prepareLiveInteraction(): LiveInteractionResult {
    this._liveSelectionReleasePending = false;
    return this._resumeLive(true, true);
  }
  beginLiveSelectionRelease(): void {
    this._liveSelectionReleasePending = this._readerIntent === "live";
  }
  finishLiveSelectionRelease(): void {
    this._liveSelectionReleasePending = false;
  }

  private renderFull(followTail: boolean, shouldPin = followTail): void {
    const frame = this.frame;
    if (!frame) return;
    // Evidence of a painted-history defect must be read BEFORE this repair
    // replaces the nodes that carry it.
    if (this.incidentObserver?.armed === true) this.incidentObserver.observe("pre_destructive", null);
    this._paintedSbBase = frame.scrollbackTotal;
    this._scrollbackLayoutEnd = frame.scrollbackTotal;
    this._gapRows = 0;
    this._tailGapEl = null;
    this._syncSpacer();
    this._rowH = 0;
    this.scrollbackEl.replaceChildren();
    this._curBlock = null;
    this._curBlockRows = 0;
    this._paintedRows = [];
    this.viewportEl.replaceChildren();
    this._rowEls = [];
    this._rowHashes = [];
    this._insertAuthoritativeHistory(frame.scrollbackRows, followTail);
    this._syncSpacer();
    this.renderViewportRepair();
    this.setGridWidth();
    this._syncAltScreen();
    this._pinToBottom(shouldPin);
    this._markReconciledIfCurrent();
  }
  private _sealCurrentBlock(retainTail = false): void {
    if (!this._curBlock) return;
    sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
    this._curBlock.style.removeProperty("overflow-anchor");
    this._curBlock.style.removeProperty("content-visibility");
    if (retainTail) return;
    this._curBlock = null;
    this._curBlockRows = 0;
  }

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

  private _gapRange(gap: HTMLElement): CellHistoryRange | null {
    const start = Number(gap.dataset.startRow);
    const end = Number(gap.dataset.endRow);
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start >= end) {
      return null;
    }
    return { start, end };
  }

  private _setGapRange(gap: HTMLElement, start: number, end: number): void {
    gap.dataset.startRow = String(start);
    gap.dataset.endRow = String(end);
    gap.style.setProperty("height", blockPlaceholder(end - start, this.rowHeight()));
  }

  private _createGap(start: number, end: number): HTMLElement {
    const gap = this.doc.createElement("div");
    gap.className = "cell-sb-gap";
    gap.style.setProperty("overflow-anchor", "none");
    this._setGapRange(gap, start, end);
    return gap;
  }

  private _insertPageBlocks(
    rows: readonly CellRow[],
    reference: HTMLElement | null,
    reuseTail: boolean,
    opensTail: boolean,
  ): void {
    let offset = 0;
    if (reuseTail && this._curBlock) {
      this._curBlock.style.setProperty("overflow-anchor", "none");
      this._curBlock.style.setProperty("content-visibility", "visible");
      while (offset < rows.length && this._curBlockRows < SB_BLOCK) {
        this._curBlock.appendChild(this._renderScrollbackRow(rows[offset++]!));
        this._curBlockRows++;
      }
      sizeBlock(this._curBlock, this._curBlockRows, this.rowHeight());
      if (this._curBlockRows === SB_BLOCK) this._sealCurrentBlock();
    }
    let lastBlock: HTMLElement | null = null;
    let lastRows = 0;
    while (offset < rows.length) {
      const block = this.doc.createElement("div");
      block.className = "cell-block";
      this.scrollbackEl.insertBefore(block, reference);
      let blockRows = 0;
      while (offset < rows.length && blockRows < SB_BLOCK) {
        block.appendChild(this._renderScrollbackRow(rows[offset++]!));
        blockRows++;
      }
      sizeBlock(block, blockRows, this.rowHeight());
      lastBlock = block;
      lastRows = blockRows;
    }
    if (opensTail && lastBlock && lastRows < SB_BLOCK) {
      lastBlock.style.setProperty("overflow-anchor", "none");
      lastBlock.style.setProperty("content-visibility", "visible");
      this._curBlock = lastBlock;
      this._curBlockRows = lastRows;
    }
  }

  private _insertPageIntoPlaceholder(
    rows: readonly CellRow[],
    start: number,
    end: number,
  ): boolean {
    const headEnd = this._paintedSbBase;
    if (start < headEnd) {
      if (end > headEnd) return false;
      const first = this.scrollbackEl.firstElementChild as HTMLElement | null;
      const tailTarget = headEnd === this._scrollbackLayoutEnd;
      if (this._curBlock && !tailTarget) this._sealCurrentBlock(true);
      this._insertPageBlocks(rows, first, false, tailTarget);
      this._paintedSbBase = start;
      if (end < headEnd) {
        const right = this._createGap(end, headEnd);
        this.scrollbackEl.insertBefore(right, first);
        this._gapRows += headEnd - end;
        if (tailTarget) this._tailGapEl = right;
      } else if (tailTarget) {
        this._tailGapEl = null;
      }
      return true;
    }

    for (let index = 0; index < this.scrollbackEl.children.length; index++) {
      const gap = this.scrollbackEl.children[index] as HTMLElement;
      if (gap.className !== "cell-sb-gap") continue;
      const range = this._gapRange(gap);
      if (!range || start < range.start || end > range.end) continue;
      const next = (this.scrollbackEl.children[index + 1] as HTMLElement | undefined) ?? null;
      const tailTarget = range.end === this._scrollbackLayoutEnd;
      const reuseTail = tailTarget && start === range.start && this._curBlock !== null;
      if (!tailTarget && this._curBlock) this._sealCurrentBlock(true);
      this._gapRows -= end - start;
      if (start === range.start) {
        if (end < range.end) {
          this._setGapRange(gap, end, range.end);
          this._insertPageBlocks(rows, gap, reuseTail, tailTarget);
          if (tailTarget) this._tailGapEl = gap;
        } else {
          gap.remove();
          this._insertPageBlocks(rows, next, reuseTail, tailTarget);
          if (tailTarget) this._tailGapEl = null;
        }
      } else {
        if (tailTarget && this._curBlock) this._sealCurrentBlock();
        this._setGapRange(gap, range.start, start);
        this._insertPageBlocks(rows, next, false, tailTarget);
        if (end < range.end) {
          const right = this._createGap(end, range.end);
          this.scrollbackEl.insertBefore(right, next);
          if (tailTarget) this._tailGapEl = right;
        } else if (tailTarget) {
          this._tailGapEl = null;
        }
      }
      return true;
    }
    return false;
  }

  insertHistoryPage(rows: readonly CellRow[], followTail: boolean): boolean {
    if (this.incidentObserver?.armed === true) this.incidentObserver.observe("pre_history_insert", null);
    const frame = this.frame;
    const start = rows[0]?.index;
    const end = start === undefined ? undefined : start + rows.length;
    if (
      !frame
      || start === undefined
      || end === undefined
      || this._scrollbackLayoutEnd !== frame.scrollbackTotal
      || !hasContiguousCellHistoryRows(rows, start, end)
    ) return false;
    const missing = missingCellHistoryRange(this._paintedRows, frame.scrollbackTotal, start);
    if (!missing || end > missing.end || !this._insertPageIntoPlaceholder(rows, start, end)) {
      return false;
    }
    this._paintedRows.splice(cellHistoryInsertionIndex(this._paintedRows, start), 0, ...rows);
    if (followTail) this._evictScrollback(true);
    this._syncSpacer();
    return true;
  }

  private _insertAuthoritativeHistory(rows: readonly CellRow[], followTail: boolean): boolean {
    if (this.incidentObserver?.armed === true) this.incidentObserver.observe("pre_history_insert", null);
    const frame = this.frame;
    const start = rows[0]?.index;
    const end = start === undefined ? undefined : start + rows.length;
    if (rows.length === 0) return true;
    if (
      !frame
      || start === undefined
      || end === undefined
      || this._scrollbackLayoutEnd !== frame.scrollbackTotal
      || !hasContiguousCellHistoryRows(rows, start, end)
      || end > frame.scrollbackTotal
    ) return false;
    for (const row of rows) {
      const painted = this._paintedRow(row.index);
      if (painted && !sameScrollbackRow(painted, row)) return false;
    }
    const missing = missingCellHistoryRanges(
      this._paintedRows,
      frame.scrollbackTotal,
      start,
      end,
    );
    for (const range of missing) {
      const from = range.start - start;
      const through = range.end - start;
      if (!this.insertHistoryPage(rows.slice(from, through), followTail && range.end === end)) {
        return false;
      }
    }
    return true;
  }

  private _extendScrollbackGap(end: number): void {
    if (end <= this._scrollbackLayoutEnd) return;
    const start = this._scrollbackLayoutEnd;
    const gap = this._tailGapEl;
    const range = gap ? this._gapRange(gap) : null;
    if (gap && range?.end === start) {
      this._setGapRange(gap, range.start, end);
    } else {
      const tail = this._createGap(start, end);
      this.scrollbackEl.appendChild(tail);
      this._tailGapEl = tail;
    }
    this._gapRows += end - start;
    this._scrollbackLayoutEnd = end;
  }

  private _evictScrollback(followTail: boolean): void {
    if (!followTail) return;
    while (this._paintedRows.length > MAX_HELD_SCROLLBACK_ROWS) {
      this._collapseLeadingGaps();
      const lead = this.scrollbackEl.firstElementChild as HTMLElement | null;
      if (!lead || lead.className !== "cell-block") break;
      const dropped = Math.min(this._paintedRows.length - MAX_HELD_SCROLLBACK_ROWS, lead.children.length);
      if (dropped === 0) break;
      const nextBase = this._paintedRows[dropped - 1]!.index + 1;
      if (dropped === lead.children.length) lead.remove();
      else {
        for (let index = 0; index < dropped; index++) lead.firstElementChild?.remove();
        sizeBlock(lead, lead.children.length, this.rowHeight());
      }
      this._paintedRows.splice(0, dropped);
      this._paintedSbBase = nextBase;
    }
    this._collapseLeadingGaps();
    const frame = this.frame;
    if (frame && frame.scrollbackRows.length > MAX_HELD_SCROLLBACK_ROWS) {
      frame.scrollbackRows.splice(0, frame.scrollbackRows.length - MAX_HELD_SCROLLBACK_ROWS);
      frame.sbBase = frame.scrollbackRows[0]?.index ?? frame.scrollbackTotal;
    }
  }

  private _collapseLeadingGaps(): void {
    for (;;) {
      const lead = this.scrollbackEl.firstElementChild as HTMLElement | null;
      if (!lead || lead.className !== "cell-sb-gap") return;
      const range = this._gapRange(lead);
      if (!range || range.start !== this._paintedSbBase) return;
      this._gapRows -= range.end - range.start;
      if (this._tailGapEl === lead) this._tailGapEl = null;
      this._paintedSbBase = range.end;
      lead.remove();
    }
  }

  private _syncSpacer(): void {
    const rowH = this.rowHeight();
    if (rowH > 0 && rowH !== this._paintedGapRowHeight) {
      this._resizeHistoryPlaceholders(rowH);
    }
    const height = `${(
      this._paintedSbBase * (rowH > 0 ? rowH : DEFAULT_ROW_PX)
    ).toFixed(2)}px`;
    if (height === this._paintedSpacerHeight) return;
    this._paintedSpacerHeight = height;
    this.spacerEl.style.setProperty("height", height);
  }

  backfillAnchor(): BackfillAnchor | null {
    if (
      !this.frame
      || (this.readerPendingFrame && !this.readerPendingFrameRetainsHistory)
    ) return null;
    return {
      sbBase: this._paintedSbBase,
      cols: this.frame.cols,
      total: this.frame.scrollbackTotal,
      gridEpoch: this.frame.gridEpoch,
    };
  }

  missingScrollbackRange(row: number): CellHistoryRange | null {
    const anchor = this.backfillAnchor();
    if (!anchor || this._scrollbackLayoutEnd !== anchor.total) return null;
    return missingCellHistoryRange(this._paintedRows, anchor.total, row);
  }

  missingScrollbackRangeAtScroll(): (CellHistoryRange & { focusRow: number }) | null {
    const anchor = this.backfillAnchor();
    if (!anchor || this._scrollbackLayoutEnd !== anchor.total) return null;
    const visible = visibleHistoryRowRange({
      scrollTop: this.container.scrollTop,
      spacerTop: this.spacerEl.offsetTop,
      clientHeight: this.container.clientHeight,
      rowHeight: this.rowHeight(),
      total: anchor.total,
    });
    if (!visible) return null;
    const gaps = missingCellHistoryRanges(
      this._paintedRows,
      anchor.total,
      visible.start,
      visible.end,
    );
    const visibleGap = gaps.at(-1);
    if (!visibleGap) return null;
    const focusRow = visibleGap.start;
    const gap = missingCellHistoryRange(this._paintedRows, anchor.total, focusRow);
    return gap ? { ...gap, focusRow } : null;
  }

  hasPaintedScrollbackRange(start: number, end: number): boolean {
    const anchor = this.backfillAnchor();
    return anchor !== null
      && this._scrollbackLayoutEnd === anchor.total
      && hasCellHistoryRange(this._paintedRows, anchor.total, start, end);
  }
  paintedScrollbackRange(start: number, end: number): Array<{ index: number; text: string }> | null {
    if (!this.hasPaintedScrollbackRange(start, end)) return null;
    return this._paintedRows
      .filter((row) => row.index >= start && row.index < end)
      .map((row) => ({ index: row.index, text: spansText(row.spans) }));
  }

  paintedScrollbackRowCount(): number {
    return this._paintedRows.length;
  }

  private _paintedRow(index: number): CellRow | null {
    return paintedRowAt(this._paintedRows, index);
  }

  paintPresentation(rowLimit?: number): RendererPaintPresentation {
    if (this._readerIntent === "reading") this._captureReaderAnchor();
    return createRendererPaintPresentation(this.rendererProjection(), rowLimit);
  }

  canonicalFrameSeq(): number {
    return this.readerPendingFrame?.seq ?? this.frame?.seq ?? 0;
  }

  canonicalEpochSeq(): RendererEpochSeq {
    const frame = this._canonicalFrame();
    return {
      grid_epoch: frame?.gridEpoch ?? null,
      seq: frame?.seq ?? null,
    };
  }

  reconciledEpochSeq(): RendererEpochSeq {
    return {
      grid_epoch: this._reconciledGridEpoch,
      seq: this._reconciledSeq,
    };
  }

  reconcileBlockReason(): ReconcileBlockReason {
    return rendererReconcileBlockReason({
      readerPending: this.readerPendingFrame !== null,
      holdMask: this._holdMask,
      predictedCol: this.predictedCol,
      cursorCol: this.frame?.cursorCol ?? null,
      pendingRender: this.pendingRender,
      canonical: this.canonicalEpochSeq(),
      reconciled: this.reconciledEpochSeq(),
    });
  }

  presentationSnapshot(): RendererPresentationSnapshot {
    return createRendererPresentationSnapshot(this.rendererProjection());
  }

  /** Single read-only view of renderer internals, for the presentation
   *  snapshot and the incident DOM reader. Callers never mutate what it
   *  returns; `paintedHistory` is the live painted model, not a copy. */
  rendererProjection(): RendererProjection {
    return {
      container: this.container,
      scrollbackEl: this.scrollbackEl,
      viewportEl: this.viewportEl,
      canonical: this._canonicalFrame(),
      applied: this.frame,
      canonicalWatermark: this.canonicalEpochSeq(),
      reconciledWatermark: this.reconciledEpochSeq(),
      readerIntent: this._readerIntent,
      readerReason: this._readerReason,
      readerAnchor: this._readerAnchor,
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
      paintedHistory: this._paintedRows,
      paintedSbBase: this._paintedSbBase,
      scrollbackLayoutEnd: this._scrollbackLayoutEnd,
      paintedSpacerHeight: this._paintedSpacerHeight,
      gapRows: this._gapRows,
      defaultRowHeight: DEFAULT_ROW_PX,
      rowHeight: this.rowHeight(),
      scrollTop: this.container.scrollTop,
      scrollHeight: this.container.scrollHeight,
      clientHeight: this.container.clientHeight,
    };
  }

  private _canonicalFrame(): CellGridFrame | null {
    return this.readerPendingFrame ?? this.frame;
  }

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
    if (this.incidentObserver?.armed === true) this.incidentObserver.observe("post_reconcile", null);
    if (firstReconcile) {
      const callback = this.onFirstReconcile;
      this.onFirstReconcile = undefined;
      callback?.();
    }
    this.onReconcile?.();
  }

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

  private predictedCol: number | null = null;
  setPredictedCursor(col: number | null): void {
    if (this.predictedCol === col) return;
    this.predictedCol = col;
    this.updateCursor();
    this._markReconciledIfCurrent();
  }

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

  private setGridWidth(): void {
    this._paintedCols = paintCellGridWidth(
      this.container,
      this.frame,
      this._paintedCols,
    );
  }

  gridText(): string {
    return cellGridText(this._canonicalFrame());
  }

  get currentFrame(): CellGridFrame | null { return this.frame; }

  scrollbackText(maxRows = 250): string {
    return cellScrollbackText(this.frame, maxRows);
  }

  get predictionHost(): HTMLElement { return this.viewportEl; }

  rowHeight(): number {
    if (this._rowH > 0) return this._rowH;
    const height = measureCellRowHeight(this.doc, this.viewportEl);
    if (height > 0) this._rowH = height;
    return this._rowH;
  }

  viewportCellGeometry(): TerminalCellGeometry | null {
    const frame = this._canonicalFrame();
    if (!frame) return null;
    return terminalViewportCellGeometry(
      frame,
      this.viewportEl,
      this.rowHeight(),
    );
  }

  invalidateRowHeight(): void {
    this._rowH = 0;
    this._paintedSpacerHeight = "";
    this._paintedGapRowHeight = 0;
  }

  private _renderScrollbackRow(row: CellRow): HTMLElement {
    const hits = this._findHits.get(row.index);
    const activeCol = this._activeHit?.row === row.index ? this._activeHit.col : undefined;
    const el = renderRow(row, this.doc, hits, activeCol);
    el.dataset.rowIndex = String(row.index);
    return el;
  }

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

  scrollToScrollbackRow(absIndex: number): void {
    if (!this.hasPaintedScrollbackRange(absIndex, absIndex + 1)) return;
    const rowH = this.rowHeight();
    if (rowH <= 0) return;
    this.enterReading("find");
    const top = this.spacerEl.offsetTop + absIndex * rowH;
    const max = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    this._writeScrollTop(Math.max(0, Math.min(top - this.container.clientHeight / 3, max)));
  }

  setAccessibleLabel(label: string): void {
    this.container.setAttribute("aria-label", label);
  }

  private _writeScrollTop(value: number): void {
    const before = this.container.scrollTop;
    if (before !== value) this.container.scrollTop = value;
    const after = this.container.scrollTop;
    if (after !== before && this._ownedScrollEpoch === 0) {
      this._nextOwnedScrollEpoch += 1;
      this._ownedScrollEpoch = this._nextOwnedScrollEpoch;
    }
    if (this._ownedScrollEpoch !== 0) this._ownedScrollTop = after;
  }

  // Preserve an exact renderer-owned placement when late geometry moves its bottom.
  private _atBottomOrOwnedPlacement(): boolean {
    return this.atBottom()
      || (this._ownedScrollEpoch !== 0 && this.container.scrollTop === this._ownedScrollTop);
  }

  private _pinToBottom(shouldPin: boolean): void {
    if (!shouldPin) return;
    const bottom = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    this._lastScrollMax = bottom;
    this._writeScrollTop(bottom);
  }

  atBottom(): boolean {
    const el = this.container;
    return el.scrollTop >= Math.max(0, el.scrollHeight - el.clientHeight);
  }

  handleScroll(): LiveInteractionResult {
    // A scroll that only follows a shrunken maximum is a clamp no gesture
    // aimed at, so an anchor park outranks it; with no range, nothing is aimed.
    const max = Math.max(0, this.container.scrollHeight - this.container.clientHeight);
    const clamped = max > 0 && max < this._lastScrollMax;
    this._lastScrollMax = max;
    let owned = false;
    if (this._ownedScrollEpoch !== 0) {
      owned = this.container.scrollTop === this._ownedScrollTop;
      const bottom = this.atBottom();
      if (!owned || bottom) this._ownedScrollEpoch = 0;
      // An owned event that landed on the bottom is the only proof a parked
      // reader returned; swallowing it leaves the pane parked with no retry.
      if (owned && !bottom) return NO_LIVE_INTERACTION_RESULT;
    }
    if (this._readerIntent === "reading" && this._readerReason === "find") {
      if (owned || clamped || !this.atBottom()) {
        this._captureReaderAnchor();
        return NO_LIVE_INTERACTION_RESULT;
      }
      return this._resumeLive(false, true);
    }
    if (this._liveSelectionReleasePending && !this.atBottom()) {
      this._liveSelectionReleasePending = false;
      return this._resumeLive(false);
    }
    if (this.atBottom()) return this._resumeLive(false);
    if (this._readerIntent === "live") {
      this.enterReading("native_scroll");
      this._settleBottomPark();
    }
    return NO_LIVE_INTERACTION_RESULT;
  }

  noteBoxResize(): LiveInteractionResult {
    const el = this.container;
    const h = el.clientHeight;
    const prev = this._lastBoxH;
    if (h > 0) this._lastBoxH = h;
    if (prev <= 0 || h <= 0 || h === prev) return NO_LIVE_INTERACTION_RESULT;
    const wasAtOldBottom =
      el.scrollTop >= Math.max(0, el.scrollHeight - Math.max(prev, h));
    if (!wasAtOldBottom) return NO_LIVE_INTERACTION_RESULT;
    // A grow that leaves no scroll range can never fire another scroll event,
    // so this observer tick is the last chance to resume; and with no range
    // there is no reader position left to protect, whatever parked it.
    if (
      this._readerIntent === "reading"
      && !isPositionOnlyReaderReason(this._readerReason)
      && el.scrollHeight > h
    ) return NO_LIVE_INTERACTION_RESULT;
    return this._resumeLive(false, true);
  }

  dispose(): void {
    this.incidentObserver = null;
    this.spacerEl.remove();
    this.scrollbackEl.remove();
    this.viewportEl.remove();
    this.frame = null;
    this.readerPendingFrame = null;
    this.readerPendingFrameRetainsHistory = true;
    this._reconciledGridEpoch = null;
    this._reconciledSeq = null;
    this._reconciledAltScreen = null;
    this._reconciledCursorKeysApp = null;
    this._reconciledBracketedPaste = null;
    this._readerIntent = "live";
    this._readerReason = null;
    this._holdMask = 0;
    this._ownedScrollEpoch = 0;
    this._bottomParkSettleEpoch += 1;
    this._ownedScrollTop = 0;
    this._liveSelectionReleasePending = false;
    this.pendingRender = false;
    this._paintedSbBase = 0;
    this._paintedRows = [];
    this._scrollbackLayoutEnd = 0;
    this._gapRows = 0;
    this._tailGapEl = null;
    this._readerAnchor = null;
    this._rowEls = [];
    this._rowHashes = [];
  }
}
