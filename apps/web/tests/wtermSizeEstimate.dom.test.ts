// The pre-spawn PTY size hint and the live membership claim must be ONE
// measurement: the hint's cols/rows reach the keeper PTY directly, so a hint
// that disagrees with the claim that follows it starts every TUI at the wrong
// width. Both paths measure the mounted display box through
// terminalCellGeometry, so this file pins agreement plus the refusal rule —
// no measurable box means no hint, never a fabricated 1×1.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import {
  installFakeTerminalDom,
  mountFakeDeck,
  mountFakeSlot,
  type FakeDocument,
  type FakeElement,
  type InstalledFakeDom,
} from "./helpers/fakeTerminalDom.ts";

let dom: InstalledFakeDom;
let fakeDocument: FakeDocument;

mock.module("../src/browser/pageVisible.ts", () => ({ isPageVisible: () => true }));
mock.module("@roost/observability/diag", () => ({ diag: () => undefined }));

// Module-loading boundary: the units below bind the mocked visibility facade
// at import time, so they load after the mocks above. Browser globals are
// installed per test — nothing here reads them at import.
const { estimateWtermSize } = await import("../src/client/terminal-stream/wtermSizeEstimate.ts");
const { createCellTerminalViewport } = await import(
  "../src/components/terminal/cell-terminal-viewport.ts"
);

/** The live claim for one mounted display box, measured the way a mounted pane
 *  measures it — uncached cell dimensions force the shared probe. */
function liveClaim(display: FakeElement): { cols: number; rows: number } | null {
  const runtime = {
    backfill: null,
    cellHeight: 0,
    cellWidth: 0,
    display: () => display as unknown as HTMLDivElement,
    sessionId: "estimate-session",
    unmounted: false,
    view: null,
  };
  const viewport = createCellTerminalViewport(
    runtime as never,
    {} as never,
    () => false,
    () => true,
  );
  return viewport.measureViewport();
}

beforeEach(() => {
  dom = installFakeTerminalDom();
  fakeDocument = dom.document;
});

afterEach(() => {
  dom.restore();
});

describe("wterm size estimate", () => {
  test("the hint equals the live claim for the same display box", () => {
    const display = mountFakeSlot(mountFakeDeck(fakeDocument), {
      focused: true,
      visible: true,
      slotRect: { width: 900, height: 500 },
      displayWidth: 832,
      displayHeight: 424,
    });

    expect(estimateWtermSize()).toEqual({ cols: 80, rows: 20 });
    expect(liveClaim(display)).toEqual({ cols: 80, rows: 20 });
  });

  test("the focused pane's box is the hint in a split", () => {
    const deck = mountFakeDeck(fakeDocument);
    mountFakeSlot(deck, {
      focused: false,
      visible: true,
      slotRect: { width: 900, height: 500 },
      displayWidth: 632,
      displayHeight: 424,
    });
    mountFakeSlot(deck, {
      focused: true,
      visible: true,
      slotRect: { width: 900, height: 500 },
      displayWidth: 832,
      displayHeight: 424,
    });

    expect(estimateWtermSize()).toEqual({ cols: 80, rows: 20 });
  });

  test("a mid-layout deck yields no hint instead of 1x1", () => {
    const deck = mountFakeDeck(fakeDocument);
    deck.clientWidth = 4;
    deck.clientHeight = 3;
    deck.rect = { width: 4, height: 3 };
    mountFakeSlot(deck, {
      focused: true,
      visible: true,
      slotRect: { width: 4, height: 3 },
      displayWidth: 4,
      displayHeight: 3,
    });

    expect(estimateWtermSize()).toBeNull();
  });

  test("a parked pane's retained box is not a hint source", () => {
    mountFakeSlot(mountFakeDeck(fakeDocument), {
      focused: false,
      visible: false,
      slotRect: { width: 900, height: 500 },
      displayWidth: 832,
      displayHeight: 424,
    });

    expect(estimateWtermSize()).toBeNull();
  });

  test("no mounted terminal yields no hint", () => {
    mountFakeDeck(fakeDocument);

    expect(estimateWtermSize()).toBeNull();
  });
});
