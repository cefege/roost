// Typed terminal-pipeline diagnostics route over the worker protobuf frame.
// These tests pin direct request framing, target-worker correlation, and the
// response validation boundary without using generic browser-command payloads.

import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  WTerminalPipelineSnapshotSchema,
  type CoordWorkerDown,
  type WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalPipelineReason,
  TerminalPipelineSessionSnapshotSchema,
  TerminalPipelineStage,
  TerminalPipelineStageSnapshotSchema,
} from "@roost/shared/proto/wire_pb";
import {
  TERMINAL_PIPELINE_DIAG_MAX_TARGETS,
  collectWorkerTerminalPipelineSnapshots,
  type TerminalPipelineDiagnosticTarget,
} from "../src/connect/worker-terminal-pipeline-snapshot.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import {
  _pendingRpcStats,
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
} from "../src/router/pending-rpcs.ts";

const TARGET_WORKER_FP = "a".repeat(64);
const SPOOFED_WORKER_FP = "b".repeat(64);

function pipelineReply(
  requestId: string,
  targets: readonly TerminalPipelineDiagnosticTarget[],
): WTerminalPipelineSnapshot {
  return create(WTerminalPipelineSnapshotSchema, {
    requestId,
    sessions: targets.map((target) => create(TerminalPipelineSessionSnapshotSchema, {
      sessionId: target.sessionId,
      viewId: target.viewId,
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

function pipelineRequest(frame: CoordWorkerDown) {
  expect(frame.frame.case).toBe("terminalPipelineSnapshot");
  if (frame.frame.case !== "terminalPipelineSnapshot") {
    throw new Error("expected terminal pipeline snapshot request");
  }
  return frame.frame.value;
}

afterEach(() => {
  rejectPendingRpcsForWorker(TARGET_WORKER_FP, "test cleanup");
  rejectPendingRpcsForWorker(SPOOFED_WORKER_FP, "test cleanup");
  __setConnectWorkerForTest(TARGET_WORKER_FP, null);
  __setConnectWorkerForTest(SPOOFED_WORKER_FP, null);
});

describe("typed worker terminal pipeline diagnostics", () => {
  test("sends a bounded direct request and accepts only the target worker reply", async () => {
    const pendingBefore = _pendingRpcStats().pending;
    const targets = Array.from({ length: TERMINAL_PIPELINE_DIAG_MAX_TARGETS + 1 }, (_, index) => ({
      sessionId: `session-${String(index).padStart(2, "0")}`,
      viewId: "",
    }));
    let receivedTargets: TerminalPipelineDiagnosticTarget[] = [];
    const worker: WorkerHandle = {
      workerFp: TARGET_WORKER_FP,
      revoked: false,
      ready: true,
      send(frame) {
        const request = pipelineRequest(frame);
        receivedTargets = request.targets.map((target) => ({
          sessionId: target.sessionId,
          viewId: target.viewId,
        }));
        expect(receivedTargets).toHaveLength(TERMINAL_PIPELINE_DIAG_MAX_TARGETS);
        expect(resolvePendingRpc(
          request.requestId,
          pipelineReply(request.requestId, receivedTargets),
          SPOOFED_WORKER_FP,
        )).toBe(false);
        expect(resolvePendingRpc(
          request.requestId,
          pipelineReply(request.requestId, receivedTargets),
          TARGET_WORKER_FP,
        )).toBe(true);
        return 1;
      },
    };
    __setConnectWorkerForTest(TARGET_WORKER_FP, worker);

    const result = await collectWorkerTerminalPipelineSnapshots(
      new Map([[TARGET_WORKER_FP, targets]]),
      100,
    );

    expect(receivedTargets.map((target) => target.sessionId)).toEqual(
      targets.slice(0, TERMINAL_PIPELINE_DIAG_MAX_TARGETS).map((target) => target.sessionId),
    );
    expect(result[TARGET_WORKER_FP]).toMatchObject({ status: "ok" });
    if (result[TARGET_WORKER_FP]?.status === "ok") {
      expect(result[TARGET_WORKER_FP].snapshot.sessions[0]).toMatchObject({
        sessionId: "session-00",
        viewId: "",
      });
    }
    expect(_pendingRpcStats().pending).toBe(pendingBefore);
  });

  test("ignores a mismatched reply before the matching response arrives", async () => {
    const worker: WorkerHandle = {
      workerFp: TARGET_WORKER_FP,
      revoked: false,
      ready: true,
      send(frame) {
        const request = pipelineRequest(frame);
        expect(resolvePendingRpc(
          "other-request",
          pipelineReply("other-request", [{ sessionId: "session", viewId: "" }]),
          TARGET_WORKER_FP,
        )).toBe(false);
        expect(resolvePendingRpc(
          request.requestId,
          pipelineReply(request.requestId, [{ sessionId: "session", viewId: "" }]),
          TARGET_WORKER_FP,
        )).toBe(true);
        return 1;
      },
    };
    __setConnectWorkerForTest(TARGET_WORKER_FP, worker);

    const result = await collectWorkerTerminalPipelineSnapshots(
      new Map([[TARGET_WORKER_FP, [{ sessionId: "session", viewId: "" }]]]),
      100,
    );

    expect(result[TARGET_WORKER_FP]).toMatchObject({ status: "ok" });
  });

  test("rejects an unaccounted partial response", async () => {
    const worker: WorkerHandle = {
      workerFp: TARGET_WORKER_FP,
      revoked: false,
      ready: true,
      send(frame) {
        const request = pipelineRequest(frame);
        const firstTarget = request.targets[0]!;
        expect(resolvePendingRpc(
          request.requestId,
          pipelineReply(request.requestId, [{
            sessionId: firstTarget.sessionId,
            viewId: firstTarget.viewId,
          }]),
          TARGET_WORKER_FP,
        )).toBe(true);
        return 1;
      },
    };
    __setConnectWorkerForTest(TARGET_WORKER_FP, worker);

    const result = await collectWorkerTerminalPipelineSnapshots(
      new Map([[TARGET_WORKER_FP, [
        { sessionId: "first-session", viewId: "" },
        { sessionId: "second-session", viewId: "" },
      ]]]),
      100,
    );

    expect(result[TARGET_WORKER_FP]).toMatchObject({
      status: "error",
      error: { code: "rpc_error" },
    });
  });

  test("does not send to an unready worker", async () => {
    const worker: WorkerHandle = {
      workerFp: TARGET_WORKER_FP,
      revoked: false,
      ready: false,
      send() {
        throw new Error("unready worker must not receive a pipeline request");
      },
    };
    __setConnectWorkerForTest(TARGET_WORKER_FP, worker);

    const result = await collectWorkerTerminalPipelineSnapshots(
      new Map([[TARGET_WORKER_FP, [{ sessionId: "session", viewId: "" }]]]),
      100,
    );

    expect(result[TARGET_WORKER_FP]).toMatchObject({
      status: "error",
      error: { code: "offline" },
    });
  });

  test("does not dispatch an empty worker target group", async () => {
    __setConnectWorkerForTest(TARGET_WORKER_FP, {
      workerFp: TARGET_WORKER_FP,
      revoked: false,
      ready: true,
      send() {
        throw new Error("empty group must not reach the worker");
      },
    });

    expect(await collectWorkerTerminalPipelineSnapshots(
      new Map([[TARGET_WORKER_FP, []]]),
      100,
    )).toEqual({});
  });
});
