// Exercises source-cursor release paths that are reached before a snapshot can
// complete. The production terminal scheduler owns these cursors, so replacing
// or dropping a lane must release each source exactly once.

import { expect, test } from "bun:test";
import { makeCell, makeHarness } from "./sync-ws-v2-scheduler-harness.ts";

function trackedSnapshot(releases: { count: number }, sequence: number) {
  return {
    createCursor() {
      let released = false;
      return {
        partCount: 1,
        materialize(partIndex: number) {
          if (partIndex !== 0) throw new Error("missing tracked snapshot part");
          return makeCell("44444444-4444-4444-8444-444444444444", sequence, true);
        },
        release() {
          if (released) return;
          released = true;
          releases.count++;
        },
      };
    },
  };
}

test("releases lazy sources on unsent replacement and terminal-session drop", () => {
  const sessionId = "44444444-4444-4444-8444-444444444444";
  const streamId = "99999999-9999-4999-8999-999999999999";
  const harness = makeHarness("scheduler-test:source-replacement", false);
  const firstReleases = { count: 0 };
  const secondReleases = { count: 0 };

  harness.scheduler.beginTerminalStream(harness.ws, sessionId, streamId);
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    sessionId,
    streamId,
    trackedSnapshot(firstReleases, 1),
  )).toBe(true);
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    sessionId,
    streamId,
    trackedSnapshot(secondReleases, 2),
  )).toBe(true);
  expect(firstReleases.count).toBe(1);

  harness.scheduler.dropTerminalSession(harness.ws, sessionId);
  expect(firstReleases.count).toBe(1);
  expect(secondReleases.count).toBe(1);
});
