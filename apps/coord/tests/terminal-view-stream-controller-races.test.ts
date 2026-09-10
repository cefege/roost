// Regression coverage for controller cancellation and post-admission generation replacement.
// The hub harness keeps route changes and deadline exhaustion deterministic through
// deferred results while retaining its MessageChannel settlement boundary.
import { afterEach, describe, expect, test } from "bun:test";
import { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import type { WTerminalStreamResult } from "@roost/shared/proto/worker_transport_pb";
import type { HopDeadline } from "../src/connect/worker-send.ts";
import {
  SESSION,
  VIEW_A,
  WORKER,
  admitted,
  deferred,
  disposeHubs,
  makeHarness,
  register,
  resultFor,
  settle,
  statesFor,
  type Route,
  viewCommand,
} from "./terminal-view-hub-harness.ts";

afterEach(disposeHubs);

describe("TerminalViewStreamController cancellation reconciliation", () => {
  test("publishes unavailable state when generation loss cancels a final route check", async () => {
    const finalRoute = deferred<Route>();
    let routeCalls = 0;
    let recovered = false;
    const { hub, sent } = makeHarness({
      resolveRoute: async () => {
        routeCalls += 1;
        if (routeCalls === 1) return { workerFp: WORKER, channel: 7 };
        if (routeCalls === 2) return finalRoute.promise;
        return recovered ? { workerFp: WORKER, channel: 7 } : null;
      },
    });
    const sink = register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    expect(routeCalls).toBe(2);

    hub.workerReplacement(WORKER);
    finalRoute.resolve(null);
    await settle();

    expect(sent).toEqual([]);
    expect(hub.snapshot(SESSION)?.unavailable).toBe(true);
    expect(statesFor(sink, VIEW_A).map((state) => state.status)).toEqual([
      TerminalViewStatus.ACCEPTED,
      TerminalViewStatus.UNAVAILABLE,
    ]);

    recovered = true;
    hub.routeReconciled(WORKER, [SESSION]);
    await settle();

    expect(sent).toHaveLength(1);
    expect(hub.snapshot(SESSION)?.unavailable).toBe(false);
  });

  test("keeps an immediately reconciled replacement on its cancelled deadline", async () => {
    let deadlinesCreated = 0;
    let routeCalls = 0;
    const finalRoute = deferred<Route>();
    const originalDeadline: HopDeadline = { totalMs: 60_000, remainingMs: () => 60_000 };
    const refreshedDeadline: HopDeadline = { totalMs: 60_000, remainingMs: () => 60_000 };
    const deliveredDeadlines: HopDeadline[] = [];
    const { hub } = makeHarness({
      createStreamDeadline: () => {
        deadlinesCreated += 1;
        return deadlinesCreated === 1 ? originalDeadline : refreshedDeadline;
      },
      resolveRoute: () => {
        routeCalls += 1;
        if (routeCalls === 1) {
          return Promise.resolve({ workerFp: WORKER, channel: 7 });
        }
        if (routeCalls === 2) return finalRoute.promise;
        return Promise.resolve({ workerFp: WORKER, channel: 7 });
      },
      sendStreamState: (_workerFp, state, deadline) => {
        if (!deadline) throw new Error("expected stream deadline");
        deliveredDeadlines.push(deadline);
        return admitted(Promise.resolve(resultFor(state)));
      },
    });
    register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    expect(routeCalls).toBe(2);

    hub.workerReplacement(WORKER);
    await settle();

    expect(deliveredDeadlines).toHaveLength(1);
    expect(deliveredDeadlines[0]).toBe(originalDeadline);
    expect(deadlinesCreated).toBe(1);
  });

  test("keeps a direct route-change redrive on the original deadline", async () => {
    let originalRemainingMs = 60_000;
    let deadlinesCreated = 0;
    let routeCalls = 0;
    const recoveryVerification = deferred<Route>();
    const originalDeadline: HopDeadline = {
      totalMs: 60_000,
      remainingMs: () => originalRemainingMs,
    };
    const refreshedDeadline: HopDeadline = {
      totalMs: 60_000,
      remainingMs: () => 60_000,
    };
    const { hub, sent } = makeHarness({
      createStreamDeadline: () => {
        deadlinesCreated += 1;
        return deadlinesCreated === 1 ? originalDeadline : refreshedDeadline;
      },
      resolveRoute: () => {
        routeCalls += 1;
        if (routeCalls === 1) {
          return Promise.resolve({ workerFp: WORKER, channel: 7 });
        }
        if (routeCalls === 2 || routeCalls === 3) {
          return Promise.resolve({ workerFp: "worker-b", channel: 7 });
        }
        return recoveryVerification.promise;
      },
      sendStreamState: (_workerFp, state) => admitted(Promise.resolve(resultFor(state))),
    });
    register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    expect(routeCalls).toBe(4);
    expect(deadlinesCreated).toBe(1);

    originalRemainingMs = -1;
    recoveryVerification.resolve({ workerFp: "worker-b", channel: 7 });
    await settle();

    expect(sent).toEqual([]);
    expect(deadlinesCreated).toBe(1);
    expect(hub.snapshot(SESSION)?.unavailable).toBe(true);
  });
  test("keeps an admitted replaced request route-owned until recovery", async () => {
    let deadlinesCreated = 0;
    let currentGeneration: object = {};
    const originalDeadline: HopDeadline = { totalMs: 60_000, remainingMs: () => 60_000 };
    const refreshedDeadline: HopDeadline = { totalMs: 60_000, remainingMs: () => 60_000 };
    const oldResult = deferred<WTerminalStreamResult>();
    const deliveredDeadlines: HopDeadline[] = [];
    const { hub, sent } = makeHarness({
      createStreamDeadline: () => {
        deadlinesCreated += 1;
        return deadlinesCreated === 1 ? originalDeadline : refreshedDeadline;
      },
      currentWorker: () => currentGeneration,
      sendStreamState: (_workerFp, state, deadline) => {
        if (!deadline) throw new Error("expected stream deadline");
        deliveredDeadlines.push(deadline);
        return deliveredDeadlines.length === 1
          ? admitted(oldResult.promise)
          : admitted(Promise.resolve(resultFor(state)));
      },
    });
    const sink = register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    expect(sent).toHaveLength(1);
    expect(deliveredDeadlines[0]).toBe(originalDeadline);

    currentGeneration = {};
    oldResult.reject(new Error("old generation response channel replaced"));
    await settle();

    expect(hub.snapshot(SESSION)?.unavailable).toBe(true);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    expect(sent).toHaveLength(1);
    expect(statesFor(sink, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.UNAVAILABLE);

    hub.workerReplacement(WORKER);
    await settle();

    expect(sent).toHaveLength(2);
    expect(deliveredDeadlines[1]).toBe(originalDeadline);
    expect(deadlinesCreated).toBe(1);
  });
});
