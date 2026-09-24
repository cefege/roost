// Exercises raw worker-WS result dispatch while a durable frame is held in its
// ordered queue. Input and stream results may settle their exact pending RPC;
// every other frame remains on the ordered connection lane.

import { afterEach, describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  TerminalInputStatus,
  TerminalStreamStatus,
  TerminalWritePhase,
  WInputResultSchema,
  WPongSchema,
  WSessionEventSchema,
  WTerminalPipelineSnapshotSchema,
  WTerminalStreamResultSchema,
  type CoordWorkerUp,
  type WInputResult,
  type WTerminalStreamResult,
} from "@roost/protocol/proto/worker_transport_pb";
import { asSessionId } from "@roost/protocol/wire";
import { eventToProto } from "@roost/protocol/wire/event-proto";
import { makeWorkerFrameDispatcher } from "../../src/workers/worker-frame-dispatch.ts";
import {
  createAnnouncedChannelBarrier,
  makeWorkerWsHandler,
  type WorkerWsData,
} from "../../src/workers/worker-ws-handler.ts";
import type { WorkerConn, WorkerServiceDeps } from "../../src/workers/worker-service.ts";
import {
  _pendingRpcStats,
  createPendingRpc,
  rejectPendingRpcsForWorker,
} from "../../src/router/pending-rpcs.ts";

const TARGET_WORKER_FP = "a1".repeat(32);
const FOREIGN_WORKER_FP = "b2".repeat(32);
const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");

interface SocketHarness {
  data: WorkerWsData;
  received: string[];
  durableComplete(): boolean;
  deliver(frame: CoordWorkerUp): void;
  releaseDurable(): void;
  setCurrent(current: boolean): void;
  setReady(ready: boolean): void;
  close(): void;
}

function durableEventFrame(): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "event",
      value: create(WSessionEventSchema, {
        event: eventToProto({
          kind: "closed",
          session_id: SESSION_ID,
          exit_code: 0,
          ts: Date.now(),
        }, 0)!,
        clientSeq: 1n,
      }),
    },
  });
}

function inputResultFrame(requestId: string): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "inputResult",
      value: create(WInputResultSchema, {
        requestId,
        sessionId: SESSION_ID,
        inputSeq: 1n,
        status: TerminalInputStatus.ACCEPTED,
        phase: TerminalWritePhase.WRITTEN,
        writtenBytes: 1,
      }),
    },
  });
}

function streamResultFrame(requestId: string): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "terminalStreamResult",
      value: create(WTerminalStreamResultSchema, {
        requestId,
        sessionId: SESSION_ID,
        streamId: "test-stream",
        enabled: true,
        status: TerminalStreamStatus.COMMITTED,
        effectiveCols: 80,
        effectiveRows: 24,
        phase: TerminalWritePhase.WRITTEN,
      }),
    },
  });
}

function pipelineResultFrame(requestId: string): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "terminalPipelineSnapshot",
      value: create(WTerminalPipelineSnapshotSchema, { requestId }),
    },
  });
}

function pongFrame(): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "pong", value: create(WPongSchema, { ts: 1n }) },
  });
}

function socketHarness(workerFp: string): SocketHarness {
  let current = true;
  let ready = true;
  let durableComplete = false;
  const delayedDurable = Promise.withResolvers<void>();
  const received: string[] = [];
  const dispatcher = makeWorkerFrameDispatcher({
    deps: {} as WorkerServiceDeps,
    callerFingerprint: workerFp,
    requestClose() {},
    getWorkerFp: () => workerFp,
    isSnapshotReady: () => ready,
    isCurrentGeneration: () => current,
    fenced: () => !current,
    sendBestEffort: () => true,
    markSnapshotReady: () => false,
    scheduleRespawn() {},
  });
  const conn: WorkerConn = {
    async handleUpstream(frame): Promise<void> {
      received.push(frame.frame.case ?? "unknown");
      if (dispatcher.handleLiveFrame(frame)) return;
      if (frame.frame.case === "event") {
        await delayedDurable.promise;
        durableComplete = true;
      }
    },
    close() {},
    revoke() {},
    isCurrentGeneration: () => current,
    isReady: () => current && ready,
  };
  const data: WorkerWsData = {
    kind: "worker",
    caller: {
      fingerprint: workerFp,
      label: "result-dispatch-test",
      keyGeneration: 1,
      validUntilMs: Date.now() + 60_000,
    },
    fp: workerFp,
    conn,
    queue: null,
    eventRate: { startedAtMs: null, events: 0 },
    announcedChannels: createAnnouncedChannelBarrier(workerFp),
  };
  const ws = { data, close() {} };
  const handler = makeWorkerWsHandler({} as WorkerServiceDeps);
  return {
    data,
    received,
    durableComplete: () => durableComplete,
    deliver(frame): void {
      handler.message(ws as never, Buffer.from(toBinary(CoordWorkerUpSchema, frame)));
    },
    releaseDurable(): void { delayedDurable.resolve(); },
    setCurrent(value): void { current = value; },
    setReady(value): void { ready = value; },
    close(): void {
      delayedDurable.resolve();
      handler.close(ws as never);
    },
  };
}

afterEach(() => {
  rejectPendingRpcsForWorker(TARGET_WORKER_FP, "result dispatch test cleanup");
  rejectPendingRpcsForWorker(FOREIGN_WORKER_FP, "result dispatch test cleanup");
});

describe("worker WS correlated result dispatch", () => {
  test("settles input and stream results without waiting for a durable handler", async () => {
    const harness = socketHarness(TARGET_WORKER_FP);
    const pendingBefore = _pendingRpcStats().pending;
    const input = createPendingRpc<WInputResult>(1_000, TARGET_WORKER_FP);
    const stream = createPendingRpc<WTerminalStreamResult>(1_000, TARGET_WORKER_FP);
    void input.promise.catch(() => undefined);
    void stream.promise.catch(() => undefined);
    try {
      harness.deliver(durableEventFrame());
      await Promise.resolve();
      expect(harness.received).toEqual(["event"]);
      expect(harness.data.queue?.stats().frames).toBe(1);

      harness.deliver(inputResultFrame(input.request_id));
      harness.deliver(streamResultFrame(stream.request_id));
      await expect(input.promise).resolves.toMatchObject({ requestId: input.request_id });
      await expect(stream.promise).resolves.toMatchObject({ requestId: stream.request_id });
      expect(harness.data.queue?.stats().frames).toBe(1);
      expect(harness.durableComplete()).toBe(false);

      harness.deliver(pipelineResultFrame("pipeline-stays-ordered"));
      harness.deliver(pongFrame());
      await Promise.resolve();
      expect(harness.received).toEqual([
        "event",
        "inputResult",
        "terminalStreamResult",
      ]);
      expect(harness.data.queue?.stats().frames).toBe(3);

      harness.releaseDurable();
      await harness.data.queue!.whenIdle();
      expect(harness.durableComplete()).toBe(true);
      expect(harness.received).toEqual([
        "event",
        "inputResult",
        "terminalStreamResult",
        "terminalPipelineSnapshot",
        "pong",
      ]);
      expect(_pendingRpcStats().pending).toBe(pendingBefore);
    } finally {
      harness.close();
    }
  });

  test("leaves stale, unready, foreign, and unmatched results pending", async () => {
    const target = socketHarness(TARGET_WORKER_FP);
    const foreign = socketHarness(FOREIGN_WORKER_FP);
    const pendingBefore = _pendingRpcStats().pending;
    const input = createPendingRpc<WInputResult>(1_000, TARGET_WORKER_FP);
    const stream = createPendingRpc<WTerminalStreamResult>(1_000, TARGET_WORKER_FP);
    void input.promise.catch(() => undefined);
    void stream.promise.catch(() => undefined);
    try {
      target.setCurrent(false);
      target.deliver(inputResultFrame(input.request_id));
      await Promise.resolve();
      expect(target.received).toEqual([]);

      target.setCurrent(true);
      target.setReady(false);
      target.deliver(streamResultFrame(stream.request_id));
      await Promise.resolve();
      expect(target.received).toEqual([]);

      target.setReady(true);
      foreign.deliver(streamResultFrame(stream.request_id));
      target.deliver(inputResultFrame("unmatched-request"));
      await Promise.resolve();
      expect(_pendingRpcStats().pending).toBe(pendingBefore + 2);

      target.deliver(inputResultFrame(input.request_id));
      target.deliver(streamResultFrame(stream.request_id));
      await expect(input.promise).resolves.toMatchObject({ requestId: input.request_id });
      await expect(stream.promise).resolves.toMatchObject({ requestId: stream.request_id });
      expect(_pendingRpcStats().pending).toBe(pendingBefore);
    } finally {
      target.close();
      foreign.close();
    }
  });
});
