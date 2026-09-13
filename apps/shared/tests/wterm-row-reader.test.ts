// Packed WTerm row reader differential tests. Factory-created cores use the raw
// 12-byte ABI path while a prototype-only view of the same core forces public
// bridge reads. Every comparison is complete span/frame structure, so scratch
// reuse, links, scrollback width, resize, and alternate grids cannot drift.

import { describe, expect, test } from "bun:test";
import type { CellData, TerminalCore } from "@wterm/core";
import {
  MAX_LINK_URI_BYTES,
  gridToCellFrame,
  readScrollbackRangeCells,
  registerWtermRowReader,
  rowToSpans,
  scrollbackOffsetSpans,
  viewportRowSpans,
  wtermRowReader,
} from "../src/cell/index.ts";
import { createWtermCore, resizeWtermCore } from "../src/wterm-core-factory.ts";
import { TERMINAL_MAX_COLS } from "../src/viewport.ts";

const STREAM_ID = "00000000-0000-4000-8000-000000000001";
const CELL_SIZE = 12;
const GRID_POINTER = 64;

type PackedFixtureCell = {
  char: number;
  fg?: number;
  bg?: number;
  flags?: number;
  width?: number;
  linkIndex?: number;
};

type FixtureLink = {
  uri: string;
  key: string;
};

function publicReaderView(core: TerminalCore): TerminalCore {
  // The registry is keyed by identity. This inherits the same bridge methods
  // while deliberately selecting the public reader for a same-core differential.
  return Object.create(core) as TerminalCore;
}

function expectPackedParity(core: TerminalCore): void {
  const publicCore = publicReaderView(core);
  expect(wtermRowReader(core)).not.toBeNull();
  expect(wtermRowReader(publicCore)).toBeNull();

  const cols = core.getCols();
  for (let row = 0; row < core.getRows(); row++) {
    expect(viewportRowSpans(core, row, cols)).toEqual(viewportRowSpans(publicCore, row, cols));
  }
  for (let offset = 0; offset < core.getScrollbackCount(); offset++) {
    expect(scrollbackOffsetSpans(core, offset)).toEqual(scrollbackOffsetSpans(publicCore, offset));
  }
  expect(gridToCellFrame(core, 7, "reader-grid", STREAM_ID)).toEqual(
    gridToCellFrame(publicCore, 7, "reader-grid", STREAM_ID),
  );
  expect(readScrollbackRangeCells(core, 0, core.getScrollbackCount())).toEqual(
    readScrollbackRangeCells(publicCore, 0, publicCore.getScrollbackCount()),
  );
}

function writePackedFixtureCell(memory: WebAssembly.Memory, pointer: number, cell: PackedFixtureCell): void {
  const view = new DataView(memory.buffer);
  view.setUint32(pointer, cell.char, true);
  view.setUint16(pointer + 4, cell.fg ?? 256, true);
  view.setUint16(pointer + 6, cell.bg ?? 256, true);
  view.setUint8(pointer + 8, cell.flags ?? 0);
  view.setUint8(pointer + 9, cell.width ?? 1);
  view.setUint16(pointer + 10, cell.linkIndex ?? 0, true);
}

function fixturePublicCell(
  memory: WebAssembly.Memory,
  pointer: number,
  links: ReadonlyMap<number, FixtureLink>,
): CellData {
  const view = new DataView(memory.buffer);
  const link = links.get(view.getUint16(pointer + 10, true));
  return {
    char: view.getUint32(pointer, true),
    fg: view.getUint16(pointer + 4, true),
    bg: view.getUint16(pointer + 6, true),
    flags: view.getUint8(pointer + 8),
    width: view.getUint8(pointer + 9),
    linkUri: link?.uri,
    linkKey: link?.key,
  };
}

function packedFixtureCore(
  memory: WebAssembly.Memory,
  links: ReadonlyMap<number, FixtureLink>,
): TerminalCore {
  const publicCell = (column: number): CellData => fixturePublicCell(
    memory,
    GRID_POINTER + column * CELL_SIZE,
    links,
  );
  return {
    getCell: (_row: number, column: number) => publicCell(column),
    getScrollbackCell: (_offset: number, column: number) => publicCell(column),
  } as unknown as TerminalCore;
}

function packedFixtureInstance(memory: WebAssembly.Memory, cellSize = CELL_SIZE): WebAssembly.Instance {
  return {
    exports: {
      memory,
      getCellSize: () => cellSize,
      getMaxCols: () => TERMINAL_MAX_COLS,
      getGridPtr: () => GRID_POINTER,
      getScrollbackLine: () => GRID_POINTER,
      getScrollbackLineLen: () => 0,
    },
  } as unknown as WebAssembly.Instance;
}

describe("factory packed WTerm row reader", () => {
  test("matches public cells for plain, styled, blank, wide, astral, cluster, and link rows", async () => {
    const core = await createWtermCore(80, 8);
    const sameUri = "https://example.test/same";

    core.writeString("\x1b[1;1Hplain   ");
    core.writeString("\x1b[2;1H\x1b[31;44mstyled \x1b[0m");
    core.writeString("\x1b[3;1Hwide: 中界");
    core.writeString("\x1b[4;1Hastral: 🐙 cluster: 👋🏽");
    core.writeString(
      `\x1b[5;1H\x1b]8;;${sameUri}\x1b\\ab\x1b]8;;\x1b\\`
      + `\x1b]8;;${sameUri}\x1b\\cd\x1b]8;;\x1b\\`,
    );

    expectPackedParity(core);

    expect(viewportRowSpans(core, 0, core.getCols()).map((span) => span.text)).toEqual(["plain"]);
    expect(viewportRowSpans(core, 1, core.getCols()).some(
      (span) => span.text.endsWith(" ") && span.fg === 1 && span.bg === 4,
    )).toBe(true);
    const sameUriSpans = viewportRowSpans(core, 4, core.getCols()).filter(
      (span) => span.linkUri === sameUri,
    );
    expect(sameUriSpans.map((span) => span.text)).toEqual(["ab", "cd"]);
    expect(sameUriSpans[0]!.linkKey).not.toBe(sameUriSpans[1]!.linkKey);
  });

  test("matches public history after a narrower resize and in the alternate screen", async () => {
    const core = await createWtermCore(48, 3);
    core.writeString(
      [
        "HISTORY-WIDER-THAN-VIEWPORT-1234567890",
        "history-two",
        "history-three",
        "history-four",
        "history-five",
      ].join("\r\n") + "\r\n",
    );
    expect(core.getScrollbackCount()).toBeGreaterThan(0);

    resizeWtermCore(core, { cols: 12, rows: 3 });
    expect(core.getScrollbackLineLen(core.getScrollbackCount() - 1)).toBeGreaterThan(core.getCols());
    expectPackedParity(core);

    core.writeString("\x1b[?1049h\x1b[1;1HALT 中 👋🏽");
    expect(core.usingAltScreen()).toBe(true);
    expectPackedParity(core);
  });

  test("copies spans before the borrowed scratch is reused and respects a supplied row length", async () => {
    const core = await createWtermCore(20, 2);
    core.writeString("\x1b[1;1Hfirst\x1b[2;1Hsecond");
    const first = viewportRowSpans(core, 0, core.getCols());
    const firstBeforeNextRead = first.map((span) => ({ ...span }));
    viewportRowSpans(core, 1, core.getCols());
    expect(first).toEqual(firstBeforeNextRead);

    const wideLead: CellData = { char: "中".codePointAt(0)!, fg: 256, bg: 256, flags: 0, width: 2 };
    const continuation: CellData = { char: 0, fg: 256, bg: 256, flags: 0, width: 0 };
    expect(rowToSpans([wideLead, continuation], 1).map((span) => [span.text, span.columns]))
      .toEqual([["中", 1]]);
  });

  test("refreshes its memory view, clears stale link fields, and falls back for an unsupported ABI", () => {
    const memory = new WebAssembly.Memory({ initial: 1, maximum: 2 });
    const overCapUri = `https://example.test/${"x".repeat(MAX_LINK_URI_BYTES)}`;
    const links = new Map<number, FixtureLink>([[1, { uri: overCapUri, key: "link-1" }]]);
    const core = packedFixtureCore(memory, links);
    registerWtermRowReader(core, packedFixtureInstance(memory));
    expect(wtermRowReader(core)).not.toBeNull();

    writePackedFixtureCell(memory, GRID_POINTER, {
      char: "x".codePointAt(0)!, fg: 1, bg: 4, flags: 1, linkIndex: 1,
    });
    const publicCore = publicReaderView(core);
    expect(viewportRowSpans(core, 0, 1)).toEqual(viewportRowSpans(publicCore, 0, 1));
    expect(viewportRowSpans(core, 0, 1)[0]!.linkUri).toBeUndefined();

    memory.grow(1);
    writePackedFixtureCell(memory, GRID_POINTER, {
      char: "y".codePointAt(0)!, fg: 2, bg: 3, flags: 2,
    });
    expect(viewportRowSpans(core, 0, 1)).toEqual(viewportRowSpans(publicCore, 0, 1));
    expect(viewportRowSpans(core, 0, 1)[0]).toMatchObject({
      text: "y", fg: 2, bg: 3, flags: 2, linkUri: undefined, linkKey: undefined,
    });

    registerWtermRowReader(core, packedFixtureInstance(memory, CELL_SIZE - 1));
    expect(wtermRowReader(core)).toBeNull();
    expect(viewportRowSpans(core, 0, 1)).toEqual(viewportRowSpans(publicCore, 0, 1));
  });
});
