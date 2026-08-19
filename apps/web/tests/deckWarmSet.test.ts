// Bounded warm-pane policy (lib/deckWarmSet.ts) — how many panes the deck keeps
// mounted, which one it drops first, and when the result is the same reference.

import { describe, test, expect } from "bun:test";
import { nextWarmSessionIds, DECK_WARM_LIMIT } from "../src/lib/deckWarmSet.ts";

// s1..sN in that order. Warm sets are recency-ordered, so a `previous` built
// from this reads oldest-first: s1 is always the next eviction candidate.
function ids(n: number, from = 1): string[] {
  return Array.from({ length: n }, (_, i) => `s${i + from}`);
}

describe("nextWarmSessionIds", () => {
  test("a slotted id is never evicted even when it is the oldest", () => {
    const all = ids(10);                       // s1 oldest, and it is on screen
    const next = nextWarmSessionIds(new Set(all), new Set(all), ["s1"]);
    expect(next.has("s1")).toBe(true);
    expect(next.has("s2")).toBe(false);        // eviction took the oldest non-slotted
    expect(next.size).toBe(1 + DECK_WARM_LIMIT);
  });

  test("a split layout keeps every slotted pane, the cap applies to the rest", () => {
    const all = ids(14);
    const slotted = ["s1", "s2", "s3"];
    const next = nextWarmSessionIds(new Set(all), new Set(all), slotted);
    for (const id of slotted) expect(next.has(id)).toBe(true);
    expect(next.size).toBe(slotted.length + DECK_WARM_LIMIT);
  });

  test("eviction takes the least-recently-slotted non-slotted id first", () => {
    const warm = ids(9);                       // s1..s9, none on screen
    const open = new Set([...warm, "s10"]);
    const next = nextWarmSessionIds(new Set(warm), open, ["s10"]);
    expect([...next]).toEqual([...ids(8, 2), "s10"]); // s1 gone, s10 newest
  });

  test("being slotted refreshes recency, so an older-inserted pane outlives a newer one", () => {
    const open = new Set(ids(11));
    let warm = nextWarmSessionIds(new Set(ids(9)), open, ["s2"]); // s2 shown → newest
    expect([...warm]).toEqual(["s1", ...ids(7, 3), "s2"]);
    warm = nextWarmSessionIds(warm, open, ["s10"]);               // over cap → drop s1
    expect(warm.has("s1")).toBe(false);
    warm = nextWarmSessionIds(warm, open, ["s11"]);               // over cap → drop s3
    expect(warm.has("s3")).toBe(false);
    expect(warm.has("s2")).toBe(true);         // inserted before s3, shown after it
  });

  test("a closed id disappears without counting against the limit", () => {
    const warm = ids(9);                       // s1..s9
    const open = new Set([...ids(6, 4), "s10"]); // s1..s3 closed
    const next = nextWarmSessionIds(new Set(warm), open, ["s10"]);
    expect([...next]).toEqual([...ids(6, 4), "s10"]);
    expect(next.size).toBe(7);                 // 3 closures evicted nobody else
  });

  test("a slot naming a session that is not open is not warmed", () => {
    const next = nextWarmSessionIds(new Set(), new Set(["s1"]), ["ghost", "s1"]);
    expect([...next]).toEqual(["s1"]);
  });

  test("membership never exceeds slotted.size + DECK_WARM_LIMIT", () => {
    const open = new Set(ids(40));
    let warm: ReadonlySet<string> = new Set();
    for (const id of ids(40)) {
      warm = nextWarmSessionIds(warm, open, [id]);
      expect(warm.size).toBeLessThanOrEqual(1 + DECK_WARM_LIMIT);
      expect(warm.has(id)).toBe(true);
    }
  });

  test("an explicit limit caps the non-slotted panes", () => {
    const all = ids(5);
    const next = nextWarmSessionIds(new Set(all), new Set(all), ["s5"], 0);
    expect([...next]).toEqual(["s5"]);         // limit 0 → only what is on screen
  });

  test("the same inputs return the identical Set reference", () => {
    const open = new Set(ids(4));
    const first = nextWarmSessionIds(new Set(ids(3)), open, ["s4"]);
    const second = nextWarmSessionIds(first, open, ["s4"]);
    expect(second).toBe(first);
    expect(nextWarmSessionIds(second, open, ["s4"])).toBe(first);
  });

  test("a pure reorder returns a new Set", () => {
    const previous = new Set(["a", "b"]);
    const next = nextWarmSessionIds(previous, new Set(["a", "b"]), ["a"]);
    expect(next).not.toBe(previous);           // recency moved: a is now newest
    expect([...next]).toEqual(["b", "a"]);
    expect(next.size).toBe(previous.size);     // membership identical
  });
});
