// Differential terminal-core harness for fast-row and cursor-margin behavior.
// Owns comparable wterm/xterm snapshots; callers own byte-stream corpora.
// Depends on the digest-verified Roost WASM and @xterm/headless.

import { Terminal } from "@xterm/headless";
import type { TerminalCore } from "@wterm/core";
import { readScrollbackRangeCells } from "../../protocol/src/cell/grid-to-cells.ts";
import { createWtermCore } from "../src/wterm-core-factory.ts";

export interface CoreView {
  scrollback: string[];
  viewport: string[];
  cursor: { row: number; col: number };
}

export interface CoreDiff {
  wterm: CoreView;
  xterm: CoreView;
  firstDivergentChunk: number | null;
}

function wtermRowText(cells: readonly { readonly text: string }[]): string {
  let text = "";
  for (const cell of cells) text += cell.text;
  return text.trimEnd();
}

function wtermView(core: TerminalCore): CoreView {
  const dropped = core.getScrollbackDiscardedCount?.() ?? 0;
  const scrollback = readScrollbackRangeCells(
    core,
    dropped,
    dropped + core.getScrollbackCount(),
    dropped,
  ).map((row) => wtermRowText(row.spans));
  const viewport: string[] = [];
  for (let row = 0; row < core.getRows(); row++) {
    let text = "";
    for (let col = 0; col < core.getCols(); col++) {
      const cell = core.getCell(row, col);
      if (cell.width === 0) continue;
      text += cell.char === 0 ? " " : String.fromCodePoint(cell.char);
    }
    viewport.push(text.trimEnd());
  }
  const cursor = core.getCursor();
  return { scrollback, viewport, cursor: { row: cursor.row, col: cursor.col } };
}

function xtermView(terminal: Terminal): CoreView {
  const buffer = terminal.buffer.active;
  const scrollback: string[] = [];
  for (let row = 0; row < buffer.baseY; row++) {
    const line = buffer.getLine(row);
    if (!line) throw new Error(`xterm.js has no buffer line ${row}`);
    scrollback.push(line.translateToString(true).trimEnd());
  }
  const viewport: string[] = [];
  for (let row = buffer.baseY; row < buffer.baseY + terminal.rows; row++) {
    const line = buffer.getLine(row);
    if (!line) throw new Error(`xterm.js has no viewport line ${row}`);
    viewport.push(line.translateToString(true).trimEnd());
  }
  return {
    scrollback,
    viewport,
    cursor: { row: buffer.cursorY, col: buffer.cursorX },
  };
}

function sameRows(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((row, index) => row === right[index]);
}

function divergent(wterm: CoreView, xterm: CoreView): boolean {
  return wterm.scrollback.length !== xterm.scrollback.length
    || !sameRows(wterm.scrollback, xterm.scrollback)
    || !sameRows(wterm.viewport, xterm.viewport);
}

export async function diffAgainstXterm(
  chunks: readonly Uint8Array[],
  cols: number,
  rows: number,
): Promise<CoreDiff> {
  const core = await createWtermCore(cols, rows);
  const terminal = new Terminal({ cols, rows, scrollback: 10_000, allowProposedApi: true });
  let firstDivergentChunk: number | null = null;
  try {
    for (let index = 0; index < chunks.length; index++) {
      const chunk = chunks[index]!;
      core.writeRaw(chunk);
      await new Promise<void>((resolve) => terminal.write(chunk, resolve));
      if (firstDivergentChunk === null && divergent(wtermView(core), xtermView(terminal))) {
        firstDivergentChunk = index;
      }
    }
    return { wterm: wtermView(core), xterm: xtermView(terminal), firstDivergentChunk };
  } finally {
    terminal.dispose();
  }
}
