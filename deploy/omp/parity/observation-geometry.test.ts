import { describe, expect, test } from "bun:test";
import { relativeGeometryValue, sameVisualRow, strictlyBelow, terminalRowGap, type BoxEdges } from "./observation-geometry";

const box = (top: number, height: number, left = 0, width = 100): BoxEdges => ({ top, bottom: top + height, left, right: left + width });

describe("browser relative geometry", () => {
  test("sameRow accepts different heights that share a centre line or a top edge", () => {
    expect(sameVisualRow(box(10, 40), box(20, 20, 120))).toBe(true);
    expect(sameVisualRow(box(0, 20), box(0, 30, 120))).toBe(true);
    expect(sameVisualRow(box(0, 24), box(0.3, 24, 120))).toBe(true);
  });

  test("sameRow rejects stacked elements, however tightly they touch", () => {
    expect(sameVisualRow(box(0, 20), box(20, 20))).toBe(false);
    expect(sameVisualRow(box(0, 20), box(21, 20))).toBe(false);
    expect(sameVisualRow(box(0, 20), box(50, 20))).toBe(false);
  });

  test("sameRow rejects a tall element whose centre falls between two stacked rows", () => {
    const tall = box(0, 100, 120);
    expect(sameVisualRow(box(0, 20), tall)).toBe(false);
    expect(sameVisualRow(box(80, 20), tall)).toBe(false);
    expect(sameVisualRow(box(40, 20), tall)).toBe(true);
  });

  test("below holds for adjacent and separated subjects, never for overlap or above", () => {
    const reference = box(100, 20);
    expect(strictlyBelow(box(120, 20), reference)).toBe(true);
    expect(strictlyBelow(box(119.8, 20), reference)).toBe(true);
    expect(strictlyBelow(box(150, 20), reference)).toBe(true);
    expect(strictlyBelow(box(110, 20), reference)).toBe(false);
    expect(strictlyBelow(box(100, 20, 120), reference)).toBe(false);
    expect(strictlyBelow(box(60, 20), reference)).toBe(false);
  });

  test("relative reads route through the same predicates", () => {
    expect(relativeGeometryValue(box(120, 20), box(100, 20), "below")).toBe(true);
    expect(relativeGeometryValue(box(120, 20), box(100, 20), "sameRow")).toBe(false);
    expect(relativeGeometryValue(box(100, 20, 150), box(100, 20), "columnGap")).toBe(50);
    expect(() => relativeGeometryValue(box(0, 1), box(0, 1), "nope")).toThrow("Unsupported relative geometry");
  });
});

describe("terminal row relation", () => {
  test("a subject on the row right after the reference is below it (gap 0); same row or above is not", () => {
    expect(terminalRowGap(5, 4, 1)).toBe(0);
    expect(terminalRowGap(7, 4, 1)).toBe(2);
    expect(terminalRowGap(6, 4, 3)).toBe(-1);
    expect(terminalRowGap(4, 4, 1)).toBe(-1);
    expect(terminalRowGap(2, 4, 1)).toBe(-3);
  });
});
