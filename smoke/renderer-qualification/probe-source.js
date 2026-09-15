// Renderer qualification projection — host-gated public Renderer input.
// browser.js mounts this against the published @wterm/dom 0.5.0 Renderer.
// It projects completed worker cell frames; it never instantiates a parser.
// A range is rendered only after its immutable absolute rows are present.

import { Renderer } from "@wterm/dom";
import { DelayedQualificationHistory, expandRow } from "./frames.ts";

const DEFAULT_CELL = { char: 32, width: 1, fg: 256, bg: 256, flags: 0 };
const OVERSCAN_ROWS = 10;

function windowRange(scrollTop, clientHeight, rowHeight, historyFloor, historyEnd) {
  const count = historyEnd - historyFloor;
  const firstVisible = Math.floor(scrollTop / rowHeight);
  const visibleRows = Math.ceil(clientHeight / rowHeight);
  const start = Math.max(0, Math.min(count, firstVisible - OVERSCAN_ROWS));
  const end = Math.max(start, Math.min(count, firstVisible + visibleRows + OVERSCAN_ROWS));
  return { start: historyFloor + start, end: historyFloor + end };
}

function sourceFor(frame, historyFloor, historyEnd, loadedRows) {
  const viewportCells = frame.viewportRows.map(expandRow);
  const viewportLengths = frame.viewportRows.map((row) => row.spans.reduce((columns, item) => columns + item.columns, 0));
  const dirtyRows = new Set(viewportCells.map((_, index) => index));
  const unavailable = (absoluteRow) => { throw new Error(`HistoryUnavailable:${absoluteRow}`); };
  return {
    getRows: () => frame.rows,
    getCols: () => frame.cols,
    getCursor: () => ({ row: frame.cursorRow, col: frame.cursorCol, visible: frame.cursorVisible }),
    getCell: (row, col) => viewportCells[row]?.[col] ?? DEFAULT_CELL,
    isDirtyRow: (row) => dirtyRows.has(row),
    clearDirty: () => dirtyRows.clear(),
    getScrollbackCount: () => historyEnd - historyFloor,
    getScrollbackLineLen: (offset) => {
      const absoluteRow = historyEnd - 1 - offset;
      const cells = loadedRows.get(absoluteRow) ?? unavailable(absoluteRow);
      return cells.length;
    },
    getScrollbackCell: (offset, col) => {
      const absoluteRow = historyEnd - 1 - offset;
      const cells = loadedRows.get(absoluteRow) ?? unavailable(absoluteRow);
      return cells[col] ?? DEFAULT_CELL;
    },
  };
}

function selectionInside(surface) {
  const selection = surface.ownerDocument.getSelection();
  return selection !== null && !selection.isCollapsed && (surface.contains(selection.anchorNode) || surface.contains(selection.focusNode));
}

export function mountQualificationProbe({ surface, grid, loading }) {
  const provider = new DelayedQualificationHistory();
  const renderer = new Renderer(grid);
  const loadedRows = new Map();
  const requestedRanges = [];
  let latestRequest = 0;
  let currentFrame = provider.frame;
  let pendingFrame = null;
  let readerAnchor = null;
  let expired = false;
  let refreshQueue = Promise.resolve();
  let lastRenderDurationMs = 0;

  const viewport = () => ({
    scrollTop: surface.scrollTop,
    clientHeight: surface.clientHeight,
    rowHeight: Number.parseFloat(surface.dataset.rowHeight ?? "18"),
    charWidth: Number.parseFloat(surface.dataset.charWidth ?? "9"),
    overscanRows: OVERSCAN_ROWS,
    scrollbackDiscardedCount: provider.floor,
  });

  const renderComplete = (scrollTop = surface.scrollTop) => {
    const measurement = { ...viewport(), scrollTop };
    const renderStartedAt = performance.now();
    renderer.render(sourceFor(currentFrame, provider.floor, provider.end, loadedRows), measurement);
    lastRenderDurationMs = performance.now() - renderStartedAt;
    loading.textContent = expired ? "History expired" : "";
  };

  const loadWindow = async (range) => {
    const requestEpoch = provider.epoch;
    const requestId = ++latestRequest;
    const missing = [];
    for (let start = range.start; start < range.end;) {
      while (start < range.end && loadedRows.has(start)) start += 1;
      const end = start;
      while (start < range.end && !loadedRows.has(start)) start += 1;
      if (end < start) missing.push({ start: end, end: start });
    }
    if (missing.length === 0) return true;
    loading.textContent = "Loading history";
    for (const missingRange of missing) {
      requestedRanges.push({ epoch: requestEpoch, ...missingRange });
      const pages = await provider.load(missingRange.start, missingRange.end, requestEpoch);
      if (requestId !== latestRequest || requestEpoch !== provider.epoch) return false;
      for (const page of pages) {
        if (page.epoch !== provider.epoch) continue;
        for (const row of page.rows) loadedRows.set(row.index, expandRow(row));
      }
    }
    return requestId === latestRequest && requestEpoch === provider.epoch;
  };

  const applyPending = async () => {
    if (!pendingFrame || selectionInside(surface)) return;
    currentFrame = pendingFrame;
    pendingFrame = null;
    loadedRows.clear();
    expired = false;
    await refresh();
  };

  const refresh = () => {
    const queued = refreshQueue.then(async () => {
      if (selectionInside(surface)) return;
      const range = windowRange(viewport().scrollTop, viewport().clientHeight, viewport().rowHeight, provider.floor, provider.end);
      if (await loadWindow(range)) renderComplete();
    });
    refreshQueue = queued.catch(() => undefined);
    return queued;
  };

  const coldBottom = async () => {
    const measurement = viewport();
    const desiredScrollTop = Math.max(0, ((provider.end - provider.floor + currentFrame.rows) * measurement.rowHeight) - measurement.clientHeight);
    const trailing = { start: Math.max(provider.floor, provider.end - OVERSCAN_ROWS), end: provider.end };
    await loadWindow(trailing);
    renderComplete(desiredScrollTop);
    surface.scrollTop = desiredScrollTop;
    await refresh();
  };

  surface.addEventListener("mouseup", () => void applyPending());
  surface.addEventListener("keyup", () => void applyPending());
  surface.addEventListener("scroll", () => void refresh());

  return {
    coldBottom,
    refresh,
    async scrollToAbsolute(absoluteRow) {
      const measurement = viewport();
      surface.scrollTop = Math.max(0, (absoluteRow - provider.floor) * measurement.rowHeight);
      await refresh();
    },
    queueFrame(cols) {
      provider.replace(`${provider.epoch}-next`, cols);
      pendingFrame = provider.frame;
      if (!selectionInside(surface)) return applyPending();
    },
    evictThrough(floor) {
      const previousFloor = provider.floor;
      provider.evictThrough(floor);
      if (readerAnchor !== null && readerAnchor < provider.floor) {
        readerAnchor = provider.floor;
        expired = true;
      }
      if (floor > previousFloor) {
        for (const absoluteRow of loadedRows.keys()) if (absoluteRow < floor) loadedRows.delete(absoluteRow);
      }
      if (readerAnchor !== null) {
        surface.scrollTop = Math.max(0, (readerAnchor - provider.floor) * viewport().rowHeight);
      }
    },
    anchor(absoluteRow) { readerAnchor = absoluteRow; },
    async stalePageThenReplace() {
      const epochOne = provider.epoch;
      const delayed = provider.load(4_000, 4_010, epochOne);
      provider.replace(`${epochOne}-stale-replaced`);
      currentFrame = provider.frame;
      loadedRows.clear();
      await delayed;
      await refresh();
    },
    collapseSelection() {
      surface.ownerDocument.getSelection()?.removeAllRanges();
      return applyPending();
    },
    snapshot() {
      return {
        markerText: grid.textContent ?? "",
        requestedRanges: [...requestedRanges],
        historyFloor: provider.floor,
        historyEnd: provider.end,
        epoch: provider.epoch,
        expired,
        selection: surface.ownerDocument.getSelection()?.toString() ?? "",
        lastRenderDurationMs,
      };
    },
    destroy() { renderer.destroy(); },
  };
}
