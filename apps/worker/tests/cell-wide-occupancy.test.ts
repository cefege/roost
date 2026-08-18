// Wide-glyph COLUMN OCCUPANCY against the REAL pinned core (@wterm/core 0.3.4).
//
// 0.3.4 stores a double-width glyph as a width-2 LEAD cell plus a width-0
// CONTINUATION cell ("lead-plus-continuation"; core-trace-oracle.test.ts pins
// that observation). The cell wire folds the continuation into the lead span's
// `columns` instead of emitting it as its own space-bearing cell. Every case
// below is read off the live core rather than a mock, because the model of a
// continuation cell — and what the core leaves behind when one is overwritten —
// is exactly what a version bump can change under us.
//
// W1 CJK, W2 emoji (ZWJ sequence + skin-tone modifier), W3 the wrap boundary,
// W4 a wide glyph overwritten by a narrow one, W5 the frame/wire contract, and
// W6 the cursor column the SPA paints at.

import { describe, test, expect } from "bun:test";
import {
  gridToCellFrame, rowColumns, spanIsAtomic, spansText, viewportRowSpans,
  type CellSpan,
} from "@roost/shared/cell";
import { cellFrameToProto, protoToCellFrame } from "@roost/shared/cell/cell-proto";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import type { TerminalCore } from "@wterm/core";

const encoder = new TextEncoder();

async function coreWith(cols: number, rows: number, payload: string): Promise<TerminalCore> {
  const core = await createWtermCore(cols, rows);
  core.writeRaw(encoder.encode(payload));
  return core;
}

/** [text, columns] per span of one viewport row — the wire's whole geometry. */
const shapeOf = (spans: readonly CellSpan[]): Array<[string, number]> =>
  spans.map((span) => [span.text, span.columns]);

describe("wide-glyph occupancy on the cell wire", () => {
  test("W1 — CJK: each ideograph is one atomic 2-column span, no phantom spaces", async () => {
    const core = await coreWith(40, 4, "中文测试 日本語 한국어");
    const spans = viewportRowSpans(core, 0, 40);

    expect(shapeOf(spans)).toEqual([
      ["中", 2], ["文", 2], ["测", 2], ["试", 2], [" ", 1],
      ["日", 2], ["本", 2], ["語", 2], [" ", 1],
      ["한", 2], ["국", 2], ["어", 2],
    ]);
    // The painted text is what a selection copy yields: no space between
    // ideographs, which is exactly what an emitted continuation cell produced.
    expect(spansText(spans)).toBe("中文测试 日本語 한국어");
    expect(rowColumns(spans)).toBe(22);
    expect(spans.every((span) => spanIsAtomic(span) === (span.columns === 2))).toBe(true);
  });

  test("W2 — emoji: ZWJ sequence and skin-tone modifier keep every cell's width", async () => {
    const core = await coreWith(40, 4, "🐙 👨‍👩‍👧 👋🏽");
    const spans = viewportRowSpans(core, 0, 40);

    // The core gives each pictograph its own wide cell and each joiner/modifier
    // its own cell; the wire mirrors that model exactly rather than re-clustering.
    expect(shapeOf(spans)).toEqual([
      ["🐙", 2], [" ", 1],
      ["👨", 2], ["\u200d", 1], ["👩", 2], ["\u200d", 1], ["👧", 2], [" ", 1],
      ["👋", 2], ["🏽", 2],
    ]);
    expect(spansText(spans)).toBe("🐙 👨‍👩‍👧 👋🏽");
    expect(rowColumns(spans)).toBe(16);
    // 🐙 has as many UTF-16 code units as columns; it must still be atomic, or a
    // hit boundary at its second column would slice the surrogate pair.
    expect(spans.filter((span) => span.text === "🐙").map(spanIsAtomic)).toEqual([true]);
  });

  test("W3 — a wide glyph that will not fit wraps and leaves one blank column", async () => {
    // cols=5: "abcd" fills 0..3, 中 needs two columns and only one is left.
    const core = await coreWith(5, 4, "abcd中X");

    const first = viewportRowSpans(core, 0, 5);
    expect(shapeOf(first)).toEqual([["abcd", 4]]);   // column 4 is blank padding
    expect(rowColumns(first)).toBeLessThanOrEqual(5);

    const second = viewportRowSpans(core, 1, 5);
    expect(shapeOf(second)).toEqual([["中", 2], ["X", 1]]);
    expect(core.getCursor()).toMatchObject({ row: 1, col: 3 });
  });

  test("W4 — overwriting a wide glyph collapses its occupancy to narrow columns", async () => {
    // Overwrite the LEAD: the core clears both of its cells, so the row is two
    // narrow columns followed by the untouched second glyph.
    const overLead = await coreWith(6, 2, "中文\x1b[1;1HA");
    expect(shapeOf(viewportRowSpans(overLead, 0, 6))).toEqual([["A ", 2], ["文", 2]]);
    expect(rowColumns(viewportRowSpans(overLead, 0, 6))).toBe(4);

    // Overwrite the CONTINUATION: the lead goes too, and the blank it leaves is
    // a real narrow column, not an orphan continuation.
    const overContinuation = await coreWith(6, 2, "中文\x1b[1;2HB");
    expect(shapeOf(viewportRowSpans(overContinuation, 0, 6))).toEqual([[" B", 2], ["文", 2]]);
  });

  test("W5 — a frame full of wide glyphs survives the wire's occupancy validation", async () => {
    const core = await coreWith(20, 6, "中文\r\n🐙 ok\r\n한국어\r\n");
    const frame = gridToCellFrame(core, 1, "wide-grid:0");

    for (const row of frame.viewportRows) {
      expect(rowColumns(row.spans)).toBeLessThanOrEqual(frame.cols);
      for (const span of row.spans) expect(span.columns).toBeGreaterThanOrEqual(1);
    }
    // protoToCellFrame validates occupancy on decode, so a round trip proves the
    // emitter and the contract agree for real wide content.
    const decoded = protoToCellFrame(cellFrameToProto(frame, "sess"));
    expect(decoded.viewportRows.map((row) => spansText(row.spans)))
      .toEqual(frame.viewportRows.map((row) => spansText(row.spans)));
    expect(decoded.viewportRows.map((row) => rowColumns(row.spans)))
      .toEqual(frame.viewportRows.map((row) => rowColumns(row.spans)));
  });

  test("W6 — the cursor column equals the painted width of the row before it", async () => {
    // The cursor overlay is placed at `cursorCol` ch. If a row's painted spans
    // were one column wider per wide glyph, the cursor would sit that many
    // columns left of the text it is supposed to follow.
    const core = await coreWith(40, 4, "中文a");
    const spans = viewportRowSpans(core, 0, 40);
    expect(rowColumns(spans)).toBe(5);
    expect(core.getCursor().col).toBe(rowColumns(spans));

    const frame = gridToCellFrame(core, 1, "wide-grid:0");
    expect(frame.cursorCol).toBe(rowColumns(frame.viewportRows[0]!.spans));
  });
});
