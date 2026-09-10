// Regression coverage for dispatcher races around route verification and reconnects.
// Deferred routes and RPC results make admission ownership deterministic without
// wall-clock sleeps or a worker socket.
import { afterEach, describe, expect, test } from "bun:test";
import type { WTerminalStreamResult } from "@roost/shared/proto/worker_transport_pb";
import { TerminalStreamDispatcher } from "../src/connect/terminal-stream-dispatcher.ts";
import type { TerminalStreamRoute } from "../src/connect/terminal-view-stream-controller.ts";
import {
  TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER,
  type TerminalStreamDispatcherOptions,
  type TerminalStreamDispatchState,
} from "../src/connect/terminal-stream-dispatcher-types.ts";
import type { HopDeadline, TerminalWorkerRequest } from "../src/connect/worker-send.ts";

const WORKER = "terminal-stream-dispatcher-races-worker";
const dispatchers: TerminalStreamDispatcher[] = [];

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T | PromiseLike<T>): void;
  reject(reason?: unknown): void;
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

function workerRoute(): TerminalStreamRoute {
  return { workerFp: WORKER, channel: 7 };
}

function admitted(
  result: Promise<WTerminalStreamResult>,
): TerminalWorkerRequest<WTerminalStreamResult> {
  return { admitted: true, expired: false, requestId: null, result };
}

async function settleDispatcher(): Promise<void> {
  for (let index = 0; index < 10; index += 1) await Promise.resolve();
}

afterEach(() => {
  for (const dispatcher of dispatchers.splice(0)) dispatcher.dispose();
});

describe("TerminalStreamDispatcher race ownership", () => {
  test("retains replaced admissions after their response channel rejects first", async () => {
    let oldDeadlineRemainingMs = 100;
    let nextTimer = 0;
    let currentGeneration: object = {};
    const timers = new Map<number, () => void>();
    const oldRequests: Deferred<WTerminalStreamResult>[] = [];
    const sent: string[] = [];
    const dispatcher = makeDispatcher({
      resolveRoute: async () => workerRoute(),
      currentWorker: () => currentGeneration,
      sendStream: (_workerFp, state) => {
        const request = deferred<WTerminalStreamResult>();
        if (state.streamId.startsWith("old-")) oldRequests.push(request);
        sent.push(state.streamId);
        return admitted(request.promise);
      },
      setTimer: (callback) => {
        const timer = nextTimer;
        nextTimer += 1;
        timers.set(timer, callback);
        return timer as unknown as NodeJS.Timeout;
      },
      clearTimer: (timer) => {
        timers.delete(timer as unknown as number);
      },
    });
    const oldDeadline: HopDeadline = {
      totalMs: 100,
      remainingMs: () => oldDeadlineRemainingMs,
    };

    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`old-session-${index}`, `old-${index}`, oldDeadline));
    }
    await settleDispatcher();
    expect(oldRequests).toHaveLength(TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER);

    currentGeneration = {};
    for (const request of oldRequests) request.reject(new Error("old response channel replaced"));
    await settleDispatcher();
    dispatcher.workerReplacement(WORKER);

    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`new-session-${index}`, `new-${index}`));
    }
    await settleDispatcher();
    expect(sent).toHaveLength(TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER);

    oldDeadlineRemainingMs = -1;
    expect(timers.size).toBe(1);
    const deadlineTimer = timers.values().next().value;
    if (!deadlineTimer) throw new Error("expected retained admission deadline timer");
    deadlineTimer();
    await settleDispatcher();

    expect(sent).toHaveLength(TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER * 2);
  });

  test("does not count stalled final route checks as worker admissions", async () => {
    const stalledFinalRoute = deferred<TerminalStreamRoute | null>();
    const routeCalls = new Map<string, number>();
    const sent: string[] = [];
    const dispatcher = makeDispatcher({
      resolveRoute: (sessionId) => {
        const calls = (routeCalls.get(sessionId) ?? 0) + 1;
        routeCalls.set(sessionId, calls);
        if (sessionId.startsWith("stalled-")) {
          return calls === 1 ? Promise.resolve(workerRoute()) : stalledFinalRoute.promise;
        }
        return Promise.resolve(workerRoute());
      },
      sendStream: (_workerFp, state) => {
        sent.push(state.streamId);
        return admitted(deferred<WTerminalStreamResult>().promise);
      },
    });

    for (let index = 0; index < TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER; index += 1) {
      dispatcher.enqueue(streamState(`stalled-${index}`, `stalled-stream-${index}`));
    }
    dispatcher.enqueue(streamState("ready-session", "ready-stream"));
    await settleDispatcher();
    expect([...routeCalls.values()].filter((calls) => calls === 2)).toHaveLength(
      TERMINAL_STREAM_DISPATCH_MAX_IN_FLIGHT_PER_WORKER + 1,
    );

    expect(sent).toEqual(["ready-stream"]);
  });
  test("fences unresolved retired sessions before clearing their worker epoch", async () => {
    const unresolvedRoute = deferred<TerminalStreamRoute | null>();
    const sent: string[] = [];
    const dispatcher = makeDispatcher({
      resolveRoute: (sessionId) => sessionId === "retired-session"
        ? unresolvedRoute.promise
        : Promise.resolve(workerRoute()),
      sendStream: (_workerFp, state) => {
        sent.push(state.streamId);
        return admitted(deferred<WTerminalStreamResult>().promise);
      },
    });

    const retired = dispatcher.enqueue(streamState("retired-session", "retired-stream"));
    dispatcher.workerReplacement(WORKER);
    dispatcher.workerRetired(WORKER, ["retired-session"]);
    unresolvedRoute.resolve(workerRoute());

    expect(await retired.completion).toEqual({
      kind: "cancelled",
      reason: "worker_generation_replaced",
    });
    await settleDispatcher();
    expect(sent).toEqual([]);
    const dispatcherState: unknown = dispatcher;
    if (
      !dispatcherState
      || typeof dispatcherState !== "object"
      || !("workerReplacementEpochs" in dispatcherState)
      || !(dispatcherState.workerReplacementEpochs instanceof Map)
    ) throw new Error("expected worker replacement epoch state");
    expect(dispatcherState.workerReplacementEpochs.size).toBe(0);

    dispatcher.enqueue(streamState("replacement-session", "replacement-stream"));
    await settleDispatcher();
    expect(sent).toEqual(["replacement-stream"]);
  });
});
