// agentEntries.test.ts — the upsert-by-seq contract behind the agent
// transcript. The worker RE-EMITS an entry under the same `seq` as it grows,
// and backfill pages land UNDER the live tail, so the store must: replace on a
// matching seq, append a higher seq, splice a lower one, and survive a page
// that OVERLAPS the tail (the case where a stale seq→index map silently
// destroys an entry and duplicates another).
//
// The store is module-global with no reset hook, so each test uses its own
// session id. See src/store/agentEntries.ts.

import { expect, test, describe } from "bun:test";
import type { AgentEntry } from "@roost/shared/wire/agent-entry";
import { agentEntries, pruneAgentEntries, upsertEntries } from "../src/store/agentEntries.ts";

function text(seq: number, body: string, done = false): AgentEntry {
  return { kind: "assistant", seq, ts: 1_700_000_000_000 + seq, text: body, done };
}

function seqs(sessionId: string): number[] {
  return agentEntries(sessionId).map((e) => e.seq);
}

function bodies(sessionId: string): string[] {
  return agentEntries(sessionId).map((e) => ("text" in e ? e.text : ""));
}

describe("upsertEntries", () => {
  test("appends a monotonic live stream in seq order", () => {
    const sid = "live";
    upsertEntries(sid, [text(1, "a"), text(2, "b")]);
    upsertEntries(sid, [text(3, "c")]);
    expect(seqs(sid)).toEqual([1, 2, 3]);
    expect(bodies(sid)).toEqual(["a", "b", "c"]);
  });

  test("replaces in place when the same seq is re-emitted fuller", () => {
    const sid = "stream";
    upsertEntries(sid, [text(1, "hel")]);
    upsertEntries(sid, [text(1, "hello")]);
    upsertEntries(sid, [text(1, "hello world", true)]);
    expect(seqs(sid)).toEqual([1]);
    expect(bodies(sid)).toEqual(["hello world"]);
    expect(agentEntries(sid)[0]).toMatchObject({ done: true });
  });

  test("splices a backfill page under the live tail", () => {
    const sid = "backfill";
    upsertEntries(sid, [text(8, "h"), text(9, "i")]);
    upsertEntries(sid, [text(5, "e"), text(6, "f"), text(7, "g")]);
    expect(seqs(sid)).toEqual([5, 6, 7, 8, 9]);
    expect(bodies(sid)).toEqual(["e", "f", "g", "h", "i"]);
  });

  test("an overlapping backfill page neither duplicates nor destroys", () => {
    // The regression: after the first splice every index past it shifts, so a
    // seq→index map consulted later in the SAME batch points at the wrong slot.
    const sid = "overlap";
    upsertEntries(sid, [text(7, "old7"), text(8, "old8"), text(9, "old9")]);
    upsertEntries(sid, [text(5, "p5"), text(6, "p6"), text(7, "p7"), text(8, "p8")]);
    expect(seqs(sid)).toEqual([5, 6, 7, 8, 9]);
    expect(bodies(sid)).toEqual(["p5", "p6", "p7", "p8", "old9"]);
  });

  test("the seq index is repaired after a splice, so later upserts still land", () => {
    const sid = "reindex";
    upsertEntries(sid, [text(3, "c")]);
    upsertEntries(sid, [text(1, "a"), text(2, "b")]);
    // Post-splice: a replace must hit the shifted slot, an append the tail.
    upsertEntries(sid, [text(2, "B")]);
    upsertEntries(sid, [text(4, "d")]);
    expect(seqs(sid)).toEqual([1, 2, 3, 4]);
    expect(bodies(sid)).toEqual(["a", "B", "c", "d"]);
  });

  test("out-of-order entries within one batch still sort", () => {
    const sid = "unsorted";
    upsertEntries(sid, [text(3, "c"), text(1, "a"), text(2, "b")]);
    expect(seqs(sid)).toEqual([1, 2, 3]);
  });

  test("sessions are independent", () => {
    upsertEntries("one", [text(1, "x")]);
    upsertEntries("two", [text(1, "y"), text(2, "z")]);
    expect(bodies("one")).toEqual(["x"]);
    expect(bodies("two")).toEqual(["y", "z"]);
  });

  test("an empty batch is a no-op and never creates a session key", () => {
    upsertEntries("empty", []);
    expect(agentEntries("empty")).toEqual([]);
  });
});

describe("pruneAgentEntries", () => {
  test("forgets a closed session's transcript and leaves others alone", () => {
    upsertEntries("closing", [text(1, "a"), text(2, "b")]);
    upsertEntries("staying", [text(1, "keep")]);
    pruneAgentEntries("closing");
    expect(agentEntries("closing")).toEqual([]);
    expect(bodies("staying")).toEqual(["keep"]);
  });

  test("a pruned session can be repopulated from seq 1 with a clean index", () => {
    // The seq→index map lives OUTSIDE the store; if prune left it behind, this
    // upsert would write through a stale index into a now-empty array.
    upsertEntries("reused", [text(1, "old"), text(2, "older")]);
    pruneAgentEntries("reused");
    upsertEntries("reused", [text(1, "new")]);
    expect(seqs("reused")).toEqual([1]);
    expect(bodies("reused")).toEqual(["new"]);
  });

  test("pruning an unknown session is a no-op", () => {
    pruneAgentEntries("never-seen");
    expect(agentEntries("never-seen")).toEqual([]);
  });
});
