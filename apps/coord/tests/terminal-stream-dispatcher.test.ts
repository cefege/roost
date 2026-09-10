// Exercises the coordinator's terminal stream-state dispatcher without a worker socket.
// The stream-state lane is deliberately tested separately from terminal input so
// worker-control fairness cannot accidentally pace input or snapshot requests.
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  TerminalStreamStatus,
  WTerminalStreamResultSchema,
  type WTerminalStreamResult,
} from "@roost/shared/proto/worker_transport_pb";
import { rejectPendingRpcsForWorker } from "../src/router/pending-rpcs.ts";
import { TerminalStreamDispatcher } from "../src/connect/terminal-stream-dispatcher.ts";
import type { TerminalStreamRoute } from "../src/connect/terminal-view-stream-controller.ts";
import {
  TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER,
  type TerminalStreamDispatcherOptions,
  type TerminalStreamDispatchState,
} from "../src/connect/terminal-stream-dispatcher-types.ts";
import {
  sendTerminalInputRequest,
  type HopDeadline,
  type TerminalWorkerRequest,
} from "../src/connect/worker-send.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";

const WORKER = "terminal-stream-dispatcher-worker";
const OTHER_WORKER = "terminal-stream-dispatcher-worker-b";
const dispatchers: TerminalStreamDispatcher[] = [];

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T | PromiseLike<T>): void;
  reject(reason?: unknown): void;
}

afterEach(() => {
  for (const dispatcher of dispatchers.splice(0)) dispatcher.dispose();
  rejectPendingRpcsForWorker(WORKER, "terminal stream dispatcher test complete");
  __setConnectWorkerForTest(WORKER, null);
});

function makeDispatcher(options: TerminalStreamDispatcherOptions): TerminalStreamDispatcher {
  const dispatcher = new TerminalStreamDispatcher(options);
  dispatchers.push(dispatcher);
  return dispatcher;
}

function longDeadline(): HopDeadline {
  return { totalMs: 60_000, remainingMs: () => 60_000 };
}

function streamState(
  sessionId: string,
  streamId: string,
  deadline: HopDeadline = longDeadline(),
): TerminalStreamDispatchState {
  return {
    sessionId,
    streamId,
    enabled: true,
    cols: 80,
    rows: 24,
    deadline,
  };
}

function workerRoute(workerFp = WORKER): TerminalStreamRoute {
  return { workerFp, channel: 7 };
}

function streamResult(
  state: Omit<TerminalStreamDispatchState, "deadline">,
): WTerminalStreamResult {
  return create(WTerminalStreamResultSchema, {
    sessionId: state.sessionId,
    streamId: state.streamId,
    enabled: state.enabled,
    status: TerminalStreamStatus.COMMITTED,
    effectiveCols: state.cols,
    effectiveRows: state.rows,
  });
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function admitted(
  result: Promise<WTerminalStreamResult>,
): TerminalWorkerRequest<WTerminalStreamResult> {
  return { admitted: true, expired: false, requestId: null, result };
}

async function settleDispatcher(): Promise<void> {
  for (let index = 0; index < 10; index += 1) await Promise.resolve();
}

describe("TerminalStreamDispatcher", () => {
  test("caps actual stream-state requests at 32 per worker", async () => {
    const sent: Array<Omit<TerminalStreamDispatchState, "deadline">> = [];
    const pending = new Map<string, Deferred<WTerminalStreamResult>>();
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      sendStream: (_workerFp, state) => {
        const result = deferred<WTerminalStreamResult>();
        pending.set(state.streamId, result);
        sent.push(state);
        return admitted(result.promise);
      },
    });

    for (let index = 0; index <= TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`session-${index}`, `stream-${index}`));
    }
    await settleDispatcher();
    expect(sent).toHaveLength(TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER);

    pending.get("stream-0")!.resolve(streamResult(sent[0]!));
    await settleDispatcher();
    expect(sent).toHaveLength(TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER + 1);
    expect(sent.at(-1)?.streamId).toBe("stream-32");
  });

  test("rotates a hot session behind another eligible session", async () => {
    const sent: Array<Omit<TerminalStreamDispatchState, "deadline">> = [];
    const pending = new Map<string, Deferred<WTerminalStreamResult>>();
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      sendStream: (_workerFp, state) => {
        const result = deferred<WTerminalStreamResult>();
        pending.set(state.streamId, result);
        sent.push(state);
        return admitted(result.promise);
      },
    });
    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`blocker-${index}`, `blocker-stream-${index}`));
    }
    await settleDispatcher();

    dispatcher.enqueue(streamState("hot-session", "hot-old"));
    dispatcher.enqueue(streamState("peer-session", "peer-stream"));
    dispatcher.enqueue(streamState("hot-session", "hot-new"));
    await settleDispatcher();
    pending.get("blocker-stream-0")!.resolve(streamResult(sent[0]!));
    await settleDispatcher();
    pending.get("blocker-stream-1")!.resolve(streamResult(sent[1]!));
    await settleDispatcher();

    expect(sent.slice(-2).map((state) => state.streamId)).toEqual(["peer-stream", "hot-new"]);
  });

  test("retains only the newest unsent state for a session", async () => {
    const sent: Array<Omit<TerminalStreamDispatchState, "deadline">> = [];
    const pending = new Map<string, Deferred<WTerminalStreamResult>>();
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      sendStream: (_workerFp, state) => {
        const result = deferred<WTerminalStreamResult>();
        pending.set(state.streamId, result);
        sent.push(state);
        return admitted(result.promise);
      },
    });
    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`blocker-${index}`, `blocker-stream-${index}`));
    }
    await settleDispatcher();

    const oldRequest = dispatcher.enqueue(streamState("coalesced-session", "coalesced-old"));
    const newRequest = dispatcher.enqueue(streamState("coalesced-session", "coalesced-new"));
    await settleDispatcher();
    expect(await oldRequest.completion).toEqual({ kind: "cancelled", reason: "superseded" });
    expect(newRequest.accepted).toBe(true);

    pending.get("blocker-stream-0")!.resolve(streamResult(sent[0]!));
    await settleDispatcher();
    expect(sent.at(-1)?.streamId).toBe("coalesced-new");
    expect(sent.some((state) => state.streamId === "coalesced-old")).toBe(false);
  });

  test("expires a locally accepted queued state on its original deadline", async () => {
    const sent: Array<Omit<TerminalStreamDispatchState, "deadline">> = [];
    const pending = new Map<string, Deferred<WTerminalStreamResult>>();
    let remainingMs = 10;
    let deadlineTimer: (() => void) | null = null;
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      sendStream: (_workerFp, state) => {
        const result = deferred<WTerminalStreamResult>();
        pending.set(state.streamId, result);
        sent.push(state);
        return admitted(result.promise);
      },
      setTimer: (callback) => {
        deadlineTimer = callback;
        return 0 as unknown as NodeJS.Timeout;
      },
      clearTimer: () => undefined,
    });
    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`blocker-${index}`, `blocker-stream-${index}`));
    }
    await settleDispatcher();

    const queued = dispatcher.enqueue(streamState("expired-session", "expired-stream", {
      totalMs: 10,
      remainingMs: () => remainingMs,
    }));
    await settleDispatcher();
    expect(queued.accepted).toBe(true);
    expect(sent.some((state) => state.streamId === "expired-stream")).toBe(false);

    remainingMs = -1;
    deadlineTimer!();
    const completion = await queued.completion;
    expect(completion.kind).toBe("request");
    if (completion.kind === "request") {
      expect(completion.request).toMatchObject({ admitted: false, expired: true });
    }
    expect(sent.some((state) => state.streamId === "expired-stream")).toBe(false);
  });

  test("cancels a route that changes during the pre-write check", async () => {
    const verification = deferred<TerminalStreamRoute | null>();
    const sent: string[] = [];
    let routeCalls = 0;
    const dispatcher = makeDispatcher({
      resolveRoute: () => {
        routeCalls += 1;
        return routeCalls === 1 ? Promise.resolve(workerRoute()) : verification.promise;
      },
      sendStream: (_workerFp, state) => {
        sent.push(state.streamId);
        return admitted(Promise.resolve(streamResult(state)));
      },
    });

    const request = dispatcher.enqueue(streamState("route-session", "route-stream"));
    await settleDispatcher();
    verification.resolve(workerRoute(OTHER_WORKER));
    expect(await request.completion).toEqual({ kind: "cancelled", reason: "route_changed" });
    expect(sent).toEqual([]);
  });

  test("cancels work created before a worker generation replacement resolves its route", async () => {
    const initialRoute = deferred<TerminalStreamRoute | null>();
    const sent: string[] = [];
    const firstGeneration = {};
    let currentGeneration: object = firstGeneration;
    const dispatcher = makeDispatcher({
      resolveRoute: () => initialRoute.promise,
      currentWorker: () => currentGeneration,
      sendStream: (_workerFp, state) => {
        sent.push(state.streamId);
        return admitted(Promise.resolve(streamResult(state)));
      },
    });

    const request = dispatcher.enqueue(streamState("generation-session", "generation-stream"));
    dispatcher.workerReplacement(WORKER);
    currentGeneration = {};
    initialRoute.resolve(workerRoute());
    expect(await request.completion).toEqual({
      kind: "cancelled",
      reason: "worker_generation_replaced",
    });
    expect(sent).toEqual([]);
  });

  test("reports local acceptance separately from later transport non-admission", async () => {
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      sendStream: () => ({
        admitted: false,
        expired: false,
        requestId: null,
        result: Promise.reject(new Error("worker transport dropped stream state")),
      }),
    });

    const request = dispatcher.enqueue(streamState("transport-session", "transport-stream"));
    expect(request.accepted).toBe(true);
    const completion = await request.completion;
    expect(completion.kind).toBe("request");
    if (completion.kind === "request") expect(completion.request.admitted).toBe(false);
  });

  test("does not pace terminal input behind a full stream-state worker window", async () => {
    const pending = new Map<string, Deferred<WTerminalStreamResult>>();
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      sendStream: (_workerFp, state) => {
        const result = deferred<WTerminalStreamResult>();
        pending.set(state.streamId, result);
        return admitted(result.promise);
      },
    });
    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`blocker-${index}`, `blocker-stream-${index}`));
    }
    await settleDispatcher();
    expect(pending.size).toBe(TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER);

    const frames: string[] = [];
    __setConnectWorkerForTest(WORKER, {
      workerFp: WORKER,
      send: (frame) => {
        frames.push(frame.frame.case ?? "");
        return 1;
      },
    });
    const input = sendTerminalInputRequest(WORKER, {
      sessionId: "input-session",
      inputSeq: 1n,
      data: Uint8Array.of(0x61),
    });
    void input.result.catch(() => undefined);

    expect(input.admitted).toBe(true);
    expect(frames).toEqual(["inputRequest"]);
  });
});
