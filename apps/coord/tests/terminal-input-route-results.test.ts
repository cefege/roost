// Focused coordinator coverage for typed input-route/probe correlation.
// These tests use a fake current WorkerHandle so they can prove outer-id fencing,
// socket cancellation, bounded waiter admission, and input actor serialization
// without starting a coordinator or a worker runtime.

import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  WTerminalInputRouteResultSchema,
  WTerminalTransportProbeResultSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import { TerminalInputRouteResultSchema } from "@roost/shared/proto/sync_pb";
import { TERMINAL_INPUT_ROUTE_CAPABILITY } from "@roost/shared/terminal-peer";
import { Code } from "@connectrpc/connect";
import {
  MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET,
  TerminalInputRouteResults,
} from "../src/connect/terminal-input-route-results.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import { sendTerminalInputRequest, type HopDeadline } from "../src/connect/worker-send.ts";
import { cancelPendingRpc } from "../src/router/pending-rpcs.ts";

const WORKER_FP = "a".repeat(64);
const WORKER_EPOCH = "worker-epoch-a";
const CONTROL_CONNECTION = "sync-connection-a";
let routeResults: TerminalInputRouteResults | null = null;

afterEach(() => {
  routeResults?.dispose();
  routeResults = null;
  __setConnectWorkerForTest(WORKER_FP, null);
});

function attachWorker(sent: CoordWorkerDown[]): WorkerHandle {
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: WORKER_EPOCH,
    connectionGeneration: "worker-connection-a",
    capabilities: new Set([TERMINAL_INPUT_ROUTE_CAPABILITY]),
    revoked: false,
    ready: true,
    send(frame): number {
      sent.push(frame);
      return 1;
    },
  };
  __setConnectWorkerForTest(WORKER_FP, worker);
  return worker;
}

function routeClaim(worker: WorkerHandle, requestId = "browser-route-request") {
  return {
    browserRequestId: requestId,
    sessionId: "session-route-a",
    revision: 1n,
    deviceFingerprint: "device-route-a",
    tabId: "tab-route-a",
    connectionId: CONTROL_CONNECTION,
    worker,
    workerEpoch: WORKER_EPOCH,
  };
}

function routeResult(outerRequestId: string, innerRequestId = outerRequestId) {
  return create(WTerminalInputRouteResultSchema, {
    requestId: outerRequestId,
    result: create(TerminalInputRouteResultSchema, {
      requestId: innerRequestId,
      sessionId: "session-route-a",
      revision: 1n,
      accepted: true,
      latestRevision: 1n,
      inputRouteEpoch: "route-epoch-a",
      workerEpoch: WORKER_EPOCH,
    }),
  });
}

describe("TerminalInputRouteResults", () => {
  test("installs typed correlation before send and restores only the validated browser nonce", async () => {
    const sent: CoordWorkerDown[] = [];
    const worker = attachWorker(sent);
    routeResults = new TerminalInputRouteResults();

    const operation = routeResults.claim(routeClaim(worker));
    const sentClaim = sent[0];
    if (sentClaim?.frame.case !== "terminalInputRouteClaim") {
      throw new Error("expected terminal input route claim");
    }
    const outerRequestId = sentClaim.frame.value.requestId;
    expect(outerRequestId).not.toBe("browser-route-request");
    expect(sentClaim.frame.value.deviceFingerprint).toBe("device-route-a");
    expect(sentClaim.frame.value.tabId).toBe("tab-route-a");
    expect(sentClaim.frame.value.browserConnectionId).toBe(CONTROL_CONNECTION);

    const wrongSource = { ...worker, connectionGeneration: "worker-connection-b" };
    expect(routeResults.acceptInputRouteResult(wrongSource, routeResult(outerRequestId))).toBe(false);
    expect(routeResults.acceptInputRouteResult(worker, routeResult(outerRequestId, "wrong-inner-id"))).toBe(false);
    expect(routeResults.acceptInputRouteResult(worker, routeResult(outerRequestId))).toBe(true);

    await expect(operation).resolves.toMatchObject({
      requestId: "browser-route-request",
      sessionId: "session-route-a",
      inputRouteEpoch: "route-epoch-a",
      workerEpoch: WORKER_EPOCH,
    });
    routeResults.retireBrowserConnection(CONTROL_CONNECTION);
    const closed = sent[1];
    if (closed?.frame.case !== "terminalViewSocketClosed") {
      throw new Error("expected exact worker route retirement");
    }
    expect(closed.frame.value.socketId).toBe(CONTROL_CONNECTION);

  });


  test("does not settle a typed result after its captured worker handle is replaced", async () => {
    const sent: CoordWorkerDown[] = [];
    const worker = attachWorker(sent);
    routeResults = new TerminalInputRouteResults();
    const operation = routeResults.claim(routeClaim(worker, "browser-route-replaced"));
    const claim = sent[0];
    if (claim?.frame.case !== "terminalInputRouteClaim") {
      throw new Error("expected terminal input route claim");
    }
    __setConnectWorkerForTest(WORKER_FP, {
      ...worker,
      connectionGeneration: "worker-connection-replacement",
    });

    expect(routeResults.acceptInputRouteResult(worker, routeResult(claim.frame.value.requestId))).toBe(false);
    routeResults.cancelForWorkerHandle(worker, "connection_superseded");
    await expect(operation).rejects.toMatchObject({ code: Code.Canceled });
  });



  test("retires a claimed route through a same-epoch worker reconnection", async () => {
    const firstSent: CoordWorkerDown[] = [];
    const worker = attachWorker(firstSent);
    routeResults = new TerminalInputRouteResults();
    const operation = routeResults.claim(routeClaim(worker, "browser-route-reconnect"));
    const claim = firstSent[0];
    if (claim?.frame.case !== "terminalInputRouteClaim") {
      throw new Error("expected terminal input route claim");
    }
    expect(routeResults.acceptInputRouteResult(worker, routeResult(claim.frame.value.requestId))).toBe(true);
    await expect(operation).resolves.toMatchObject({ requestId: "browser-route-reconnect" });

    const reconnectedSent: CoordWorkerDown[] = [];
    __setConnectWorkerForTest(WORKER_FP, null);
    routeResults.retireBrowserConnection(CONTROL_CONNECTION);
    expect(reconnectedSent).toEqual([]);

    __setConnectWorkerForTest(WORKER_FP, {
      ...worker,
      connectionGeneration: "worker-connection-reconnected",
      send(frame): number {
        reconnectedSent.push(frame);
        return 1;
      },
    });
    routeResults.flushWorkerRetirements(WORKER_FP);

    expect(reconnectedSent).toHaveLength(1);
    expect(reconnectedSent[0]?.frame.case).toBe("terminalViewSocketClosed");
  });

  test("does not send a claim after its pre-lookup socket reservation is retired", async () => {
    const sent: CoordWorkerDown[] = [];
    const worker = attachWorker(sent);
    routeResults = new TerminalInputRouteResults();
    const slot = routeResults.reserveControl(CONTROL_CONNECTION, "browser-route-before-lookup");

    routeResults.retireBrowserConnection(CONTROL_CONNECTION);

    await expect(routeResults.claim(
      routeClaim(worker, "browser-route-before-lookup"),
      slot,
    )).rejects.toMatchObject({ reason: "terminal_input_route_unavailable" });
    expect(sent).toEqual([]);
  });

  test("bounds one Sync socket and releases its waiters on close", async () => {
    const sent: CoordWorkerDown[] = [];
    const worker = attachWorker(sent);
    routeResults = new TerminalInputRouteResults();
    const operations = Array.from(
      { length: MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET },
      (_value, index) => routeResults!.claim(routeClaim(worker, `browser-route-${index}`)),
    );
    for (const operation of operations) void operation.catch(() => undefined);

    await expect(
      routeResults.claim(routeClaim(worker, "browser-route-over-limit")),
    ).rejects.toMatchObject({ reason: "route_claim_busy" });
    routeResults.retireBrowserConnection(CONTROL_CONNECTION);
    const outcomes = await Promise.allSettled(operations);
    expect(outcomes.every((outcome) => outcome.status === "rejected")).toBe(true);
    expect(sent).toHaveLength(MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET + 1);
    expect(sent.at(-1)?.frame.case).toBe("terminalViewSocketClosed");

    const replacement = routeResults.claim(routeClaim(worker, "browser-route-replacement"));
    void replacement.catch(() => undefined);
    expect(sent).toHaveLength(MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET + 2);
  });

  test("times out a typed probe through the same pending-RPC deadline", async () => {
    const sent: CoordWorkerDown[] = [];
    const worker = attachWorker(sent);
    routeResults = new TerminalInputRouteResults();
    let remainingCalls = 0;
    const deadline: HopDeadline = {
      totalMs: 1_000,
      remainingMs: () => (++remainingCalls === 1 ? 1_000 : 1),
    };

    await expect(routeResults.probe({
      browserRequestId: "browser-probe-request",
      connectionId: CONTROL_CONNECTION,
      workerFp: WORKER_FP,
      worker,
      workerEpoch: WORKER_EPOCH,
      deadline,
    })).rejects.toMatchObject({ code: Code.DeadlineExceeded });
    expect(sent).toHaveLength(1);
    expect(sent[0]?.frame.case).toBe("terminalTransportProbe");
  });

  test("serializes the authenticated actor and route epoch on Sync input", () => {
    const sent: CoordWorkerDown[] = [];
    attachWorker(sent);
    const input = sendTerminalInputRequest(WORKER_FP, {
      sessionId: "session-route-a",
      inputSeq: 9n,
      data: Uint8Array.of(0x78),
      deviceFingerprint: "device-route-a",
      tabId: "tab-route-a",
      browserConnectionId: CONTROL_CONNECTION,
      inputRouteEpoch: "route-epoch-a",
    });
    void input.result.catch(() => undefined);
    if (input.requestId !== null) cancelPendingRpc(input.requestId, WORKER_FP);

    const sentInput = sent[0];
    if (sentInput?.frame.case !== "inputRequest") throw new Error("expected input request");
    expect(sentInput.frame.value).toMatchObject({
      deviceFingerprint: "device-route-a",
      tabId: "tab-route-a",
      browserConnectionId: CONTROL_CONNECTION,
      inputRouteEpoch: "route-epoch-a",
    });
  });

  test("drops a mismatched probe epoch without settling its browser waiter", async () => {
    const sent: CoordWorkerDown[] = [];
    const worker = attachWorker(sent);
    routeResults = new TerminalInputRouteResults();
    const operation = routeResults.probe({
      browserRequestId: "browser-probe-request",
      connectionId: CONTROL_CONNECTION,
      workerFp: WORKER_FP,
      worker,
      workerEpoch: WORKER_EPOCH,
    });
    const sentProbe = sent[0];
    if (sentProbe?.frame.case !== "terminalTransportProbe") throw new Error("expected terminal transport probe");
    expect(routeResults.acceptTransportProbeResult(worker, create(WTerminalTransportProbeResultSchema, {
      requestId: sentProbe.frame.value.requestId,
      workerEpoch: "wrong-epoch",
    }))).toBe(false);
    routeResults.retireBrowserConnection(CONTROL_CONNECTION);
    await expect(operation).rejects.toMatchObject({ code: Code.Canceled });
  });
});
