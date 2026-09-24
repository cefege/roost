// Renderer qualification fixtures — deterministic cell-shipping input.
// browser.js imports this through Bun's fixture bundle; it never touches a PTY.
// The provider models delayed, bounded worker history reads by absolute row.
// Cell expansion reuses the shared wire geometry rather than reimplementing Unicode widths.

import {
  CELL_BOLD,
  CELL_ITALIC,
  CELL_UNDERLINE,
  DEFAULT_COLOR,
  rowColumns,
  spanIsAtomic,
  type CellGridFrame,
  type CellRow,
  type CellSpan,
} from "@roost/protocol/cell";

export const QUALIFICATION_COLS = 80;
export const QUALIFICATION_ROWS = 24;
export const QUALIFICATION_HISTORY_END = 10_000;
export const QUALIFICATION_DELAY_MS = 250;
export const QUALIFICATION_STREAM_ID = "renderer-qualification-stream-1";
export const QUALIFICATION_EPOCH = "renderer-qualification-epoch-1";

export interface QualificationCell {
  char: number;
  chars?: string;
  width?: 0 | 1 | 2;
  fg: number;
  bg: number;
  flags: number;
  fgRgb?: number;
  bgRgb?: number;
  linkUri?: string;
  linkKey?: string;
}

export interface QualificationFixture {
  frame: CellGridFrame;
  history: readonly CellRow[];
  historyFloor: number;
  historyEnd: number;
}

export interface LoadedHistoryRange {
  epoch: string;
  start: number;
  end: number;
  rows: readonly CellRow[];
}

function span(text: string, columns = text.length, extra: Partial<CellSpan> = {}): CellSpan {
  return { text, columns, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, ...extra };
}

function markerRow(index: number, marker: string): CellRow {
  return { index, spans: [span(marker)] };
}

function fixtureViewportRow(row: number): CellRow {
  const marker = `V${String(row).padStart(2, "0")}`;
  if (row === 4) return { index: row, spans: [span(`${marker} `), span("界", 2)] };
  if (row === 5) return { index: row, spans: [span(`${marker} `), span(String.fromCodePoint(0x1f600), 2)] };
  if (row === 6) return { index: row, spans: [span(`${marker} `), span("é", 1)] };
  if (row === 7) {
    return {
      index: row,
      spans: [
        span(`${marker} RGB`, undefined, { fgRgb: 0x12abef, bgRgb: 0x301020, flags: CELL_BOLD | CELL_ITALIC | CELL_UNDERLINE }),
      ],
    };
  }
  if (row === 8) {
    return {
      index: row,
      spans: [
        span(`${marker} A`, undefined, { linkUri: "https://example.com/", linkKey: "qualification-link-a" }),
        span("B", 1, { linkUri: "https://example.com/", linkKey: "qualification-link-b" }),
      ],
    };
  }
  return markerRow(row, marker);
}

export function createQualificationFixture(epoch = QUALIFICATION_EPOCH, cols = QUALIFICATION_COLS): QualificationFixture {
  const historyMarkerPrefix = epoch === QUALIFICATION_EPOCH ? "H" : "E2H";
  const history = Array.from({ length: QUALIFICATION_HISTORY_END }, (_, index) => markerRow(index, `${historyMarkerPrefix}${String(index).padStart(5, "0")}`));
  const viewportRows = Array.from({ length: QUALIFICATION_ROWS }, (_, row) => fixtureViewportRow(row));
  return {
    frame: {
      streamId: QUALIFICATION_STREAM_ID,
      gridEpoch: epoch,
      cols,
      rows: QUALIFICATION_ROWS,
      cursorRow: 23,
      cursorCol: 4,
      cursorVisible: true,
      altScreen: false,
      cursorKeysApp: false,
      bracketedPaste: false,
      mouseTracking: 0,
      mouseSgr: false,
      focusEvents: false,
      full: true,
      viewportRows,
      scrollbackRows: [],
      scrollbackAppend: [],
      // The cell emitter's actual contract is absolute history END, not retained count.
      scrollbackTotal: QUALIFICATION_HISTORY_END,
      sbBase: QUALIFICATION_HISTORY_END,
      baseSeq: 0,
      seq: 1,
    },
    history,
    historyFloor: 0,
    historyEnd: QUALIFICATION_HISTORY_END,
  };
}

export class DelayedQualificationHistory {
  #fixture: QualificationFixture;
  #floor: number;

  constructor(fixture = createQualificationFixture()) {
    this.#fixture = fixture;
    this.#floor = fixture.historyFloor;
  }

  get epoch(): string { return this.#fixture.frame.gridEpoch; }
  get floor(): number { return this.#floor; }
  get end(): number { return this.#fixture.historyEnd; }
  get frame(): CellGridFrame { return this.#fixture.frame; }

  replace(epoch: string, cols = this.#fixture.frame.cols): void {
    this.#fixture = createQualificationFixture(epoch, cols);
    this.#floor = 0;
  }

  evictThrough(floor: number): void {
    if (!Number.isInteger(floor) || floor < this.#floor || floor > this.end) throw new Error(`InvalidHistoryFloor:${floor}`);
    this.#floor = floor;
  }

  async load(start: number, end: number, expectedEpoch = this.epoch): Promise<readonly LoadedHistoryRange[]> {
    const clampedStart = Math.max(this.floor, Math.min(this.end, start));
    const clampedEnd = Math.max(clampedStart, Math.min(this.end, end));
    const ranges: LoadedHistoryRange[] = [];
    for (let cursor = clampedStart; cursor < clampedEnd; cursor += 250) {
      const rangeEnd = Math.min(clampedEnd, cursor + 250);
      await new Promise<void>((resolve) => globalThis.setTimeout(resolve, QUALIFICATION_DELAY_MS));
      if (expectedEpoch !== this.epoch) continue;
      ranges.push({ epoch: expectedEpoch, start: cursor, end: rangeEnd, rows: this.#fixture.history.slice(cursor, rangeEnd) });
    }
    return ranges;
  }
}

export function expandRow(row: CellRow): readonly QualificationCell[] {
  const cells: QualificationCell[] = [];
  for (const cellSpan of row.spans) {
    const common = {
      fg: cellSpan.fg,
      bg: cellSpan.bg,
      flags: cellSpan.flags,
      fgRgb: cellSpan.fgRgb,
      bgRgb: cellSpan.bgRgb,
      linkUri: cellSpan.linkUri,
      linkKey: cellSpan.linkKey,
    };
    if (spanIsAtomic(cellSpan)) {
      const char = cellSpan.text.codePointAt(0) ?? 32;
      cells.push({ ...common, char, chars: cellSpan.text, width: cellSpan.columns as 1 | 2 });
      for (let continuation = 1; continuation < cellSpan.columns; continuation++) cells.push({ ...common, char: 0, width: 0 });
      continue;
    }
    for (const character of cellSpan.text) cells.push({ ...common, char: character.codePointAt(0) ?? 32, width: 1 });
  }
  if (cells.length !== rowColumns(row.spans)) throw new Error(`ExpandedCellWidth:${row.index}`);
  return cells;
}
