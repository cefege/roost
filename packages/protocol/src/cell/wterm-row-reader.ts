// Packed WTerm row reader — the worker-only factory registers this optional
// ABI bridge against its own TerminalCore; grid-to-cells consumes it synchronously.
// It fills one borrowed 256-cell scratch row and retains public bridge reads for
// links and every core whose verified packed layout is unavailable.

import type { CellData, TerminalCore } from "@wterm/core";
import { TERMINAL_MAX_COLS } from "../viewport.ts";

const WTERM_CELL_SIZE = 12;
const WTERM_ROW_SCRATCH_CELLS = 256;
const CELL_CHAR_OFFSET = 0;
const CELL_FG_OFFSET = 4;
const CELL_BG_OFFSET = 6;
const CELL_FLAGS_OFFSET = 8;
const CELL_WIDTH_OFFSET = 9;
const CELL_LINK_OFFSET = 10;

type WasmNumberGetter = (...args: number[]) => number;

type MutableBorrowedRow = {
  cells: readonly CellData[];
  length: number;
};

/** A row backed by reusable reader storage. Consume it synchronously before
 * reading another row from the same WtermRowReader. */
export interface WtermBorrowedRow {
  readonly cells: readonly CellData[];
  readonly length: number;
}

/** Optional packed-row reader for a factory-created WTerm core. Its returned
 * row is borrowed; rowToSpans copies every field needed by canonical cells. */
export interface WtermRowReader {
  viewportRow(row: number, length: number): WtermBorrowedRow | null;
  scrollbackRow(offset: number): WtermBorrowedRow | null;
}

// Only createWtermCore installs readers. Cores from the browser, test doubles,
// and alternative implementations intentionally stay on the public reader.
const _rowReaders = new WeakMap<TerminalCore, WtermRowReader>();

function isWasmNumberGetter(value: WebAssembly.ExportValue | undefined): value is WasmNumberGetter {
  return typeof value === "function";
}

function nonNegativeSafeInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function clearOptionalCellFields(cell: CellData): void {
  cell.chars = undefined;
  cell.fgRgb = undefined;
  cell.bgRgb = undefined;
  cell.linkUri = undefined;
  cell.linkId = undefined;
  cell.linkKey = undefined;
}

function copyPublicCell(target: CellData, source: CellData): void {
  target.char = source.char;
  target.fg = source.fg;
  target.bg = source.bg;
  target.flags = source.flags;
  target.width = source.width;
  target.chars = source.chars;
  target.fgRgb = source.fgRgb;
  target.bgRgb = source.bgRgb;
  target.linkUri = source.linkUri;
  target.linkId = source.linkId;
  target.linkKey = source.linkKey;
}

class PackedWtermRowReader implements WtermRowReader {
  private dataView: DataView | null = null;
  private dataViewBuffer: ArrayBuffer | null = null;
  private readonly scratch: CellData[];
  private readonly borrowed: MutableBorrowedRow;

  constructor(
    private readonly core: TerminalCore,
    private readonly memory: WebAssembly.Memory,
    private readonly getGridPtr: WasmNumberGetter,
    private readonly getScrollbackLine: WasmNumberGetter,
    private readonly getScrollbackLineLen: WasmNumberGetter,
  ) {
    this.scratch = new Array<CellData>(WTERM_ROW_SCRATCH_CELLS);
    for (let index = 0; index < WTERM_ROW_SCRATCH_CELLS; index++) {
      this.scratch[index] = {
        char: 0,
        fg: 0,
        bg: 0,
        flags: 0,
        width: 1,
        chars: undefined,
        fgRgb: undefined,
        bgRgb: undefined,
        linkUri: undefined,
        linkId: undefined,
        linkKey: undefined,
      };
    }
    this.borrowed = { cells: this.scratch, length: 0 };
  }

  viewportRow(row: number, length: number): WtermBorrowedRow | null {
    const safeRow = nonNegativeSafeInteger(row);
    const safeLength = nonNegativeSafeInteger(length);
    if (safeRow === null || safeLength === null || safeLength > WTERM_ROW_SCRATCH_CELLS) return null;
    let gridPtr: number | null = null;
    try {
      gridPtr = nonNegativeSafeInteger(this.getGridPtr());
    } catch {
      return null;
    }
    if (gridPtr === null) return null;
    return this.readRow(
      gridPtr + safeRow * TERMINAL_MAX_COLS * WTERM_CELL_SIZE,
      safeLength,
      safeRow,
      false,
    );
  }

  scrollbackRow(offset: number): WtermBorrowedRow | null {
    const safeOffset = nonNegativeSafeInteger(offset);
    if (safeOffset === null) return null;
    let length: number | null = null;
    let linePtr: number | null = null;
    try {
      length = nonNegativeSafeInteger(this.getScrollbackLineLen(safeOffset));
      linePtr = nonNegativeSafeInteger(this.getScrollbackLine(safeOffset));
    } catch {
      return null;
    }
    if (
      length === null
      || linePtr === null
      || length > WTERM_ROW_SCRATCH_CELLS
    ) return null;
    return this.readRow(linePtr, length, safeOffset, true);
  }

  private currentDataView(): DataView {
    const buffer = this.memory.buffer;
    if (this.dataView === null || this.dataViewBuffer !== buffer) {
      this.dataView = new DataView(buffer);
      this.dataViewBuffer = buffer;
    }
    return this.dataView;
  }

  private readRow(
    pointer: number,
    length: number,
    sourceRow: number,
    scrollback: boolean,
  ): WtermBorrowedRow | null {
    const view = this.currentDataView();
    const bytes = length * WTERM_CELL_SIZE;
    if (pointer > view.byteLength || bytes > view.byteLength - pointer) return null;

    for (let column = 0; column < length; column++) {
      const offset = pointer + column * WTERM_CELL_SIZE;
      const cell = this.scratch[column]!;
      if (view.getUint16(offset + CELL_LINK_OFFSET, true) === 0) {
        cell.char = view.getUint32(offset + CELL_CHAR_OFFSET, true);
        cell.fg = view.getUint16(offset + CELL_FG_OFFSET, true);
        cell.bg = view.getUint16(offset + CELL_BG_OFFSET, true);
        cell.flags = view.getUint8(offset + CELL_FLAGS_OFFSET);
        cell.width = view.getUint8(offset + CELL_WIDTH_OFFSET);
        clearOptionalCellFields(cell);
      } else if (scrollback) {
        copyPublicCell(cell, this.core.getScrollbackCell(sourceRow, column));
      } else {
        copyPublicCell(cell, this.core.getCell(sourceRow, column));
      }
    }
    this.borrowed.length = length;
    return this.borrowed;
  }
}

function packedRowReader(core: TerminalCore, instance: WebAssembly.Instance): WtermRowReader | null {
  const exports = instance.exports;
  const memory = exports.memory;
  const getCellSize = exports.getCellSize;
  const getMaxCols = exports.getMaxCols;
  const getGridPtr = exports.getGridPtr;
  const getScrollbackLine = exports.getScrollbackLine;
  const getScrollbackLineLen = exports.getScrollbackLineLen;
  if (
    !(memory instanceof WebAssembly.Memory)
    || !isWasmNumberGetter(getCellSize)
    || !isWasmNumberGetter(getMaxCols)
    || !isWasmNumberGetter(getGridPtr)
    || !isWasmNumberGetter(getScrollbackLine)
    || !isWasmNumberGetter(getScrollbackLineLen)
    || typeof core.getCell !== "function"
    || typeof core.getScrollbackCell !== "function"
  ) return null;

  try {
    if (
      getCellSize() !== WTERM_CELL_SIZE
      || getMaxCols() !== TERMINAL_MAX_COLS
      || TERMINAL_MAX_COLS !== WTERM_ROW_SCRATCH_CELLS
    ) return null;
  } catch {
    return null;
  }

  return new PackedWtermRowReader(
    core,
    memory,
    getGridPtr,
    getScrollbackLine,
    getScrollbackLineLen,
  );
}

/** Register the packed reader only when the installed bridge exposes the exact
 * verified cell ABI. Unsupported layouts deliberately retain the public path. */
export function registerWtermRowReader(core: TerminalCore, instance: WebAssembly.Instance): void {
  _rowReaders.delete(core);
  const reader = packedRowReader(core, instance);
  if (reader !== null) _rowReaders.set(core, reader);
}

/** The factory-installed packed reader for this core, or null for public-only
 * cores and every unsupported bridge ABI. */
export function wtermRowReader(core: TerminalCore): WtermRowReader | null {
  return _rowReaders.get(core) ?? null;
}
