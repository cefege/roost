// Verifies the terminal snapshot source defers frame production until cursor admission.
// The hub memoizes this source per immutable cache version while the source memoizes its plan.
// A producer counter distinguishes source replacement from actual snapshot work.
import { expect, test } from "bun:test";
import { terminalSnapshotSource } from "../src/connect/terminal-screen-frames.ts";
import { fullFrame } from "./terminal-screen-hub-harness.ts";

test("defers snapshot frame production until a cursor is created", () => {
  let produced = 0;
  const source = terminalSnapshotSource(() => {
    produced++;
    return fullFrame();
  });
  expect(produced).toBe(0);

  const firstCursor = source.createCursor();
  try {
    expect(produced).toBe(1);
    expect(firstCursor.partCount).toBe(1);
  } finally {
    firstCursor.release();
  }

  const secondCursor = source.createCursor();
  try {
    expect(produced).toBe(1);
    expect(secondCursor.partCount).toBe(1);
  } finally {
    secondCursor.release();
  }
});
