import { afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import { collectWorkerDiagSnapshots } from "../src/connect/worker-send.ts";
import {
  _pendingRpcStats,
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
} from "../src/router/pending-rpcs.ts";

const RESPONDING_FP = "a".repeat(64);
const TIMED_OUT_FP = "b".repeat(64);
const OFFLINE_FP = "c".repeat(64);

function diagnosticRequestId(frame: CoordWorkerDown): string {
  expect(frame.frame.case).toBe("browserCommand");
  if (frame.frame.case !== "browserCommand") throw new Error("expected browser command");
  expect(JSON.parse(frame.frame.value.frameJson)).toEqual({ kind: "diag-snapshot" });
  return frame.frame.value.requestId;
}

describe("coordinator diagnostic worker fan-out", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    rejectPendingRpcsForWorker(RESPONDING_FP, "test cleanup");
    rejectPendingRpcsForWorker(TIMED_OUT_FP, "test cleanup");
    __setConnectWorkerForTest(RESPONDING_FP, null);
    __setConnectWorkerForTest(TIMED_OUT_FP, null);
    vi.useRealTimers();
  });

  test("returns one response and one explicit timeout without leaking a pending RPC", async () => {
    const pendingBefore = _pendingRpcStats().pending;
    const workerSnapshot = {
      build: { git_sha: "worker-build" },
      worker_fp: RESPONDING_FP,
      sessions: {},
    };
    let lateRequestId = "";
    const responding: WorkerHandle = {
      workerFp: RESPONDING_FP,
      send(frame) {
        const requestId = diagnosticRequestId(frame);
        expect(resolvePendingRpc(requestId, { spoofed: true }, TIMED_OUT_FP)).toBe(false);
        resolvePendingRpc(requestId, workerSnapshot, RESPONDING_FP);
        return 1;
      },
    };
    const timedOut: WorkerHandle = {
      workerFp: TIMED_OUT_FP,
      send(frame) {
        lateRequestId = diagnosticRequestId(frame);
        return 1;
      },
    };
    __setConnectWorkerForTest(RESPONDING_FP, responding);
    __setConnectWorkerForTest(TIMED_OUT_FP, timedOut);

    const pendingResult = collectWorkerDiagSnapshots(
      [TIMED_OUT_FP, RESPONDING_FP],
      20,
    );
    vi.advanceTimersByTime(20);
    const result = await pendingResult;

    expect(Object.keys(result)).toEqual([RESPONDING_FP, TIMED_OUT_FP]);
    expect(result[RESPONDING_FP]).toEqual({
      status: "ok",
      response_ms: expect.any(Number),
      snapshot: workerSnapshot,
    });
    expect(result[TIMED_OUT_FP]).toMatchObject({
      status: "error",
      error: {
        code: "timeout",
        message: expect.stringContaining("did not reply"),
      },
    });
    expect(_pendingRpcStats().pending).toBe(pendingBefore);

    // The timeout deleted its correlation entry. A late authenticated reply is
    // ignored and cannot settle a later request that happens to share state.
    expect(resolvePendingRpc(lateRequestId, { late: true }, TIMED_OUT_FP)).toBe(false);
  });

  test("reports a registered but disconnected worker as offline", async () => {
    const result = await collectWorkerDiagSnapshots([OFFLINE_FP], 20);
    expect(result[OFFLINE_FP]).toEqual({
      status: "error",
      response_ms: expect.any(Number),
      error: {
        code: "offline",
        message: "worker is not connected",
      },
    });
  });
});
