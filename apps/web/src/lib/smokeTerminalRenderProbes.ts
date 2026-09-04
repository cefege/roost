// Terminal smoke tests must inspect what the browser actually painted, not only wire state.
// These probes read focus, rendered rows, scroll geometry, markers, and grid dimensions.
// The smoke backdoor exposes them while the renderer registry supplies bounded scrollback state.
// Keeping DOM reads together makes their browser-only dependency explicit.

import { MAX_HELD_SCROLLBACK_ROWS } from "./cellRenderer.ts";
import type { SmokeApi } from "./smokeTypes.ts";
import { rendererRegistryEntry } from "./terminalPreview.ts";

type SmokeTerminalRenderMethods = Pick<
  SmokeApi,
  | "paneFocused"
  | "viewportText"
  | "renderProbe"
  | "paintedScrollback"
  | "markerScan"
  | "terminalDimensions"
>;

export function createSmokeTerminalRenderMethods(): SmokeTerminalRenderMethods {
  return {
    paneFocused(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const textarea = slot?.querySelector("textarea") ?? null;
      return {
        hasSlot: !!slot,
        hasTextarea: !!textarea,
        focused: !!textarea && document.activeElement === textarea,
      };
    },
    viewportText(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      return (slot?.textContent ?? "").replace(/\s+/g, " ").trim();
    },
    renderProbe(sessionId) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const grid = slot?.querySelector(".cell-grid") as HTMLElement | null;
      if (!grid) {
        return {
          found: false,
          mode: "none" as const,
          scrollTop: 0,
          scrollHeight: 0,
          clientHeight: 0,
          fromBottom: 0,
          atBottom: false,
          rowCount: 0,
          nonEmptyRows: 0,
          firstLine: "",
          lastLine: "",
        };
      }
      const rows = Array.from(grid.querySelectorAll(".cell-row"))
        .map((row) => (row.textContent ?? "").replace(/ /g, " ").replace(/\s+$/, ""));
      const nonEmptyRows = rows.filter((text) => text.trim() !== "");
      const fromBottom = grid.scrollHeight - grid.scrollTop - grid.clientHeight;
      return {
        found: true,
        mode: "cell" as const,
        scrollTop: Math.round(grid.scrollTop),
        scrollHeight: Math.round(grid.scrollHeight),
        clientHeight: Math.round(grid.clientHeight),
        fromBottom: Math.round(fromBottom),
        atBottom: grid.scrollTop >= Math.max(0, grid.scrollHeight - grid.clientHeight),
        rowCount: rows.length,
        nonEmptyRows: nonEmptyRows.length,
        firstLine: nonEmptyRows[0] ?? "",
        lastLine: nonEmptyRows[nonEmptyRows.length - 1] ?? "",
      };
    },
    paintedScrollback(sessionId) {
      const renderer = rendererRegistryEntry(sessionId)?.renderer;
      return renderer?.paintPresentation(MAX_HELD_SCROLLBACK_ROWS) ?? {
        rows: [],
        headSpacerPx: 0,
        tailGapPx: 0,
        readerAnchor: null,
      };
    },
    markerScan(sessionId, prefix) {
      const slot = document.querySelector(`[data-testid="terminal-slot-${sessionId}"]`);
      const grid = slot?.querySelector(".cell-grid") as HTMLElement | null;
      const markerPattern = new RegExp(
        `${prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(\\d+)`,
        "g",
      );
      const counts = new Map<number, number>();
      const markerSequence: number[] = [];
      for (const row of grid?.querySelectorAll(".cell-row") ?? []) {
        markerPattern.lastIndex = 0;
        const text = row.textContent ?? "";
        let match: RegExpExecArray | null;
        while ((match = markerPattern.exec(text)) !== null) {
          const marker = Number(match[1]);
          if (!Number.isSafeInteger(marker)) continue;
          counts.set(marker, (counts.get(marker) ?? 0) + 1);
          markerSequence.push(marker);
        }
      }
      const markers = [...counts.keys()];
      const total = [...counts.values()].reduce((sum, count) => sum + count, 0);
      const min = markers.length > 0 ? Math.min(...markers) : 0;
      const max = markers.length > 0 ? Math.max(...markers) : 0;
      const duplicated = markers
        .filter((marker) => (counts.get(marker) ?? 0) > 1)
        .sort((left, right) => left - right);
      let missing = 0;
      if (markers.length > 0) {
        for (let marker = min; marker <= max; marker++) {
          if (!counts.has(marker)) missing++;
        }
      }
      // A render-order drop catches displaced rows that duplicate and loss counts cannot see.
      let outOfOrder = 0;
      let firstInversion = -1;
      for (let index = 1; index < markerSequence.length; index++) {
        if (markerSequence[index]! < markerSequence[index - 1]!) {
          outOfOrder++;
          if (firstInversion < 0) firstInversion = markerSequence[index]!;
        }
      }
      return {
        total,
        unique: markers.length,
        min,
        max,
        duplicated,
        missing,
        outOfOrder,
        firstInversion,
      };
    },
    terminalDimensions(sessionId) {
      const slot = document.querySelector(
        `[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`,
      );
      const terminal = slot?.querySelector(".cell-grid") as HTMLElement | null;
      const viewport = terminal?.querySelector(".cell-viewport");
      const cols = Number.parseInt(terminal?.style.getPropertyValue("--cell-cols") ?? "", 10);
      const rows = viewport
        ? Array.from(viewport.children).filter((child) => child.classList.contains("cell-row")).length
        : 0;
      return { cols: Number.isSafeInteger(cols) ? cols : 0, rows };
    },
  };
}
