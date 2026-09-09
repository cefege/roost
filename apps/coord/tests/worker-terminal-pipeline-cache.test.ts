// Verifies handler-owned pipeline sampling batches target scopes without leaking them.
// Each worker may sample once per cache window, while uncached targets wait for
// their own bounded batch instead of receiving another target's empty evidence.

import { afterEach, expect, test, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  WTerminalPipelineSnapshotSchema,
  type CoordWorkerDown,
  type DTerminalPipelineSnapshotRequest,
  type WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalPipelineReason,
  TerminalPipelineSessionSnapshotSchema,
  TerminalPipelineStage,
  TerminalPipelineStageSnapshotSchema,
} from "@roost/shared/proto/wire_pb";
import {
  WORKER_TERMINAL_PIPELINE_CACHE_MS,
  createWorkerTerminalPipelineSnapshotCache,
} from "../src/connect/worker-terminal-pipeline-cache.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import { rejectPendingRpcsForWorker, resolvePendingRpc } from "../src/router/pending-rpcs.ts";

const WORKER_FP = "a".repeat(64);
const TARGET_A = { sessionId: "session-a", viewId: "" };
const TARGET_B = { sessionId: "session-b", viewId: "" };

function pipelineRequest(frame: CoordWorkerDown): DTerminalPipelineSnapshotRequest {
  expect(frame.frame.case).toBe("terminalPipelineSnapshot");
  if (frame.frame.case !== "terminalPipelineSnapshot") throw new Error("expected pipeline request");
  return frame.frame.value;
}

function pipelineReply(
  requestId: string,
  targets: readonly { sessionId: string; viewId: string }[],
): WTerminalPipelineSnapshot {
  return create(WTerminalPipelineSnapshotSchema, {
    requestId,
    sessions: targets.map((target) => create(TerminalPipelineSessionSnapshotSchema, {
      ...target,
      stages: [create(TerminalPipelineStageSnapshotSchema, {
        stage: TerminalPipelineStage.WORKER_STREAM,
        reason: TerminalPipelineReason.NONE,
        generation: 7n,
        streamId: "stream-7",
        sequence: 11n,
      })],
    })),
  });
}

async function flushCacheWork(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

afterEach(() => {
  vi.useRealTimers();
  rejectPendingRpcsForWorker(WORKER_FP, "test cleanup");
  __setConnectWorkerForTest(WORKER_FP, null);
});

test("batches concurrent target scopes and reuses only their own cached records", async () => {
  const cache = createWorkerTerminalPipelineSnapshotCache();
  const requests: DTerminalPipelineSnapshotRequest[] = [];
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    ready: true,
    revoked: false,
    send(frame) {
      requests.push(pipelineRequest(frame));
      return 1;
    },
  };
  __setConnectWorkerForTest(WORKER_FP, worker);
  try {
    const first = cache.collect(new Map([[WORKER_FP, [TARGET_A]]]), 1_000);
    const second = cache.collect(new Map([[WORKER_FP, [TARGET_B]]]), 1_000);
    await flushCacheWork();

    expect(requests).toHaveLength(1);
    expect(requests[0]?.targets.map(({ sessionId, viewId }) => ({ sessionId, viewId }))).toEqual([
      TARGET_A,
      TARGET_B,
    ]);
    const request = requests[0]!;
    expect(resolvePendingRpc(
      request.requestId,
      pipelineReply(request.requestId, request.targets),
      WORKER_FP,
    )).toBe(true);

    const [firstResult, secondResult] = await Promise.all([first, second]);
    expect(firstResult[WORKER_FP]).toMatchObject({
      status: "ok",
      snapshot: { sessions: [{ sessionId: TARGET_A.sessionId }] },
    });
    expect(secondResult[WORKER_FP]).toMatchObject({
      status: "ok",
      snapshot: { sessions: [{ sessionId: TARGET_B.sessionId }] },
    });

    const cached = await cache.collect(new Map([[WORKER_FP, [TARGET_A]]]), 1_000);
    expect(cached[WORKER_FP]).toMatchObject({ status: "ok" });
    expect(requests).toHaveLength(1);
  } finally {
    cache.dispose();
  }
});

test("waits for an uncached target batch instead of projecting another target's sample", async () => {
  vi.useFakeTimers();
  const cache = createWorkerTerminalPipelineSnapshotCache();
  const requests: DTerminalPipelineSnapshotRequest[] = [];
  __setConnectWorkerForTest(WORKER_FP, {
    workerFp: WORKER_FP,
    ready: true,
    revoked: false,
    send(frame) {
      requests.push(pipelineRequest(frame));
      return 1;
    },
  });
  try {
    const first = cache.collect(new Map([[WORKER_FP, [TARGET_A]]]), 1_000);
    await flushCacheWork();
    const firstRequest = requests[0]!;
    expect(resolvePendingRpc(
      firstRequest.requestId,
      pipelineReply(firstRequest.requestId, firstRequest.targets),
      WORKER_FP,
    )).toBe(true);
    await first;

    const second = cache.collect(new Map([[WORKER_FP, [TARGET_B]]]), 1_000);
    await flushCacheWork();
    expect(requests).toHaveLength(1);

    vi.advanceTimersByTime(WORKER_TERMINAL_PIPELINE_CACHE_MS);
    vi.advanceTimersByTime(0);
    await flushCacheWork();
    const secondRequest = requests[1]!;
    expect(secondRequest.targets.map(({ sessionId, viewId }) => ({ sessionId, viewId }))).toEqual([
      TARGET_B,
    ]);
    expect(resolvePendingRpc(
      secondRequest.requestId,
      pipelineReply(secondRequest.requestId, secondRequest.targets),
      WORKER_FP,
    )).toBe(true);

    const result = await second;
    expect(result[WORKER_FP]).toMatchObject({
      status: "ok",
      snapshot: { sessions: [{ sessionId: TARGET_B.sessionId }], droppedRecords: 0 },
    });
  } finally {
    cache.dispose();
  }
});
