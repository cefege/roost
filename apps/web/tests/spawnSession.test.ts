// spawnSession.test.ts — three concerns in the spawn helper module.
//
// 1. Spawn-retry resilience: after a coord restart the worker↔coord WS is down
//    for ~10-15s while the worker re-dials, and coord rejects a spawn with
//    FailedPrecondition "worker <fp> not connected". withSpawnRetry must retry
//    across that window (so the click flow surfaces a brief delay, not a scary
//    error toast) yet rethrow any OTHER error immediately.
// 2. The per-session auto-launch guard: maybeAutoLaunchAgent is reachable from
//    several racy new-tab paths; without a guard a second call re-queues the
//    agent command and it gets typed twice into the same PTY. Sync input
//    admission must be attempted exactly once for two calls with one session id.
// 3. The initial PTY-size hint: buildSpawnShellRequest's cols/rows are applied
//    by the keeper at PTY start, so they must be bounded to the core limits and
//    ABSENT whenever the browser has nothing real to measure.
//
// mock.module must run before the unit's static imports resolve, so the deps
// are mocked here and the unit is pulled in via a dynamic import below.

import { test, expect, describe, mock } from "bun:test";
import { ConnectError, Code } from "@connectrpc/connect";
import { asWorkerFp } from "@roost/protocol/wire";
import { TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS } from "@roost/protocol/viewport";
import {
  installFakeTerminalDom,
  mountFakeDeck,
  mountFakeSlot,
} from "./helpers/fakeTerminalDom.ts";

let enabled = true;
const sendTerminalInput = mock((_sid: string, bytes: Uint8Array) => ({
  accepted: true as const,
  inputSeq: 1n,
  result: Promise.resolve({
    status: "accepted" as const,
    inputSeq: 1n,
    writtenBytes: bytes.byteLength,
  }),
}));

mock.module("../src/lib/agents.ts", () => ({
  autoLaunchEnabled: () => enabled,
  resolveAgent: () => ({ command: "omp" }),
}));
mock.module("../src/ws/sync-outbound.ts", () => ({
  sendTerminalInput,
}));
// NOT mocked: ../src/lib/wtermSizeEstimate.ts is another suite's unit under
// test and bun module mocks are process-global. The hint cases below install a
// fake DOM and let the real estimate run.

// Dynamic import: static imports hoist above the mock.module calls above, so the
// unit must be pulled in after the deps are mocked (module-loading boundary).
const {
  isWorkerReconnecting,
  withSpawnRetry,
  maybeAutoLaunchAgent,
  buildSpawnShellRequest,
} = await import("../src/lib/spawnSession.ts");

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

describe("initial PTY-size hint", () => {
  const workerFp = asWorkerFp("aa".repeat(32));
  const sessionId = "00000000-0000-4000-8000-000000000123";

  test("a mounted pane's measured geometry is the hint", () => {
    const dom = installFakeTerminalDom();
    mountFakeSlot(mountFakeDeck(dom.document), {
      focused: true,
      visible: true,
      slotRect: { width: 900, height: 500 },
      displayWidth: 832,
      displayHeight: 424,
    });
    try {
      expect(buildSpawnShellRequest(workerFp, "/work", sessionId))
        .toMatchObject({ cols: 80, rows: 20 });
    } finally {
      dom.restore();
    }
  });

  test("nothing measurable omits cols/rows entirely", () => {
    // A fabricated hint (the 1×1 a clamped mid-layout box used to produce) is
    // applied by the keeper at PTY start, so absence is the only safe answer.
    const dom = installFakeTerminalDom();
    mountFakeDeck(dom.document);
    try {
      const request = buildSpawnShellRequest(workerFp, "/work", sessionId);
      expect(request.cols).toBeUndefined();
      expect(request.rows).toBeUndefined();
      expect(request).toEqual({
        workerFp,
        kind: "shell",
        folder: "/work",
        cols: undefined,
        rows: undefined,
        sessionId,
      });
    } finally {
      dom.restore();
    }
  });

  test("an out-of-range viewport is bounded to the core limits", () => {
    expect(buildSpawnShellRequest(workerFp, "/work", sessionId, {
      cols: 9_000,
      rows: 9_000,
    })).toMatchObject({ cols: TERMINAL_MAX_COLS, rows: TERMINAL_MAX_ROWS });
  });
});

describe("maybeAutoLaunchAgent", () => {
  test("fires at most once per session id", () => {
    maybeAutoLaunchAgent("s1");
    maybeAutoLaunchAgent("s1");
    expect(sendTerminalInput).toHaveBeenCalledTimes(1);
    const [sid, bytes] = sendTerminalInput.mock.calls[0];
    expect(sid).toBe("s1");
    expect(new TextDecoder().decode(bytes)).toBe("omp\r");
  });

  test("a distinct session id is not blocked", () => {
    maybeAutoLaunchAgent("s2");
    expect(sendTerminalInput).toHaveBeenCalledTimes(2);
    expect(sendTerminalInput.mock.calls[1][0]).toBe("s2");
  });

  test("disabled config sends nothing", () => {
    enabled = false;
    maybeAutoLaunchAgent("s3");
    expect(sendTerminalInput).toHaveBeenCalledTimes(2);
    enabled = true;
  });
});
