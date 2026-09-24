import { describe, expect, test } from "bun:test";
import type { TerminalCore } from "@wterm/core";
import { createWtermCore } from "../src/wterm-core-factory.ts";

function rowText(core: TerminalCore, row: number): string {
  let text = "";
  for (let col = 0; col < core.getCols(); col++) {
    const cell = core.getCell(row, col);
    text += cell.chars ?? String.fromCodePoint(cell.char);
  }
  return text.trimEnd();
}

function paintRows(core: TerminalCore, labels: readonly string[]): void {
  for (let row = 0; row < labels.length; row++) {
    core.writeString(`\x1b[${row + 1};1H${labels[row]}`);
  }
}

function expectEveryViewportRowDirty(core: TerminalCore): void {
  for (let row = 0; row < core.getRows(); row++) {
    expect(core.isDirtyRow(row)).toBe(true);
  }
}

describe("patched wterm in-place resize", () => {
  test("primary shrink/grow restores scrolled rows while preserving differential writes", async () => {
    const core = await createWtermCore(12, 6);
    const before = Array.from({ length: 6 }, (_, row) => `P${row}-STATIC`);
    paintRows(core, before);

    core.clearDirty();
    core.resize(10, 3);

    expect(core.getScrollbackCount()).toBe(3);
    expect(Array.from({ length: 3 }, (_, row) => rowText(core, row))).toEqual(before.slice(3));
    expect(core.getCursor().row).toBe(2);
    expectEveryViewportRowDirty(core);

    core.writeString("\x1b[2;4HCHANGED");
    core.clearDirty();
    core.resize(12, 6);

    expect(core.getScrollbackCount()).toBe(0);
    expect(Array.from({ length: 6 }, (_, row) => rowText(core, row))).toEqual([
      before[0],
      before[1],
      before[2],
      before[3],
      "P4-CHANGED",
      before[5],
    ]);
    expectEveryViewportRowDirty(core);
  });

  test("alternate resize top-anchors its grid and restores the resized primary grid", async () => {
    const core = await createWtermCore(12, 6);
    const primary = Array.from({ length: 6 }, (_, row) => `M${row}-static`);
    const alternate = Array.from({ length: 6 }, (_, row) => `A${row}-static`);
    paintRows(core, primary);
    const primaryCursor = core.getCursor();

    core.writeString("\x1b[?1049h");
    paintRows(core, alternate);
    core.clearDirty();
    core.resize(10, 3);

    expect(core.usingAltScreen()).toBe(true);
    expect(core.getScrollbackCount()).toBe(3);
    expect(Array.from({ length: 3 }, (_, row) => rowText(core, row))).toEqual(alternate.slice(0, 3));
    expect(core.getCursor().row).toBe(2);
    expectEveryViewportRowDirty(core);

    core.writeString("\x1b[2;1HALT-DIFF");
    core.clearDirty();
    core.resize(12, 6);

    expect(core.usingAltScreen()).toBe(true);
    expect(core.getScrollbackCount()).toBe(0);
    expect(Array.from({ length: 6 }, (_, row) => rowText(core, row))).toEqual([
      alternate[0],
      "ALT-DIFFc",
      alternate[2],
      "",
      "",
      "",
    ]);
    expectEveryViewportRowDirty(core);

    core.writeString("\x1b[?1049l");
    expect(core.usingAltScreen()).toBe(false);
    expect(Array.from({ length: 6 }, (_, row) => rowText(core, row))).toEqual(primary);
    expect(core.getCursor()).toEqual(primaryCursor);
  });

  test("both saved primary and active alternate grids sanitize a split wide cell", async () => {
    const core = await createWtermCore(6, 2);
    core.writeString("\x1b[1;5H界");
    expect(core.getCell(0, 4).width).toBe(2);
    expect(core.getCell(0, 5).width).toBe(0);

    core.writeString("\x1b[?1049h\x1b[1;5H界");
    core.resize(5, 2);
    core.resize(6, 2);

    for (const col of [4, 5]) {
      expect(core.getCell(0, col).char).toBe(32);
      expect(core.getCell(0, col).width).toBe(1);
    }

    core.writeString("\x1b[?1049l");
    for (const col of [4, 5]) {
      expect(core.getCell(0, col).char).toBe(32);
      expect(core.getCell(0, col).width).toBe(1);
    }
  });

  test("resize preserves parser, mode, and link state and clamps the global saved cursor", async () => {
    const core = await createWtermCore(12, 6);
    core.writeString("\x1b[?1h\x1b[?2004h\x1b[6;12H\x1b7");
    core.writeString("\x1b]8;;https://resize.test/link\x1b\\\x1b[?1002");

    core.resize(8, 3);
    core.writeString("h\x1b8L");

    expect(core.cursorKeysApp()).toBe(true);
    expect(core.bracketedPaste()).toBe(true);
    expect(core.mouseTracking?.()).toBe(1002);
    expect(core.getCursor()).toEqual({ row: 2, col: 7, visible: true });
    expect(core.getCell(2, 7).linkUri).toBe("https://resize.test/link");
  });

  test("accepts the 256 boundary, clamps 257, and dirties every clamped row", async () => {
    const core = await createWtermCore(255, 2);

    core.resize(256, 256);
    expect([core.getCols(), core.getRows()]).toEqual([256, 256]);

    core.resize(255, 2);
    core.clearDirty();
    core.resize(257, 257);

    expect([core.getCols(), core.getRows()]).toEqual([256, 256]);
    expectEveryViewportRowDirty(core);
  });
});
