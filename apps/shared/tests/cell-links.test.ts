// OSC 8 hyperlink identity on the cell wire. Links are CORE-AUTHORED: the
// emulator resolves each cell's link index and rowToSpans carries that identity
// onto the span. Nothing derives links from the byte stream and nothing matches
// link TEXT, so these are the only guarantees the browser can rely on:
//
//   * a contiguous run of ONE link is ONE span;
//   * a link boundary breaks a run even when the style is byte-identical;
//   * a URI over MAX_LINK_URI_BYTES drops the LINK, never the TEXT;
//   * both fields survive the proto round-trip on viewport AND scrollback rows;
//   * decode rejects half a link (key with no URI, empty URI, over-cap URI).

import { describe, test, expect } from "bun:test";
import type { TerminalCore, CellData, CursorState } from "@wterm/core";
import {
  rowToSpans, gridToCellFrame, assertCellRowSpans,
  DEFAULT_COLOR, CELL_BOLD, MAX_LINK_URI_BYTES, linkUriWithinCap,
  type CellRow,
} from "../src/cell/index.ts";
import { cellFrameToProto, protoToCellFrame, deepCloneCellFrameFromProto } from "../src/cell/cell-proto.ts";

const URI = "https://example.test/x";

/** One narrow cell. `link` is the [uri, key] pair the core would resolve. */
function cell(ch: string, link?: readonly [string, string], flags = 0): CellData {
  return {
    char: ch.codePointAt(0)!, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags,
    fgRgb: undefined, bgRgb: undefined,
    linkUri: link?.[0], linkKey: link?.[1],
  };
}

/** Spread `text` over cells, all sharing one link (or none). */
function linkedCells(text: string, link?: readonly [string, string]): CellData[] {
  return Array.from(text, (ch) => cell(ch, link));
}

const shapeOf = (row: readonly { text: string; linkUri?: string; linkKey?: string }[]) =>
  row.map((s) => [s.text, s.linkUri, s.linkKey]);

// Mock core just deep enough for gridToCellFrame: one viewport row plus one
// retained scrollback line, so both readers are exercised by the same frame.
class LinkCore {
  constructor(readonly grid: CellData[][], readonly sb: CellData[][]) {}
  cursor: CursorState = { row: 0, col: 0, visible: true };
  getCols() { return this.grid[0]?.length ?? 0; }
  getRows() { return this.grid.length; }
  getCell(r: number, c: number) { return this.grid[r]![c]!; }
  getCursor() { return this.cursor; }
  usingAltScreen() { return false; }
  cursorKeysApp() { return false; }
  bracketedPaste() { return false; }
  getScrollbackCount() { return this.sb.length; }
  getScrollbackDiscardedCount() { return 0; }
  /** Core order is NEWEST-FIRST; `sb` is held oldest-first. */
  getScrollbackCell(off: number, c: number) { return this.sb[this.sb.length - 1 - off]![c]!; }
  getScrollbackLineLen(off: number) { return this.sb[this.sb.length - 1 - off]!.length; }
  isDirtyRow() { return false; }
  clearDirty() { /* no dirty tracking needed */ }
}
const asCore = (m: LinkCore) => m as unknown as TerminalCore;

describe("rowToSpans link runs", () => {
  test("a contiguous run of one link is ONE span carrying uri + key", () => {
    const spans = rowToSpans(linkedCells("click", [URI, "b\u00000"]));
    expect(shapeOf(spans)).toEqual([["click", URI, "b\u00000"]]);
    expect(spans[0]!.columns).toBe(5);
  });

  test("two adjacent same-style cells with different linkKey produce TWO spans", () => {
    // Same URI, same style, two separate OSC 8 emissions: the core hands out a
    // distinct key per emission and each must stay independently clickable.
    const spans = rowToSpans([
      ...linkedCells("ab", [URI, "b\u00000"]),
      ...linkedCells("cd", [URI, "b\u00001"]),
    ]);
    expect(shapeOf(spans)).toEqual([
      ["ab", URI, "b\u00000"],
      ["cd", URI, "b\u00001"],
    ]);
  });

  test("a link boundary against unlinked text breaks the run both ways", () => {
    const spans = rowToSpans([
      ...linkedCells("go", undefined),
      ...linkedCells("here", [URI, "e\u0000id\u0000" + URI]),
      ...linkedCells("!", undefined),
    ]);
    expect(shapeOf(spans)).toEqual([
      ["go", undefined, undefined],
      ["here", URI, "e\u0000id\u0000" + URI],
      ["!", undefined, undefined],
    ]);
  });

  test("one link split by a STYLE change is two spans, both keeping the link", () => {
    const key = "b\u00007";
    const spans = rowToSpans([
      cell("a", [URI, key]),
      cell("b", [URI, key], CELL_BOLD),
    ]);
    expect(shapeOf(spans)).toEqual([["a", URI, key], ["b", URI, key]]);
  });

  test("an atomic wide glyph carries its link and folds its continuation column", () => {
    const key = "b\u00002";
    const lead: CellData = { ...cell("中", [URI, key]), width: 2 };
    const cont: CellData = { ...cell(" ", [URI, key]), width: 0, char: 0 };
    const spans = rowToSpans([lead, cont]);
    expect(shapeOf(spans)).toEqual([["中", URI, key]]);
    expect(spans[0]!.columns).toBe(2);
  });

  test("a linked trailing blank is not trimmed away — its columns are clickable", () => {
    const key = "b\u00003";
    const spans = rowToSpans([
      cell("x"),
      ...linkedCells("  ", [URI, key]),
      cell(" "),
    ]);
    expect(shapeOf(spans)).toEqual([["x", undefined, undefined], ["  ", URI, key]]);
  });
});

describe("link URI cap", () => {
  test("linkUriWithinCap counts UTF-8 bytes, not code units", () => {
    expect(linkUriWithinCap("a".repeat(MAX_LINK_URI_BYTES))).toBe(true);
    expect(linkUriWithinCap("a".repeat(MAX_LINK_URI_BYTES + 1))).toBe(false);
    // 3 bytes each: 683 chars = 2049 bytes > cap, 682 = 2046 <= cap.
    expect(linkUriWithinCap("\u4e2d".repeat(682))).toBe(true);
    expect(linkUriWithinCap("\u4e2d".repeat(683))).toBe(false);
    // Surrogate pair: 2 code units, 4 bytes.
    expect(linkUriWithinCap("\u{1f419}".repeat(MAX_LINK_URI_BYTES / 4))).toBe(true);
    expect(linkUriWithinCap("\u{1f419}".repeat(MAX_LINK_URI_BYTES / 4 + 1))).toBe(false);
  });

  test("an over-cap URI drops the LINK and keeps the TEXT", () => {
    const huge = `https://example.test/${"q".repeat(MAX_LINK_URI_BYTES)}`;
    const spans = rowToSpans(linkedCells("text", [huge, "b\u00004"]));
    expect(shapeOf(spans)).toEqual([["text", undefined, undefined]]);
  });

  test("dropping an over-cap link does not merge it with a legitimate neighbour link", () => {
    const huge = `https://example.test/${"q".repeat(MAX_LINK_URI_BYTES)}`;
    const spans = rowToSpans([
      ...linkedCells("ab", [huge, "b\u00005"]),
      ...linkedCells("cd", [URI, "b\u00006"]),
    ]);
    expect(shapeOf(spans)).toEqual([
      ["ab", undefined, undefined],
      ["cd", URI, "b\u00006"],
    ]);
  });

  test("a URI exactly at the cap survives", () => {
    const exact = "h".repeat(MAX_LINK_URI_BYTES);
    const spans = rowToSpans(linkedCells("t", [exact, "b\u00008"]));
    expect(spans[0]!.linkUri).toBe(exact);
  });
});

describe("proto round-trip", () => {
  const key = "e\u0000a1\u0000" + URI;
  const core = new LinkCore(
    [[...linkedCells("live", [URI, key]), cell(" "), ...linkedCells("no", undefined)]],
    [linkedCells("history", [URI, key])],
  );

  // tailRows = 1 so the frame carries the retained line as well.
  const frame = gridToCellFrame(asCore(core), 1, "grid", 1);

  test("link fields survive on viewport AND scrollback rows", () => {
    const back = protoToCellFrame(cellFrameToProto(frame, "sid"));
    expect(shapeOf(back.viewportRows[0]!.spans)).toEqual([
      ["live", URI, key],
      [" no", undefined, undefined],
    ]);
    expect(shapeOf(back.scrollbackRows[0]!.spans)).toEqual([["history", URI, key]]);
  });

  test("the deep-clone adapter copies link fields too", () => {
    const clone = deepCloneCellFrameFromProto(cellFrameToProto(frame, "sid"));
    expect(shapeOf(clone.viewportRows[0]!.spans)).toEqual([
      ["live", URI, key],
      [" no", undefined, undefined],
    ]);
    expect(shapeOf(clone.scrollbackRows[0]!.spans)).toEqual([["history", URI, key]]);
  });
});

describe("decode validation", () => {
  const rowWith = (over: { linkUri?: string; linkKey?: string }): CellRow => ({
    index: 3,
    spans: [{
      text: "ab", columns: 2, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0,
      fgRgb: undefined, bgRgb: undefined, ...over,
    }],
  });

  test("accepts a complete link and a link-free span", () => {
    expect(() => assertCellRowSpans(rowWith({ linkUri: URI, linkKey: "k" }), 80)).not.toThrow();
    expect(() => assertCellRowSpans(rowWith({}), 80)).not.toThrow();
  });

  test("rejects link_key without link_uri", () => {
    expect(() => assertCellRowSpans(rowWith({ linkKey: "k" }), 80))
      .toThrow(/link_key with no link_uri/);
  });

  test("rejects link_uri without link_key", () => {
    expect(() => assertCellRowSpans(rowWith({ linkUri: URI }), 80))
      .toThrow(/link_uri with no link_key/);
  });

  test("rejects an empty link_uri", () => {
    expect(() => assertCellRowSpans(rowWith({ linkUri: "", linkKey: "k" }), 80))
      .toThrow(/empty link_uri/);
  });

  test("rejects a link_uri over the wire cap", () => {
    const huge = "h".repeat(MAX_LINK_URI_BYTES + 1);
    expect(() => assertCellRowSpans(rowWith({ linkUri: huge, linkKey: "k" }), 80))
      .toThrow(new RegExp(`over ${MAX_LINK_URI_BYTES} bytes`));
  });
});
