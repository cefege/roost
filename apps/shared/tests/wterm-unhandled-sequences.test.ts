// The core's unhandled-sequence ring, decoded against the real pinned WASM.
//
// This ring is the only evidence Roost can get that the terminal core silently
// dropped an escape sequence ("renders wrong in Roost, fine in iTerm"), and
// @wterm/core 0.3.4's own bridge decodes it at the WRONG OFFSETS: `DebugLogEntry`
// is a plain Zig struct, so the compiler puts its `[4]u16 params` first and the
// three u8s (`final_byte`, `private_marker`, `param_count`) after them, while
// wasm-bridge.js reads the final byte at +0 and the params from +4. Its output is
// therefore a parameter's low byte wearing the final byte's name.
//
// createWtermCore installs the correct reader, so these assertions are what pins
// the layout: a rebuilt/rebumped module that reordered the struct fails HERE,
// loudly, instead of quietly reporting sequences that were never sent.

import { describe, test, expect } from "bun:test";
import { createWtermCore, unhandledSequenceRing } from "../src/wterm-core-factory.ts";
import type { TerminalCore, UnhandledSequence } from "@wterm/core";

const enc = new TextEncoder();

async function coreFed(payload: string): Promise<TerminalCore> {
  const core = await createWtermCore(40, 6);
  core.writeRaw(enc.encode(payload));
  return core;
}

function ringOf(core: TerminalCore) {
  const ring = unhandledSequenceRing(core);
  if (ring === null) throw new Error("createWtermCore must register a ring reader");
  return ring;
}

describe("unhandled-sequence ring", () => {
  test("decodes final byte, private marker, count and parameters at the real offsets", async () => {
    // DECSCUSR (cursor style), a private-prefixed final, and a `?`-prefixed one:
    // all parse cleanly, all are dropped by the dispatcher, all carry parameters —
    // exactly the shape 0.3.4's own decode mangles.
    const core = await coreFed("\x1b[2 q\x1b[>10;20;30W\x1b[?5Z");
    expect(core.getUnhandledSequences()).toEqual([
      { final: "q", private: "", paramCount: 1, params: [2] },
      { final: "W", private: ">", paramCount: 3, params: [10, 20, 30] },
      { final: "Z", private: "?", paramCount: 1, params: [5] },
    ] satisfies UnhandledSequence[]);
  });

  test("a parameterless sequence is reported, not swallowed", async () => {
    // DA1: the core's CSI dispatcher does not implement it (Roost answers the
    // probe itself, host-side), and it is the case 0.3.4's decode loses
    // COMPLETELY — reading the final byte at the params' offset yields 0, which
    // its loop then skips as an empty slot. The most common unhandled shape there
    // is, reported as nothing at all.
    const core = await coreFed("\x1b[c");
    expect(core.getUnhandledSequences()).toEqual([
      { final: "c", private: "", paramCount: 0, params: [] },
    ] satisfies UnhandledSequence[]);
    expect(ringOf(core).total()).toBe(1);
  });

  test("records at most four parameters while reporting how many arrived", async () => {
    const core = await coreFed("\x1b[1;2;3;4;5Q");
    expect(core.getUnhandledSequences()).toEqual([
      { final: "Q", private: "", paramCount: 5, params: [1, 2, 3, 4] },
    ] satisfies UnhandledSequence[]);
  });

  test("sequences the core implements are not logged", async () => {
    // Erase, cursor position, SGR, scroll region, plain text: a session of these
    // must leave the ring empty, or every session would report noise.
    const core = await coreFed("hello\x1b[2J\x1b[3;4H\x1b[1;31m\x1b[0m\x1b[1;5r\r\n");
    expect(core.getUnhandledSequences()).toEqual([]);
    expect(ringOf(core).total()).toBe(0);
  });

  test("total counts every occurrence; the window keeps the newest, oldest first", async () => {
    let spray = "";
    for (let i = 1; i <= 40; i++) spray += `\x1b[${i}Y`;
    const core = await coreFed(spray + "\x1b[7Y");
    const ring = ringOf(core);

    expect(ring.capacity).toBe(32);
    // 41 logged, duplicates included: the ring is a window onto a total, which is
    // what lets a consumer hold an exact high-water mark against it.
    expect(ring.total()).toBe(41);
    const window = ring.entries();
    expect(window).toHaveLength(32);
    // Wrapped: parameters 10..40 then the repeat of 7, oldest → newest.
    expect(window.map((seq) => seq.params[0])).toEqual([
      ...Array.from({ length: 31 }, (_unused, i) => i + 10), 7,
    ]);
    expect(window.every((seq) => seq.final === "Y" && seq.private === "")).toBe(true);
  });

  test("the ring is never cleared: reading it twice returns the same window", async () => {
    // The whole reason a consumer needs its own mark — there is no clear, and no
    // export that could clear it.
    const core = await coreFed("\x1b[2 q");
    const first = core.getUnhandledSequences();
    expect(core.getUnhandledSequences()).toEqual(first);
    expect(ringOf(core).total()).toBe(1);
  });

  test("each core instance owns its own ring", async () => {
    const noisy = await coreFed("\x1b[2 q");
    const quiet = await coreFed("plain\r\n");
    expect(ringOf(noisy).total()).toBe(1);
    expect(ringOf(quiet).total()).toBe(0);
    expect(quiet.getUnhandledSequences()).toEqual([]);
  });
});
