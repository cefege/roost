// Worker-frame admission for typed terminal-pipeline diagnostic replies.
// The dispatcher must use the authenticated connection identity, readiness, and
// correlation before it can settle the coordinator's worker-keyed pending RPC.

import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WTerminalPipelineSnapshotSchema,
  type WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import { makeWorkerFrameDispatcher } from "../src/connect/worker-frame-dispatch.ts";
import type { WorkerServiceDeps } from "../src/connect/worker-conn-types.ts";
import {
  _pendingRpcStats,
  createPendingRpc,
  rejectPendingRpcsForWorker,
} from "../src/router/pending-rpcs.ts";

const TARGET_WORKER_FP = "a".repeat(64);
const SPOOFED_WORKER_FP = "b".repeat(64);

function pipelineReply(requestId: string): WTerminalPipelineSnapshot {
  return create(WTerminalPipelineSnapshotSchema, { requestId });
}

function pipelineFrame(requestId: string) {
  return create(CoordWorkerUpSchema, {
    frame: { case: "terminalPipelineSnapshot", value: pipelineReply(requestId) },
  });
}

function workerFrameDispatcher(workerFp: string, isSnapshotReady: () => boolean) {
  return makeWorkerFrameDispatcher({
    deps: {} as WorkerServiceDeps,
    callerFingerprint: workerFp,
    requestClose() {},
    getWorkerFp: () => workerFp,
    isSnapshotReady,
    isCurrentGeneration: () => true,
    fenced: () => false,
    sendBestEffort: () => true,
    markSnapshotReady: () => false,
    scheduleRespawn() {},
  });
}

afterEach(() => {
  rejectPendingRpcsForWorker(TARGET_WORKER_FP, "test cleanup");
  rejectPendingRpcsForWorker(SPOOFED_WORKER_FP, "test cleanup");
});

describe("typed terminal pipeline reply dispatch", () => {
  test("requires a ready authenticated target and exact request correlation", async () => {
    const pendingBefore = _pendingRpcStats().pending;
    const pending = createPendingRpc<WTerminalPipelineSnapshot>(1_000, TARGET_WORKER_FP);
    let snapshotReady = false;
    const targetDispatcher = workerFrameDispatcher(TARGET_WORKER_FP, () => snapshotReady);

    expect(targetDispatcher.handleLiveFrame(pipelineFrame(pending.request_id))).toBe(true);
    expect(_pendingRpcStats().pending).toBe(pendingBefore + 1);

    snapshotReady = true;
    expect(targetDispatcher.handleLiveFrame(pipelineFrame("other-request"))).toBe(true);
    expect(_pendingRpcStats().pending).toBe(pendingBefore + 1);

    const spoofedDispatcher = workerFrameDispatcher(SPOOFED_WORKER_FP, () => true);
    expect(spoofedDispatcher.handleLiveFrame(pipelineFrame(pending.request_id))).toBe(true);
    expect(_pendingRpcStats().pending).toBe(pendingBefore + 1);

    expect(targetDispatcher.handleLiveFrame(pipelineFrame(pending.request_id))).toBe(true);
    await expect(pending.promise).resolves.toMatchObject({ requestId: pending.request_id });
    expect(_pendingRpcStats().pending).toBe(pendingBefore);
  });
});
