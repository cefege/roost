// Pins the ring's EVICTION ACCOUNTING against the REAL cores. Every absolute
// row index Roost exposes is measured from `sbDropped` — the lines the ring has
// discarded — so if that origin is wrong by N, every held index on every viewer
// names a line N rows away: the L11 "history mis-splices / absolute indices
// re-alias" class.
//
// Roost used to INFER the origin by re-identifying the previously-newest lines
// by content hash inside a band below a hardcoded capacity table. 0.3.4 counts
// discards in the core, so the origin is now read, and the contract this file
// pins is arithmetic rather than heuristic: retained + discarded must equal
// everything the stream ever pushed into the ring, and discarded must never
// rewind. A wasm swap that changes MAX_SCROLLBACK_LINES, or a bridge that stops
// counting, must fail here — loudly — not in a user's scrollback.

import { describe, test, expect } from "bun:test";
import { WasmBridge } from "@wterm/core";
import type { TerminalCore } from "@wterm/core";
import { createWtermCore } from "../src/wterm-core-factory.ts";
import { initCellEmitState, scrollbackOrigin } from "../src/cell/index.ts";

const LINES = 12_000;
const ROWS = 24;
// Newline-terminated lines land on successive grid rows and only scroll into
// history once they leave the viewport, so writing N of them into a fresh
// ROWS-row grid pushes N - (ROWS - 1) lines into the ring; the last ROWS-1 are
// still on the live grid. Derived from the writer, never from the counter under
// test.
const pushedBy = (lines: number): number => lines - (ROWS - 1);
// Roost's patched MAX_SCROLLBACK_LINES (scripts/wterm-0.3.4-scrollback.patch
// raises upstream's 1k). Stock @wterm/core is still a real 1k core.
const ROOST_CAPACITY = 10_000;
const STOCK_CAPACITY = 1_000;

function floodLines(core: TerminalCore, count: number, from = 1): void {
  const enc = new TextEncoder();
  // One write per 500 lines keeps the wasm call count sane without changing
  // what the parser sees.
  let batch = "";
  for (let i = from; i < from + count; i++) {
    batch += `CAPLINE-${i}\r\n`;
    if ((i - from + 1) % 500 === 0) { core.writeRaw(enc.encode(batch)); batch = ""; }
  }
  if (batch) core.writeRaw(enc.encode(batch));
}

/** The ring's own arithmetic: retained + discarded must account for every line
 *  the stream scrolled into history, with nothing lost between them. */
function expectExactAccounting(core: TerminalCore, pushed: number, capacity: number): void {
  const retained = core.getScrollbackCount();
  const discarded = scrollbackOrigin(core, initCellEmitState("cap"));
  expect(retained).toBe(capacity);
  expect(discarded).toBe(pushed - capacity);
  expect(retained + discarded).toBe(pushed);
}

describe("the ring's eviction origin is authoritative on the real cores", () => {
  test("the roost-patched core retains 10k and counts every discard", async () => {
    const core = await createWtermCore(80, ROWS);
    floodLines(core, LINES);
    expectExactAccounting(core, pushedBy(LINES), ROOST_CAPACITY);
  });

  test("the stock inline core retains 1k and counts every discard", async () => {
    const core = await WasmBridge.load();
    core.init(80, ROWS);
    floodLines(core, LINES);
    expectExactAccounting(core, pushedBy(LINES), STOCK_CAPACITY);
  });

  test("nothing is discarded below capacity", async () => {
    const core = await createWtermCore(80, ROWS);
    floodLines(core, 500);
    expect(core.getScrollbackCount()).toBe(pushedBy(500));
    expect(scrollbackOrigin(core, initCellEmitState("cap"))).toBe(0);
  });

  test("the origin only ever grows as the ring keeps rolling", async () => {
    const core = await createWtermCore(80, ROWS);
    const emit = initCellEmitState("cap");
    floodLines(core, LINES);
    const first = scrollbackOrigin(core, emit);
    expect(first).toBeGreaterThan(0);
    floodLines(core, 3_000, LINES + 1);
    // Exactly the lines pushed since — at saturation each new line evicts one.
    // An origin that rewound or stalled is the silent re-alias this counter
    // exists to make impossible, and it is invisible to getScrollbackCount(),
    // which pins at capacity through all of it.
    expect(scrollbackOrigin(core, emit)).toBe(first + 3_000);
    expect(core.getScrollbackCount()).toBe(ROOST_CAPACITY);
  });

  test("the rebuild base offsets the origin without touching the core's count", async () => {
    // A resize replays the raw ring into a FRESH core, whose own discarded
    // counter restarts at 0 while the SPA's indices must not rewind;
    // session-resize-capture pins the difference in sbOrigin.
    const core = await createWtermCore(80, ROWS);
    floodLines(core, LINES);
    const raw = scrollbackOrigin(core, initCellEmitState("cap"));
    const rebased = scrollbackOrigin(core, { ...initCellEmitState("cap"), sbOrigin: 40_000 });
    expect(rebased).toBe(raw + 40_000);
  });

  test("a core that cannot report discards fails loudly instead of guessing", () => {
    const blind = { getScrollbackCount: () => 10 } as unknown as TerminalCore;
    expect(() => scrollbackOrigin(blind, initCellEmitState("cap")))
      .toThrow(/getScrollbackDiscardedCount/);
  });
});
