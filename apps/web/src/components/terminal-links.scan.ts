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
  requestFullScan(): void;
  dispose(): void;
}

type IdleScheduler = {
  requestIdleCallback?: (callback: () => void, opts?: { timeout: number }) => number;
  cancelIdleCallback?: (handle: number) => void;
};

export function attachTerminalLinkScanner(
  container: HTMLElement,
  options: TerminalLinkScannerOptions,
): TerminalLinkScannerAttachment {
  let scanScheduled = false;
  let scanHandle = 0;
  let scanHandleIsIdle = false;
  let fullScanNeeded = true;
  const dirtyRows = new Set<HTMLElement>();
  const idleWindow = window as Window & IdleScheduler;

  const cancelScan = (): void => {
    if (!scanHandle) return;
    if (scanHandleIsIdle) idleWindow.cancelIdleCallback?.(scanHandle);
    else cancelAnimationFrame(scanHandle);
    scanHandle = 0;
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
    if (!isPageVisible()) return;
    const colsRaw = container.style.getPropertyValue("--cell-cols");
    const cols = colsRaw ? parseInt(colsRaw, 10) || 0 : 0;
    const ownerRepo = options.githubOwnerRepo?.();
    const dirtyOverflow = dirtyRows.size > DIRTY_LIMIT && !fullScanNeeded;
    const hot = dirtyOverflow ? hotRows() : [];
    const hotSet = new Set(hot);
    const hotStreamOverflow = dirtyOverflow
      && hot.length > 0
      && !Array.from(dirtyRows).some((row) => row.isConnected && !hotSet.has(row));
    if (fullScanNeeded || hotStreamOverflow) {
      // Connected cold rows mean retained history materialized; scan them rather
      // than mistaking the append for a hot-stream-only overflow.
      dirtyRows.clear();
      const rows = hotStreamOverflow
        ? hot
        : Array.from(container.querySelectorAll<HTMLElement>(ROW_SELECTOR));
      fullScanNeeded = hotStreamOverflow;
      linkifyTerminalRows(rows, cols, options.resolveFile, ownerRepo);
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
  const requestFullScan = (): void => {
    fullScanNeeded = true;
    scheduleScan();
  };

  const observer = new MutationObserver((mutations) => {
    if (!isPageVisible()) {
      requestFullScan();
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
  });
  observer.observe(container, { childList: true, characterData: true, subtree: true });
  scheduleScan();

  // Browsers may drop a hidden tab's queued animation frame. Reset both handle
  // and latch on visibility recovery so future mutation scans cannot deadlock.
  const onVisibilityChange = (): void => {
    if (!isPageVisible()) return;
    cancelScan();
    scanScheduled = false;
    requestFullScan();
  };
  document.addEventListener("visibilitychange", onVisibilityChange);

  const dispose = (): void => {
    observer.disconnect();
    cancelScan();
    document.removeEventListener("visibilitychange", onVisibilityChange);
  };
  return { requestFullScan, dispose };
}
