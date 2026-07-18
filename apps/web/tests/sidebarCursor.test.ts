// Unit coverage for the flat-list keyboard cursor.
// Pure module-singleton state, no DOM. Mirrors src/lib/sidebarCursor.ts.

import { test, expect, beforeEach } from "bun:test";
import {
  setOrderedSessionIds,
  cursorSessionId,
  moveCursor,
} from "../src/lib/sidebarCursor.ts";

beforeEach(() => {
  // Reset: empty order clamps the cursor back to -1.
  setOrderedSessionIds([]);
});

test("moveCursor from -1 lands on first (down) and clamps at top", () => {
  setOrderedSessionIds(["a", "b", "c"]);
  expect(cursorSessionId()).toBe(null); // -1 = nothing highlighted
  moveCursor(1);
  expect(cursorSessionId()).toBe("a");
  moveCursor(1);
  expect(cursorSessionId()).toBe("b");
  moveCursor(-1);
  expect(cursorSessionId()).toBe("a");
  moveCursor(-1); // clamp, no wrap
  expect(cursorSessionId()).toBe("a");
});

test("moveCursor clamps at the bottom", () => {
  setOrderedSessionIds(["a", "b"]);
  moveCursor(1); // a
  moveCursor(1); // b
  moveCursor(1); // clamp at b
  expect(cursorSessionId()).toBe("b");
});

test("setOrderedSessionIds shrinking clamps the cursor into range", () => {
  setOrderedSessionIds(["a", "b", "c"]);
  moveCursor(1); moveCursor(1); moveCursor(1); // c (idx 2)
  expect(cursorSessionId()).toBe("c");
  setOrderedSessionIds(["a"]); // idx 2 now out of range
  expect(cursorSessionId()).toBe("a");
});
