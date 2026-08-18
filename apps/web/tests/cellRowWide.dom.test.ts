// Wide-glyph COLUMN OCCUPANCY in the paint (CellSpan.columns, R11).
//
// The worker's core stores a double-width glyph as a width-2 LEAD plus a
// width-0 CONTINUATION cell. The wire folds that continuation into the lead's
// `columns` instead of emitting it as a space-bearing cell, so the paint has to
// derive geometry from `columns` and never from `text.length`. What breaks when
// it does not:
//   * one phantom space per wide glyph ("中  文"), which is also what a copy of
//     the selection would put on the clipboard;
//   * every column right of a wide glyph shifted, so the cursor overlay (left:
//     Ncolumns ch) and the find highlights land on the wrong cells;
//   * a find hit boundary slicing an astral glyph into two lone surrogates.
//
// No jsdom (repo convention, see cellRenderer.dom.test.ts): a small fake covers
// exactly what renderRow touches. `1ch` is one grid column by construction —
// .cell-viewport is `width: cols * 1ch` (styles/sidebar.css) — so summing each
// painted child's pinned ch box (or its code units, for an unboxed narrow run)
// is the painted width of the row.

import { describe, test, expect } from "bun:test";
import { renderRow, rowHash, spanStyle } from "../src/lib/cellRow.ts";
import type { FindHit } from "../src/lib/cellRow.ts";
import { DEFAULT_COLOR, rowColumns, spansText, type CellRow, type CellSpan } from "@roost/shared/cell";

// ── minimal fake DOM ──────────────────────────────────────────────────────
class FakeEl {
  className = "";
  textContent = "";
  children: FakeEl[] = [];
  attrs: Record<string, string> = {};
  constructor(public tagName: string) {}
  setAttribute(key: string, value: string): void { this.attrs[key] = value; }
  appendChild(child: FakeEl): FakeEl { this.children.push(child); return child; }
}
const fakeDoc = {
  createElement: (tag: string) => new FakeEl(tag),
  createTextNode: (text: string) => {
    const node = new FakeEl("#text");
    node.textContent = text;
    return node;
  },
} as unknown as Document;

const paint = (row: CellRow, hits?: readonly FindHit[], activeCol?: number): FakeEl =>
  renderRow(row, fakeDoc, hits, activeCol) as unknown as FakeEl;

// ── span fixtures, shaped exactly as rowToSpans emits them ────────────────
function run(text: string, over: Partial<CellSpan> = {}): CellSpan {
  return {
    text, columns: text.length,
    fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined,
    ...over,
  };
}
/** An atomic single cell: a wide lead (2 columns) or a narrow astral glyph (1). */
function atom(text: string, columns: number): CellSpan {
  return {
    text, columns,
    fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0, fgRgb: undefined, bgRgb: undefined,
  };
}
const rowOf = (spans: CellSpan[]): CellRow => ({ index: 0, spans });

/** Columns the painted DOM occupies: a pinned box counts its declared ch width,
 *  an unboxed narrow run counts one column per code unit. */
function paintedColumns(el: FakeEl): number {
  let columns = 0;
  for (const child of el.children) {
    const pin = /width:(\d+)ch/.exec(child.attrs.style ?? "");
    columns += pin ? Number(pin[1]) : child.textContent.length;
  }
  return columns;
}
const paintedText = (el: FakeEl): string => el.children.map((c) => c.textContent).join("");
const paintedPieces = (el: FakeEl): Array<[string, string]> =>
  el.children.map((c) => [c.textContent, c.className]);

/** Grid column each painted child starts at — the space the cursor overlay and
 *  the find highlights are addressed in. */
function pieceStartColumns(el: FakeEl): number[] {
  const starts: number[] = [];
  let column = 0;
  for (const child of el.children) {
    starts.push(column);
    const pin = /width:(\d+)ch/.exec(child.attrs.style ?? "");
    column += pin ? Number(pin[1]) : child.textContent.length;
  }
  return starts;
}

const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;

describe("wide-glyph occupancy in the painted row", () => {
  test("a CJK row paints its grid columns and carries no phantom spaces", () => {
    // Grid: 中 中 文 文 ' ' o k  →  7 columns, 5 characters.
    const row = rowOf([atom("中", 2), atom("文", 2), run(" ok")]);
    expect(rowColumns(row.spans)).toBe(7);
    expect(spansText(row.spans)).toBe("中文 ok");

    const el = paint(row);
    expect(paintedColumns(el)).toBe(7);
    // What a selection copy yields: no space between the ideographs.
    expect(paintedText(el)).toBe("中文 ok");
  });

  test("an atomic span is pinned to its column box; a narrow run is not", () => {
    const wide = atom("中", 2);
    const flag = atom("\u{1F1FA}", 1); // regional indicator: astral, ONE column
    expect(spanStyle(wide)).toContain("display:inline-block;width:2ch");
    expect(spanStyle(flag)).toContain("display:inline-block;width:1ch");
    expect(spanStyle(run("ok"))).not.toContain("width");
  });

  test("emoji with a ZWJ sequence and a skin-tone modifier keep every glyph whole", () => {
    // As the pinned core stores it: each pictograph is its own wide cell and the
    // joiners/modifiers are their own cells.
    const row = rowOf([
      atom("🐙", 2), run(" "),
      atom("👨", 2), run("\u200d"), atom("👩", 2), run("\u200d"), atom("👧", 2),
      run(" "), atom("👋", 2), atom("🏽", 2),
    ]);
    expect(rowColumns(row.spans)).toBe(16);

    // Highlight a range that starts and ends INSIDE the ZWJ sequence: a
    // column→code-unit split would cut a surrogate pair in half.
    const el = paint(row, [{ col: 4, len: 4 }], 4);
    expect(paintedColumns(el)).toBe(16);
    expect(paintedText(el)).toBe("🐙 👨\u200d👩\u200d👧 👋🏽");
    for (const piece of el.children) {
      expect(LONE_SURROGATE.test(piece.textContent)).toBe(false);
    }
  });

  test("a hit that touches a wide glyph highlights the whole glyph", () => {
    // Columns: a=0 b=1 中=2-3 c=4 d=5. The hit covers b and only the LEAD
    // column of 中, so the glyph must still paint as one highlighted piece.
    const row = rowOf([run("ab"), atom("中", 2), run("cd")]);
    const el = paint(row, [{ col: 1, len: 2 }]);
    expect(paintedPieces(el)).toEqual([
      ["a", ""], ["b", "cell-find-hit"], ["中", "cell-find-hit"], ["cd", ""],
    ]);
    expect(paintedColumns(el)).toBe(6);
  });

  test("hit columns past a wide glyph land on the characters the worker matched", () => {
    // Columns: 中=0-1 a=2 b=3 c=4. A hit at column 3 is "bc", not "ab" — the
    // off-by-one a text.length walk would produce.
    const row = rowOf([atom("中", 2), run("abc")]);
    const el = paint(row, [{ col: 3, len: 2 }]);
    expect(paintedPieces(el)).toEqual([["中", ""], ["a", ""], ["bc", "cell-find-hit"]]);
    expect(pieceStartColumns(el)).toEqual([0, 2, 3]);
  });

  test("the active hit keeps its own class across a wide glyph", () => {
    const row = rowOf([run("x"), atom("文", 2), run("y")]);
    const el = paint(row, [{ col: 1, len: 2 }], 1);
    expect(el.children[1]!.className).toBe("cell-find-hit cell-find-hit-active");
    expect(el.children[1]!.textContent).toBe("文");
    // Not the active hit → plain highlight.
    expect(paint(row, [{ col: 1, len: 2 }], 9).children[1]!.className).toBe("cell-find-hit");
  });

  test("every painted piece starts at the grid column the model gives it", () => {
    // The cursor overlay is placed at `cursorCol` ch, so a painted row whose
    // pieces drift from model columns puts the cursor on the wrong cell.
    const row = rowOf([run("ab"), atom("中", 2), run("c"), atom("👋", 2), run("d")]);
    expect(pieceStartColumns(paint(row))).toEqual([0, 2, 4, 5, 7]);
    expect(paintedColumns(paint(row))).toBe(8);
    expect(rowColumns(row.spans)).toBe(8);
  });

  test("row identity folds occupancy, so the same text at a different width repaints", () => {
    // Two narrow cells vs one wide glyph twice: identical concatenated text,
    // different grid geometry. A hash that ignored `columns` would skip the
    // repaint and leave the previous row's pixels on screen.
    const asRun = rowOf([run("中中")]);
    const asWide = rowOf([atom("中", 2), atom("中", 2)]);
    expect(spansText(asRun.spans)).toBe(spansText(asWide.spans));
    expect(rowHash(asRun)).not.toBe(rowHash(asWide));
  });
});
