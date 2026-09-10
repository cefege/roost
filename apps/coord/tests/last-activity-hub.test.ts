// Coordinator activity hub coverage for semantic worker metadata.
// The tests pin immediate first visibility and the local receipt-time throttle
// that bounds Sync fan-out regardless of worker source timestamps.

import { afterEach, expect, setSystemTime, test } from "bun:test";
import {
  LAST_ACTIVITY_THROTTLE_MS,
  observeTerminalActivity,
  startLastActivityHub,
} from "../src/last-activity-hub.ts";
import { lastActivityBus } from "../src/buses.ts";

let stopHub: (() => void) | undefined;

afterEach(() => {
  stopHub?.();
  stopHub = undefined;
  setSystemTime();
});

test("publishes first semantic activity immediately and throttles later updates", () => {
  setSystemTime(new Date(1_000));
  const sessionId = "activity-throttle";
  const received: number[] = [];
  const unsubscribe = lastActivityBus.subscribe((message) => {
    if (message.session_id === sessionId) received.push(message.ts_ms);
  });
  stopHub = startLastActivityHub();

  observeTerminalActivity(sessionId, 100);
  observeTerminalActivity(sessionId, 101);
  setSystemTime(new Date(1_000 + LAST_ACTIVITY_THROTTLE_MS));
  observeTerminalActivity(sessionId, 102);

  unsubscribe();
  expect(received).toEqual([100, 102]);
});
