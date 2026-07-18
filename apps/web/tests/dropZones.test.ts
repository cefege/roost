// Edge-band drop-zone math (lib/dropZones.ts) — the drag-to-tile routing.

import { describe, test, expect } from "bun:test";
import { dropZoneFor, zoneToSplit, zoneRect, tileTargetFor, type Rect, type PaneBox } from "../src/lib/dropZones.ts";

const R: Rect = { x: 0, y: 0, w: 800, h: 600 }; // bands: 200px horiz, 150px vert

describe("dropZoneFor", () => {
  test("center of a pane = merge zone", () => {
    expect(dropZoneFor(R, 400, 300)).toBe("center");
  });
  test("outer band on each side resolves to that edge", () => {
    expect(dropZoneFor(R, 20, 300)).toBe("left");
    expect(dropZoneFor(R, 790, 300)).toBe("right");
    expect(dropZoneFor(R, 400, 10)).toBe("top");
    expect(dropZoneFor(R, 400, 590)).toBe("bottom");
  });
  test("band is at least 80px even on a small pane", () => {
    const small: Rect = { x: 0, y: 0, w: 200, h: 200 }; // 25% = 50 → clamped to 80
    expect(dropZoneFor(small, 70, 100)).toBe("left"); // within 80 of the left
    expect(dropZoneFor(small, 100, 100)).toBe("center");
  });
  test("corner favours the horizontal edge (closest wins, left/right tie-break)", () => {
    // top-left corner: left dist 10, top dist 10 → tie → horizontal (left)
    expect(dropZoneFor(R, 10, 10)).toBe("left");
  });
  test("respects a non-zero rect origin", () => {
    const off: Rect = { x: 400, y: 300, w: 800, h: 600 };
    expect(dropZoneFor(off, 800, 600)).toBe("center");
    expect(dropZoneFor(off, 420, 600)).toBe("left");
  });
});

describe("zoneToSplit", () => {
  test("edges map to a split with the right side + orientation", () => {
    expect(zoneToSplit("left")).toEqual({ dir: "row", insertFirst: true });
    expect(zoneToSplit("right")).toEqual({ dir: "row", insertFirst: false });
    expect(zoneToSplit("top")).toEqual({ dir: "col", insertFirst: true });
    expect(zoneToSplit("bottom")).toEqual({ dir: "col", insertFirst: false });
  });
  test("center is not a split (merge)", () => {
    expect(zoneToSplit("center")).toBeNull();
  });
  test("reorder is not a split", () => {
    expect(zoneToSplit("reorder")).toBeNull();
  });
});

describe("zoneRect", () => {
  test("edge zones highlight the correct half; center highlights the whole pane", () => {
    expect(zoneRect(R, "left")).toEqual({ x: 0, y: 0, w: 400, h: 600 });
    expect(zoneRect(R, "right")).toEqual({ x: 400, y: 0, w: 400, h: 600 });
    expect(zoneRect(R, "top")).toEqual({ x: 0, y: 0, w: 800, h: 300 });
    expect(zoneRect(R, "bottom")).toEqual({ x: 0, y: 300, w: 800, h: 300 });
    expect(zoneRect(R, "center")).toEqual({ x: 0, y: 0, w: 800, h: 600 });
    expect(zoneRect(R, "reorder")).toEqual({ x: 0, y: 0, w: 800, h: 600 });
  });
});

const STRIP = 40;
const HOME: PaneBox = { paneId: "home", rect: { x: 0, y: 0, w: 400, h: 600 } };
const OTHER: PaneBox = { paneId: "other", rect: { x: 400, y: 0, w: 400, h: 600 } };
const PANES = [HOME, OTHER];

describe("tileTargetFor", () => {
  test("home body center → reorder (overlay shows, but not a tile)", () => {
    const t = tileTargetFor(PANES, "home", 200, 300, STRIP);
    expect(t?.zone).toBe("reorder");
    expect(t?.paneId).toBe("home");
  });
  test("home body edges → split zones", () => {
    expect(tileTargetFor(PANES, "home", 10, 300, STRIP)?.zone).toBe("left");
    expect(tileTargetFor(PANES, "home", 390, 300, STRIP)?.zone).toBe("right");
    expect(tileTargetFor(PANES, "home", 200, 590, STRIP)?.zone).toBe("bottom");
  });
  test("home strip band → null (strip slide handles reorder)", () => {
    expect(tileTargetFor(PANES, "home", 200, 10, STRIP)).toBeNull();
  });
  test("other pane center/strip → merge (center)", () => {
    expect(tileTargetFor(PANES, "home", 600, 300, STRIP)?.zone).toBe("center");
    expect(tileTargetFor(PANES, "home", 600, 10, STRIP)?.zone).toBe("center");
  });
  test("other pane edges → split", () => {
    expect(tileTargetFor(PANES, "home", 410, 300, STRIP)?.zone).toBe("left");
    expect(tileTargetFor(PANES, "home", 790, 300, STRIP)?.zone).toBe("right");
  });
  test("pointer off all panes → null", () => {
    expect(tileTargetFor(PANES, "home", 900, 300, STRIP)).toBeNull();
    expect(tileTargetFor(PANES, "home", 200, 900, STRIP)).toBeNull();
  });
});
