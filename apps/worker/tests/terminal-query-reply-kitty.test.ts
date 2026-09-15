// The keyboard-protocol half of the reply lane, against a real patched core.
//
// The core answers the Kitty keyboard query itself, and answering it is a
// promise Roost's browser input encoder cannot keep: it sends legacy key bytes
// only. These tests pin that the probe leaves the lane silent while every other
// native reply still reaches the pty in probe order.

import { describe, test, expect } from "bun:test";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { answerQueries, drainCoreReplies, PRIMARY_DA_REPLY, type QueryCarry } from "../src/terminal-query-reply.ts";

const encoder = new TextEncoder();

function carry(): QueryCarry {
  return { query_carry: new Uint8Array(0) };
}

describe("kitty keyboard negotiation stays unanswered", () => {
  test("the core answers CSI ? u but the lane forwards nothing", async () => {
    const core = await createWtermCore(80, 24);
    core.writeRaw(encoder.encode("\x1b[?u"));
    const queued: string[] = [];
    for (;;) {
      const reply = core.getResponse();
      if (reply === null || reply.length === 0) break;
      queued.push(reply);
    }
    // Guards the premise: if upstream stops answering, this filter is dead code.
    expect(queued).toEqual(["\x1b[?0u"]);

    const fresh = await createWtermCore(80, 24);
    fresh.writeRaw(encoder.encode("\x1b[?u"));
    expect(drainCoreReplies(fresh)).toBe("");
  });

  test("a cursor report still reaches the pty when a kitty probe precedes it", async () => {
    const core = await createWtermCore(80, 24);
    const reply = answerQueries(carry(), core, encoder.encode("\x1b[?u\x1b[6n"));
    expect(reply.bytes).toBe("\x1b[1;1R");
    expect(reply.synth).toBe("");
    expect(reply.mutedKeyboardReports).toBe(1);
  });

  test("pushing kitty flags never makes the lane speak", async () => {
    const core = await createWtermCore(80, 24);
    const reply = answerQueries(carry(), core, encoder.encode("\x1b[>1u\x1b[?u\x1b[<u"));
    expect(reply.bytes).toBe("");
  });

  test("a primary DA sharing the chunk is still answered in probe order", async () => {
    const core = await createWtermCore(80, 24);
    const reply = answerQueries(carry(), core, encoder.encode("\x1b[?u\x1b[c\x1b[6n"));
    expect(reply.bytes).toBe(`${PRIMARY_DA_REPLY}\x1b[1;1R`);
  });
});
