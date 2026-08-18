import { expect, test } from "bun:test";
import {
  initHistory, pushHistory, goBack, goForward, canGoBack, canGoForward,
} from "../src/lib/browseHistory.ts";

test("initHistory seeds cursor 0", () => {
  const s = initHistory("~");
  expect(s.entries).toEqual(["~"]);
  expect(s.cursor).toBe(0);
});

test("pushHistory appends and advances", () => {
  let s = initHistory("~");
  s = pushHistory(s, "/Users/dev/Code");
  expect(s.entries).toEqual(["~", "/Users/dev/Code"]);
  expect(s.cursor).toBe(1);
});

test("pushHistory truncates forward branch after a back", () => {
  let s = initHistory("~");
  s = pushHistory(s, "A");
  s = pushHistory(s, "B"); // ~, A, B  cursor=2
  s = goBack(s);           // ~, A     cursor=1
  s = pushHistory(s, "C"); // ~, A, C  cursor=2  (B discarded)
  expect(s.entries).toEqual(["~", "A", "C"]);
  expect(s.cursor).toBe(2);
});

test("pushHistory dedups adjacent equal (no-op, same ref)", () => {
  let s = initHistory("~");
  s = pushHistory(s, "A");
  const s2 = pushHistory(s, "A"); // same dir, no-op
  expect(s2).toBe(s); // same reference — no change
  expect(s2.entries).toEqual(["~", "A"]);
  expect(s2.cursor).toBe(1);
});

test("goBack moves cursor, no-op + same-ref at bounds", () => {
  let s = initHistory("~");
  s = pushHistory(s, "A");
  s = pushHistory(s, "B"); // cursor=2
  s = goBack(s);
  expect(s.cursor).toBe(1);
  expect(s.entries[1]).toBe("A");
  s = goBack(s);
  expect(s.cursor).toBe(0);
  const sBounded = goBack(s);
  expect(sBounded).toBe(s); // same ref — no-op at bound
});

test("goForward moves cursor, no-op + same-ref at bounds", () => {
  let s = initHistory("~");
  s = pushHistory(s, "A");
  s = pushHistory(s, "B"); // cursor=2
  s = goBack(s);           // cursor=1
  s = goBack(s);           // cursor=0
  s = goForward(s);        // cursor=1
  expect(s.cursor).toBe(1);
  s = goForward(s);        // cursor=2
  expect(s.cursor).toBe(2);
  const sBounded = goForward(s); // at end
  expect(sBounded).toBe(s); // same ref — no-op at bound
});

test("canGoBack / canGoForward bounds", () => {
  let s = initHistory("~");
  expect(canGoBack(s)).toBe(false);
  expect(canGoForward(s)).toBe(false);
  s = pushHistory(s, "A");
  pushHistory(s, "B");
  // s still points at state after init, not after pushes...
  // re-derive:
  let t = initHistory("~");
  t = pushHistory(t, "A");
  t = pushHistory(t, "B");
  expect(canGoBack(t)).toBe(true);
  expect(canGoForward(t)).toBe(false);
  t = goBack(t);
  expect(canGoBack(t)).toBe(true);
  expect(canGoForward(t)).toBe(true);
  t = goBack(t);
  expect(canGoBack(t)).toBe(false);
  expect(canGoForward(t)).toBe(true);
});

test("regression: empty-start home never becomes root", () => {
  let s = initHistory("~");
  s = pushHistory(s, "/Users/dev/Code");
  s = pushHistory(s, "/Users/dev/Docs");
  // back twice to home
  s = goBack(s);
  expect(s.entries[s.cursor]).toBe("/Users/dev/Code");
  s = goBack(s);
  expect(s.entries[s.cursor]).toBe("~"); // home, NOT "/" or ""
  // forward through all
  s = goForward(s);
  expect(s.entries[s.cursor]).toBe("/Users/dev/Code");
  s = goForward(s);
  expect(s.entries[s.cursor]).toBe("/Users/dev/Docs");
  // back one, then push new — forward branch discarded
  s = goBack(s); // /Users/dev/Code
  s = pushHistory(s, "/Users/dev/Code/Sub");
  expect(s.entries).toEqual(["~", "/Users/dev/Code", "/Users/dev/Code/Sub"]);
  expect(s.cursor).toBe(2);
  expect(canGoForward(s)).toBe(false);
});