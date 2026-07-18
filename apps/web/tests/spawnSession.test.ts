// Verifies spawn-retry resilience: after a coord restart the worker↔coord WS
// is down for ~10-15s while the worker re-dials, and coord rejects a spawn with
// FailedPrecondition "worker <fp> not connected". withSpawnRetry must retry
// across that window (so the click flow surfaces a brief delay, not a scary
// error toast) yet rethrow any OTHER error immediately. Guards the fix for the
// "New terminal failed: worker ... not connected" report.

import { test, expect } from "bun:test";
import { ConnectError, Code } from "@connectrpc/connect";
import { isWorkerReconnecting, withSpawnRetry } from "../src/lib/spawnSession.ts";

test("isWorkerReconnecting: only the transient precondition", () => {
  expect(isWorkerReconnecting(new ConnectError("worker abc123 not connected", Code.FailedPrecondition))).toBe(true);
  // wrong code
  expect(isWorkerReconnecting(new ConnectError("worker abc123 not connected", Code.Internal))).toBe(false);
  // right code, different precondition (NOT a reconnect — must not retry)
  expect(isWorkerReconnecting(new ConnectError("folder does not exist", Code.FailedPrecondition))).toBe(false);
  // plain error
  expect(isWorkerReconnecting(new Error("not connected"))).toBe(false);
});

test("withSpawnRetry: retries the reconnect window, then succeeds", async () => {
  let calls = 0;
  const result = await withSpawnRetry(async () => {
    calls++;
    if (calls < 3) throw new ConnectError("worker abc not connected", Code.FailedPrecondition);
    return { sessionId: "sess-1" };
  });
  expect(result.sessionId).toBe("sess-1");
  expect(calls).toBe(3); // failed twice, succeeded on the third
});

test("withSpawnRetry: rethrows a non-transient error immediately (no retry)", async () => {
  let calls = 0;
  const run = withSpawnRetry(async () => {
    calls++;
    throw new ConnectError("internal boom", Code.Internal);
  });
  await expect(run).rejects.toThrow("internal boom");
  expect(calls).toBe(1);
});
