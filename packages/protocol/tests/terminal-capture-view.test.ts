// Canonical terminal-state comparison contract. The whole attribution chain
// rests on ONE question — "are these two layers holding the same screen?" — so
// a false equal silently exonerates the layer that broke, and a false different
// blames a layer that merely re-encoded the same cells.
//
// Two axes are pinned here: what MUST be visible to the comparison (text,
// styles, wide-column occupancy, links, cursor, modes, scrollback bounds) and
// what MUST NOT be (full-vs-delta framing, baseSeq, history pages and appends,
// and any legal re-split of the same painted cells into different spans).

import { describe, expect, test } from "bun:test";
import {
  canonicalViewOfFrame,
  compareCanonicalViews,
  paintedRowFingerprint,
  paintedTextFingerprint,
} from "../src/terminal-capture.ts";
import {
  CELL_BOLD,
  DEFAULT_COLOR,
  type CellGridFrame,
  type CellRow,
  type CellSpan,
} from "../src/cell/types.ts";

function span(text: string, over: Partial<CellSpan> = {}): CellSpan {
  return {
    text,
    fg: DEFAULT_COLOR,
    bg: DEFAULT_COLOR,
    flags: 0,
    columns: text.length,
    ...over,
  };
}

function row(index: number, spans: CellSpan[]): CellRow {
  return { index, spans };
}

function frame(rows: CellRow[], over: Partial<CellGridFrame> = {}): CellGridFrame {
  return {
    streamId: "11111111-1111-4111-8111-111111111111",
    gridEpoch: "epoch:1",
    cols: 10,
    rows: rows.length,
    cursorRow: 0,
    cursorCol: 0,
    cursorVisible: true,
    altScreen: false,
    cursorKeysApp: false,
    bracketedPaste: false,
    mouseTracking: 0,
    mouseSgr: false,
    focusEvents: false,
    full: true,
    viewportRows: rows,
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0,
    sbBase: 0,
    baseSeq: 0,
    seq: 1,
    ...over,
  };
}

function viewOf(value: CellGridFrame) {
  const view = canonicalViewOfFrame(value);
  expect(view).not.toBeNull();
  return view!;
}

describe("canonicalViewOfFrame", () => {
  test("refuses a sparse delta so a patch is never compared to a screen", () => {
    const delta = frame([row(1, [span("x")])], {
      full: false,
      rows: 3,
      baseSeq: 1,
      seq: 2,
    });
    expect(canonicalViewOfFrame(delta)).toBeNull();
  });

  test("refuses a full whose viewport rows are not densely index-ordered", () => {
    const misnumbered = frame([row(0, [span("a")]), row(2, [span("b")])]);
    expect(canonicalViewOfFrame(misnumbered)).toBeNull();
  });
});

describe("compareCanonicalViews — what it must ignore", () => {
  test("delivery framing (full/baseSeq) and history payloads do not differ", () => {
    const left = frame([row(0, [span("FOOTER 12s")])], {
      scrollbackRows: [row(4, [span("old")])],
      baseSeq: 0,
    });
    const right = frame([row(0, [span("FOOTER 12s")])], {
      scrollbackRows: [],
      scrollbackAppend: [row(9, [span("newer")])],
      baseSeq: 7,
    });
    expect(compareCanonicalViews(viewOf(left), viewOf(right))).toBeNull();
  });

  test("a different but legal span split of the same cells is equal", () => {
    const coalesced = frame([row(0, [span("abcd")])]);
    const resplit = frame([row(0, [span("ab"), span("cd")])]);
    expect(compareCanonicalViews(viewOf(coalesced), viewOf(resplit))).toBeNull();
  });
});

describe("compareCanonicalViews — what it must catch", () => {
  test("one differing character names its row and column", () => {
    const left = frame([row(0, [span("FOOTER 12s")])]);
    const right = frame([row(0, [span("FOOTER 14s")])]);
    const difference = compareCanonicalViews(viewOf(left), viewOf(right));
    expect(difference?.kind).toBe("row");
    expect(difference).toMatchObject({ row: 0, column: 8, field: "text" });
  });

  test("a style-only divergence is as loud as a text one", () => {
    const left = frame([row(0, [span("ok")])]);
    const right = frame([row(0, [span("ok", { flags: CELL_BOLD })])]);
    expect(compareCanonicalViews(viewOf(left), viewOf(right))).toMatchObject({
      kind: "row",
      row: 0,
      column: 0,
      field: "flags",
    });
  });

  test("a wide glyph is aligned by COLUMN, not by code unit", () => {
    // Both rows occupy three columns, but the wide lead sits at a different
    // one. Comparing by code unit would call these equal-length and miss it.
    const leading = frame([row(0, [span("中", { columns: 2 }), span("x")])]);
    const trailing = frame([row(0, [span("x"), span("中", { columns: 2 })])]);
    expect(compareCanonicalViews(viewOf(leading), viewOf(trailing))).toMatchObject({
      kind: "row",
      row: 0,
      column: 0,
      field: "text",
    });
  });

  test("a row claiming different total columns is reported as occupancy", () => {
    const three = frame([row(0, [span("中", { columns: 2 }), span("x")])]);
    const two = frame([row(0, [span("中x", { columns: 2 })])]);
    expect(compareCanonicalViews(viewOf(three), viewOf(two))).toMatchObject({
      kind: "row",
      row: 0,
      field: "rowColumns",
      left: "3",
      right: "2",
    });
  });

  test("a link on identical text is a divergence", () => {
    const plain = frame([row(0, [span("docs")])]);
    const linked = frame([
      row(0, [span("docs", { linkUri: "https://example.test/a", linkKey: "k1" })]),
    ]);
    expect(compareCanonicalViews(viewOf(plain), viewOf(linked))).toMatchObject({
      kind: "row",
      field: "linkUri",
    });
  });

  test("cursor, mode and scrollback-bound fields are compared by name", () => {
    const base = frame([row(0, [span("a")])]);
    const cases: Array<[Partial<CellGridFrame>, string]> = [
      [{ cursorCol: 3 }, "cursorCol"],
      [{ cursorVisible: false }, "cursorVisible"],
      [{ altScreen: true }, "altScreen"],
      [{ cursorKeysApp: true }, "cursorKeysApp"],
      [{ bracketedPaste: true }, "bracketedPaste"],
      [{ mouseTracking: 1002 }, "mouseTracking"],
      [{ mouseSgr: true }, "mouseSgr"],
      [{ focusEvents: true }, "focusEvents"],
      [{ scrollbackTotal: 5, sbBase: 5 }, "sbBase"],
    ];
    for (const [over, field] of cases) {
      const difference = compareCanonicalViews(viewOf(base), viewOf(frame([row(0, [span("a")])], over)));
      expect(difference).toMatchObject({ kind: "state", field });
    }
  });

  test("a differing row count is reported as a count, not as a row", () => {
    const shorter = frame([row(0, [span("a")])]);
    const longer = frame([row(0, [span("a")]), row(1, [span("b")])]);
    // `rows` is compared before the row list, so the geometry field wins.
    expect(compareCanonicalViews(viewOf(shorter), viewOf(longer))).toMatchObject({
      kind: "state",
      field: "rows",
    });
  });

  test("no field of the difference carries the row's characters", () => {
    const left = frame([row(0, [span("secret-token-value")])]);
    const right = frame([row(0, [span("secret-token-walue")])]);
    const difference = compareCanonicalViews(viewOf(left), viewOf(right));
    const serialized = JSON.stringify(difference);
    expect(serialized).not.toContain("secret");
    expect(serialized).not.toContain("token");
  });
});

describe("painted-row fingerprints", () => {
  test("agree between a model row and the text the DOM painted", () => {
    const spans = [span("ab"), span("cd")];
    expect(paintedRowFingerprint(spans)).toBe(paintedTextFingerprint("abcd", 4));
  });

  test("survive a re-split but not a changed character", () => {
    expect(paintedRowFingerprint([span("abcd")])).toBe(
      paintedRowFingerprint([span("ab"), span("cd")]),
    );
    expect(paintedRowFingerprint([span("abcd")])).not.toBe(
      paintedRowFingerprint([span("abce")]),
    );
  });

  test("distinguish the same text at a different column width", () => {
    expect(paintedTextFingerprint("中", 2)).not.toBe(paintedTextFingerprint("中", 1));
  });
});
