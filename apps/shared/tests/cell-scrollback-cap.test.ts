// Pins WTERM_SCROLLBACK_CAPS against the REAL cores. nearScrollbackCap() arms
// the eviction probe only inside a band below a listed capacity, so a listed
// value that is too high silently disables eviction detection at saturation —
// the L11 "history mis-splices / absolute indices re-alias" class. A wasm swap
// that changes MAX_SCROLLBACK_LINES must fail here, loudly, not in a user's
// scrollback.

import { describe, test, expect } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { createWtermCore } from "../src/wterm-core-factory.ts";
import {
  WTERM_SCROLLBACK_CAPS, SB_SHIFT_SCAN_MAX, nearScrollbackCap,
} from "../src/cell/grid-to-cells.ts";

const LINES = 12_000;

function floodLines(core: { writeRaw(b: Uint8Array): void }, count: number): void {
  const enc = new TextEncoder();
  // One write per 500 lines keeps the wasm call count sane without changing
  // what the parser sees.
  let batch = "";
  for (let i = 1; i <= count; i++) {
    batch += `CAPLINE-${i}\r\n`;
    if (i % 500 === 0) { core.writeRaw(enc.encode(batch)); batch = ""; }
  }
  if (batch) core.writeRaw(enc.encode(batch));
}

describe("scrollback capacity is pinned to the real cores", () => {
  test("the roost-patched core saturates at a listed capacity", async () => {
    const core = await createWtermCore(80, 24);
    floodLines(core, LINES);
    const total = core.getScrollbackCount();
    expect(WTERM_SCROLLBACK_CAPS).toContain(total);
    expect(nearScrollbackCap(total)).toBe(true);
  });

  test("the stock inline core (factory fallback) saturates at a listed capacity", async () => {
    const core = await WasmBridge.load();
    core.init(80, 24);
    floodLines(core, LINES);
    const total = core.getScrollbackCount();
    expect(WTERM_SCROLLBACK_CAPS).toContain(total);
    expect(nearScrollbackCap(total)).toBe(true);
  });

  test("the probe stays off in the ordinary mid-history range", () => {
    // The waste this gate deletes: a session with a few thousand retained lines
    // typing at a prompt. No listed capacity is anywhere near, so no probe.
    expect(nearScrollbackCap(0)).toBe(false);
    expect(nearScrollbackCap(500)).toBe(false);
    expect(nearScrollbackCap(5_000)).toBe(false);
    expect(nearScrollbackCap(9_000)).toBe(false);
  });

  test("the probe arms one scan window below every listed capacity", () => {
    for (const cap of WTERM_SCROLLBACK_CAPS) {
      expect(nearScrollbackCap(cap)).toBe(true);
      expect(nearScrollbackCap(cap - SB_SHIFT_SCAN_MAX)).toBe(true);
      expect(nearScrollbackCap(cap - SB_SHIFT_SCAN_MAX - 1)).toBe(false);
    }
  });
});
