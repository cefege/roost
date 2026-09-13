// Watches renderer row mutations and scans only the soft-wrap groups they touch.
// terminal-links.ts owns user interaction, while this attachment owns scheduling,
// hidden-page recovery, and bounded hot-tail rescans for streaming terminals.
// The DOM applier remains isolated in terminal-links.dom.ts.

import { isPageVisible } from "../lib/pageVisible.ts";
import {
  linkifyTerminalRows,
  terminalRowColumns,
} from "./terminal-links.dom.ts";
import type { ResolveFile } from "./terminal-links.target.ts";

const ROW_SELECTOR = ".cell-row";
const DIRTY_LIMIT = 300;

interface TerminalLinkScannerOptions {
  resolveFile?: ResolveFile;
  githubOwnerRepo?: () => string | undefined;
}

export interface TerminalLinkScannerAttachment {
  requestCurrentScan(): void;
  setActive(active: boolean): void;
  dispose(): void;
}

type IdleScheduler = {
  requestIdleCallback?: (callback: () => void, opts?: { timeout: number }) => number;
  cancelIdleCallback?: (handle: number) => void;
};

export function attachTerminalLinkScanner(
  container: HTMLElement,
  options: TerminalLinkScannerOptions,
  initialActive = true,
): TerminalLinkScannerAttachment {
  let active = false;
  let disposed = false;
  let observing = false;
  let listeningForVisibility = false;
  let scanScheduled = false;
  let scanHandle = 0;
  let scanHandleIsIdle = false;
  let activationFrame: number | null = null;
  let hotTailScanNeeded = false;
  let discardDirtyUntilActivationScan = false;
  const dirtyRows = new Set<HTMLElement>();
  const idleWindow = window as Window & IdleScheduler;

  const cancelScan = (): void => {
    if (scanHandle !== 0) {
      if (scanHandleIsIdle) idleWindow.cancelIdleCallback?.(scanHandle);
      else cancelAnimationFrame(scanHandle);
    }
    scanHandle = 0;
    scanHandleIsIdle = false;
    scanScheduled = false;
  };

  const rowOf = (node: Node | null): HTMLElement | null => {
    const element = node instanceof HTMLElement ? node : node?.parentElement ?? null;
    return element?.closest<HTMLElement>(ROW_SELECTOR) ?? null;
  };
  const noteAdded = (node: Node): void => {
    if (!(node instanceof HTMLElement)) {
      const row = rowOf(node);
      if (row) dirtyRows.add(row);
      return;
    }
    if (node.matches(ROW_SELECTOR)) {
      dirtyRows.add(node);
      return;
    }
    for (const row of node.querySelectorAll<HTMLElement>(ROW_SELECTOR)) dirtyRows.add(row);
  };

  // Walk across cell blocks and the scrollback-to-viewport seam because a
  // logical soft-wrapped line is not confined to one renderer container.
  const previousRow = (element: HTMLElement): HTMLElement | null => {
    const sibling = element.previousElementSibling;
    if (sibling?.matches(ROW_SELECTOR)) return sibling as HTMLElement;
    const parent = element.parentElement;
    if (!parent) return null;
    let scope: Element | null = null;
    if (parent.classList.contains("cell-block")) scope = parent.previousElementSibling;
    else if (parent.classList.contains("cell-viewport")) {
      scope = parent.parentElement?.querySelector(".cell-scrollback")?.lastElementChild ?? null;
    }
    const last = scope?.lastElementChild;
    return last?.matches(ROW_SELECTOR) ? last as HTMLElement : null;
  };
  const nextRow = (element: HTMLElement): HTMLElement | null => {
    const sibling = element.nextElementSibling;
    if (sibling?.matches(ROW_SELECTOR)) return sibling as HTMLElement;
    const parent = element.parentElement;
    if (!parent) return null;
    let scope: Element | null = null;
    if (parent.classList.contains("cell-block")) {
      scope = parent.nextElementSibling;
      if (!scope) {
        scope = parent.parentElement?.parentElement?.querySelector(".cell-viewport") ?? null;
      }
    }
    const first = scope?.firstElementChild;
    return first?.matches(ROW_SELECTOR) ? first as HTMLElement : null;
  };

  // A streaming frame can touch only the newest scrollback block and viewport.
  // Bounding overflow to that tail avoids rescanning retained history each frame.
  const hotRows = (): HTMLElement[] => {
    const rows: HTMLElement[] = [];
    const newestBlock = container.querySelector(".cell-scrollback")?.lastElementChild;
    if (newestBlock) {
      for (const row of newestBlock.querySelectorAll<HTMLElement>(ROW_SELECTOR)) rows.push(row);
    }
    const viewport = container.querySelector(".cell-viewport");
    if (viewport) {
      for (const row of viewport.querySelectorAll<HTMLElement>(ROW_SELECTOR)) rows.push(row);
    }
    return rows;
  };

  const scan = (): void => {
    scanScheduled = false;
    scanHandle = 0;
    scanHandleIsIdle = false;
    if (!active || !isPageVisible()) return;
    const colsRaw = container.style.getPropertyValue("--cell-cols");
    const cols = colsRaw ? parseInt(colsRaw, 10) || 0 : 0;
    const ownerRepo = options.githubOwnerRepo?.();
    if (hotTailScanNeeded) {
      hotTailScanNeeded = false;
      const discardDirty = discardDirtyUntilActivationScan;
      discardDirtyUntilActivationScan = false;
      const hot = hotRows();
      if (discardDirty) {
        dirtyRows.clear();
      } else {
        for (const row of hot) dirtyRows.delete(row);
      }
      if (hot.length > 0) {
        linkifyTerminalRows(hot, cols, options.resolveFile, ownerRepo);
      }
      if (!discardDirty && dirtyRows.size > 0) scheduleScan();
      return;
    }
    const dirtyOverflow = dirtyRows.size > DIRTY_LIMIT;
    const hot = dirtyOverflow ? hotRows() : [];
    const hotSet = new Set(hot);
    const hotStreamOverflow = dirtyOverflow
      && hot.length > 0
      && !Array.from(dirtyRows).some((row) => row.isConnected && !hotSet.has(row));
    if (hotStreamOverflow) {
      dirtyRows.clear();
      linkifyTerminalRows(hot, cols, options.resolveFile, ownerRepo);
      return;
    }
    if (dirtyRows.size === 0) return;
    const dirty = Array.from(dirtyRows);
    dirtyRows.clear();
    const visited = new Set<HTMLElement>();
    for (const seed of dirty) {
      if (!seed.isConnected || visited.has(seed)) continue;
      let first = seed;
      while (cols > 0) {
        const previous = previousRow(first);
        if (!previous || visited.has(previous) || terminalRowColumns(previous) !== cols) break;
        first = previous;
      }
      const group: HTMLElement[] = [];
      let current: HTMLElement | null = first;
      while (current) {
        group.push(current);
        visited.add(current);
        if (cols <= 0 || terminalRowColumns(current) !== cols) break;
        current = nextRow(current);
        if (!current || visited.has(current)) break;
      }
      linkifyTerminalRows(group, cols, options.resolveFile, ownerRepo);
    }
  };

  const scheduleScan = (): void => {
    if (!active || scanScheduled) return;
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
  const requestCurrentScan = (): void => {
    if (!active) return;
    hotTailScanNeeded = true;
    if (activationFrame === null) scheduleScan();
  };

  const cancelCurrentScanAfterPaint = (): void => {
    if (activationFrame === null) return;
    cancelAnimationFrame(activationFrame);
    activationFrame = null;
  };
  const scheduleCurrentScanAfterPaint = (): void => {
    if (activationFrame !== null) return;
    activationFrame = requestAnimationFrame(() => {
      activationFrame = null;
      requestCurrentScan();
    });
  };

  const observeMutations = (mutations: MutationRecord[]): void => {
    if (!active || discardDirtyUntilActivationScan) return;
    if (!isPageVisible()) {
      requestCurrentScan();
      return;
    }
    for (const mutation of mutations) {
      if (mutation.type === "characterData") {
        const row = rowOf(mutation.target);
        if (row) dirtyRows.add(row);
        continue;
      }
      const targetRow = rowOf(mutation.target);
      if (targetRow) dirtyRows.add(targetRow);
      for (const node of mutation.addedNodes) noteAdded(node);
    }
    scheduleScan();
  };
  let observer: MutationObserver | null = null;
  const attachObserver = (): void => {
    if (observing) return;
    observer ??= new MutationObserver(observeMutations);
    observer.observe(container, { childList: true, characterData: true, subtree: true });
    observing = true;
  };
  const detachObserver = (): void => {
    if (!observing) return;
    observer?.disconnect();
    observing = false;
  };

  // Browsers may drop a hidden tab's queued animation frame. Reset both handle
  // and latch on visibility recovery so future mutation scans cannot deadlock.
  const onVisibilityChange = (): void => {
    if (!active || !isPageVisible()) return;
    cancelCurrentScanAfterPaint();
    cancelScan();
    requestCurrentScan();
  };
  const attachVisibilityListener = (): void => {
    if (listeningForVisibility) return;
    document.addEventListener("visibilitychange", onVisibilityChange);
    listeningForVisibility = true;
  };
  const detachVisibilityListener = (): void => {
    if (!listeningForVisibility) return;
    document.removeEventListener("visibilitychange", onVisibilityChange);
    listeningForVisibility = false;
  };
  const setActive = (nextActive: boolean): void => {
    if (disposed || nextActive === active) return;
    active = nextActive;
    if (!active) {
      cancelScan();
      cancelCurrentScanAfterPaint();
      dirtyRows.clear();
      hotTailScanNeeded = false;
      discardDirtyUntilActivationScan = false;
      detachObserver();
      detachVisibilityListener();
      return;
    }
    // Canonical repaint mutations may include retained history; activation owns
    // one current-tail scan instead of replaying that history as dirty rows.
    discardDirtyUntilActivationScan = true;
    attachObserver();
    attachVisibilityListener();
    scheduleCurrentScanAfterPaint();
  };

  if (initialActive) setActive(true);

  const dispose = (): void => {
    if (disposed) return;
    setActive(false);
    disposed = true;
  };
  return { requestCurrentScan, setActive, dispose };
}
