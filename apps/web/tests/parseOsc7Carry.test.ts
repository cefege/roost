// OSC 7 carry-buffer semantics in sync-dispatch's byte fan-out. The perf-sweep
// fix: the carry retains ONLY an unterminated OSC7 candidate (or a trailing
// partial introducer) — the old code kept the FULL tail whenever nothing
// matched, so after the first ESC-containing chunk every later chunk paid
// decode+concat+regex forever, per session. Contracts locked here:
//   1. ESC-dense chunk with no OSC7 → carry EMPTY (no-ESC fast path restored)
//   2. OSC7 split across two chunks → cwd still updates, then carry clears
//   3. chunk ending mid-introducer → completed by the next chunk
//   4. terminated-but-unmatched candidate (empty path) → dropped, not carried

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { _dispatchBytes, _oscBuffer, pruneOscBuffer } from "../src/store/sync-dispatch.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";

const SID = asSessionId("00000000-0000-4000-8000-00000000d001");
const FP = asWorkerFp("d".repeat(64));

// latin1 text → bytes, matching the decoder _parseOsc7 uses.
function bytes(s: string): Uint8Array {
  return Uint8Array.from(s, (c) => c.charCodeAt(0) & 0xff);
}

describe("_parseOsc7 carry", () => {
  beforeEach(() => {
    pruneOscBuffer(SID);
    setRootStore("sessions", SID, {
      id: SID, worker_fp: FP, channel: asChannelId(1), kind: "shell",
      cwd: "/old", spawn_cwd: "/old", workspace_id: null, status: "open",
      created_at: 1000, closed_at: null, custom_title: null,
    } as Session);
  });

  test("ESC-dense chunk without any OSC7 leaves NO carry (fast path restored)", () => {
    _dispatchBytes(SID, bytes("\x1b[31mred\x1b[0m plain \x1b[1mbold\x1b[0m"));
    expect(_oscBuffer.get(SID)).toBeUndefined(); // old code carried the whole tail
    expect(rootStore.sessions[SID]?.cwd).toBe("/old");
  });

  test("OSC7 split across two chunks still updates cwd, then carry clears", () => {
    _dispatchBytes(SID, bytes("prompt$ \x1b]7;file://mac/Users/yo"));
    expect(_oscBuffer.get(SID)).toBe("\x1b]7;file://mac/Users/yo"); // unterminated candidate retained
    expect(rootStore.sessions[SID]?.cwd).toBe("/old"); // not applied yet

    _dispatchBytes(SID, bytes("u\x07")); // terminator arrives (chunk itself has no ESC)
    expect(rootStore.sessions[SID]?.cwd).toBe("/Users/you");
    expect(_oscBuffer.get(SID)).toBeUndefined(); // consumed — nothing carried
  });

  test("chunk ending mid-introducer is completed by the next chunk", () => {
    _dispatchBytes(SID, bytes("some output\x1b]"));
    expect(_oscBuffer.get(SID)).toBe("\x1b]"); // partial introducer, not the whole tail

    _dispatchBytes(SID, bytes("7;file://mac/x%20y\x07"));
    expect(rootStore.sessions[SID]?.cwd).toBe("/x y"); // percent-decoded
    expect(_oscBuffer.get(SID)).toBeUndefined();
  });

  test("complete OSC7 mid-chunk applies and carries nothing", () => {
    _dispatchBytes(SID, bytes("a\x1b]7;file://mac/tmp\x07 trailing shell noise"));
    expect(rootStore.sessions[SID]?.cwd).toBe("/tmp");
    expect(_oscBuffer.get(SID)).toBeUndefined();
  });

  test("terminated-but-unmatched candidate (no path at all) is dropped", () => {
    // NB: with a non-empty host the regex backtracks host chars into the path
    // group ("file://mac" parses as host "ma", path "c") — longstanding parser
    // behavior, not under test here. An empty authority+path is the shape
    // that terminates without matching.
    _dispatchBytes(SID, bytes("x\x1b]7;file://\x07y"));
    expect(rootStore.sessions[SID]?.cwd).toBe("/old"); // nothing to apply
    expect(_oscBuffer.get(SID)).toBeUndefined(); // and the dead candidate is not carried
  });
});
