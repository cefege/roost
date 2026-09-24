import { afterEach, describe, expect, test, vi } from "bun:test";
import type {
  InputCommand,
  TerminalInputRouteClaim,
  TerminalInputRouteResult,
} from "@roost/protocol/proto/sync_pb";
import type { TerminalGenerationToken } from "../src/store/terminal-stream-types.ts";
import {
  HELD_INPUT_ADMISSION_TIMEOUT_MS,
  MAX_TERMINAL_INPUT_ROUTE_REVISION,
  createTerminalInputRouter,
  type TerminalInputDestination,
} from "../src/ws/terminal-input-router.ts";
import { createTerminalInputLanes, inputMapSizes } from "../src/ws/terminal-input-lanes.ts";

function token(socketId: string, domainGeneration = 1n): TerminalGenerationToken {
  return {
    socketGeneration: 1,
    socketId,
    processEpoch: `epoch-${socketId}`,
    domainGeneration,
    transportKind: "webrtc",
    workerFp: "worker-a",
  };
}

function routeResult(
  command: TerminalInputRouteClaim,
  values: Partial<TerminalInputRouteResult>,
): TerminalInputRouteResult {
  return {
    requestId: command.requestId,
    sessionId: command.sessionId,
    revision: command.revision,
    accepted: false,
    latestRevision: 0n,
    inputRouteEpoch: "",
    workerEpoch: command.workerEpoch,
    reason: "",
    ...values,
  } as TerminalInputRouteResult;
}

function destination(
  tokenValue: TerminalGenerationToken,
  values: Partial<TerminalInputDestination> = {},
): TerminalInputDestination & { inputs: InputCommand[]; claims: TerminalInputRouteClaim[] } {
  const inputs: InputCommand[] = [];
  const claims: TerminalInputRouteClaim[] = [];
  return {
    token: tokenValue,
    workerEpoch: values.workerEpoch ?? tokenValue.processEpoch,
    inputRouteSupported: values.inputRouteSupported ?? (values.claimInputRoute !== undefined),
    sendInput(input): "accepted" | "queued" | "refused" {
      inputs.push(input);
      return values.sendInput?.(input) ?? "accepted";
    },
    ...(values.claimInputRoute ? {
      claimInputRoute: (claim: TerminalInputRouteClaim) => {
        claims.push(claim);
        return values.claimInputRoute!(claim);
      },
    } : {}),
    ...(values.close ? { close: values.close } : {}),
    inputs,
    claims,
  };
}

afterEach(() => vi.useRealTimers());

describe("document terminal input router", () => {
  test("drains a healthy old route, claims the candidate, then releases held input once", async () => {
    const router = createTerminalInputRouter();
    const oldDestination = destination(token("old"));
    const candidate = destination(token("candidate"), {
      claimInputRoute: async (claim) => routeResult(claim, { accepted: true, inputRouteEpoch: "route-candidate" }),
    });
    const started = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!started.accepted) throw new Error(started.reason);
    const hold = router.hold("s1");
    const held = router.admit(oldDestination, "s1", new Uint8Array([2]));
    if (!held.accepted) throw new Error(held.reason);
    const drained = router.drain("s1", oldDestination.token);
    router.settle(oldDestination.token, { sessionId: "s1", inputSeq: started.inputSeq, status: "accepted", writtenBytes: 1 });
    await drained;
    expect((await router.claim("s1", candidate)).accepted).toBe(true);
    hold.release(candidate);
    expect(candidate.inputs).toHaveLength(1);
    expect(candidate.inputs[0]?.inputRouteEpoch).toBe("route-candidate");
    router.settle(candidate.token, { sessionId: "s1", inputSeq: held.inputSeq, status: "accepted", writtenBytes: 1 });
    expect((await held.result).status).toBe("accepted");
    router.dispose();
  });

  test("drains started input through a same-connection domain refresh", async () => {
    const router = createTerminalInputRouter();
    const oldDestination = destination(token("sync", 1n));
    const refreshed = destination(token("sync", 2n));
    const started = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!started.accepted) throw new Error(started.reason);
    router.refresh(refreshed);
    let drained = false;
    const drain = router.drain("s1", refreshed.token).then(() => { drained = true; });
    await Promise.resolve();
    expect(drained).toBe(false);
    router.settle(oldDestination.token, { sessionId: "s1", inputSeq: started.inputSeq, status: "accepted", writtenBytes: 1 });
    await drain;
    expect(drained).toBe(true);
    router.dispose();
  });

  test("preserves a claimed route epoch across a domain generation reset", async () => {
    const router = createTerminalInputRouter();
    const initial = destination(token("sync", 1n), {
      claimInputRoute: async (claim) => routeResult(claim, { accepted: true, inputRouteEpoch: "route-sync" }),
    });
    const hold = router.hold("s1");
    expect((await router.claim("s1", initial)).accepted).toBe(true);
    hold.release(initial);
    const refreshed = destination(token("sync", 2n));
    router.refresh(refreshed);
    const admitted = router.admit(refreshed, "s1", new Uint8Array([1]));
    if (!admitted.accepted) throw new Error(admitted.reason);
    expect(refreshed.inputs[0]).toMatchObject({ domainGeneration: 2n, inputRouteEpoch: "route-sync" });
    router.dispose();
  });

  test("expires held admission and fails a drain when started input times out", async () => {
    vi.useFakeTimers();
    const router = createTerminalInputRouter();
    const direct = destination(token("direct"));
    const started = router.admit(direct, "s1", new Uint8Array([1]));
    if (!started.accepted) throw new Error(started.reason);
    const draining = router.drain("s1", direct.token);
    router.hold("s1");
    const held = router.admit(direct, "s1", new Uint8Array([2]));
    if (!held.accepted) throw new Error(held.reason);
    vi.advanceTimersByTime(HELD_INPUT_ADMISSION_TIMEOUT_MS);
    expect((await held.result).status).toBe("rejected");
    expect((await started.result).status).toBe("ambiguous");
    await expect(draining).rejects.toThrow("cannot drain an ambiguous batch");
    expect(direct.inputs).toHaveLength(1);
    router.dispose();
  });

  test("rejects held bytes when no current destination is acknowledged", async () => {
    const router = createTerminalInputRouter();
    const oldDestination = destination(token("old"));
    const hold = router.hold("s1");
    const held = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!held.accepted) throw new Error(held.reason);
    hold.release();
    expect((await held.result).status).toBe("rejected");
    expect(router.phase("s1")).toBe("blocked");
    expect(oldDestination.inputs).toHaveLength(0);
    router.dispose();
  });

  test("does not let a superseded hold release a newer transition", async () => {
    const router = createTerminalInputRouter();
    const direct = destination(token("direct"));
    const stale = router.hold("s1");
    const current = router.hold("s1");
    const held = router.admit(direct, "s1", new Uint8Array([1]));
    if (!held.accepted) throw new Error(held.reason);
    stale.release(direct);
    expect(stale.isCurrent()).toBe(false);
    expect(direct.inputs).toHaveLength(0);
    current.release(direct);
    expect(direct.inputs).toHaveLength(1);
    router.dispose();
  });

  test("settles a retired started batch as ambiguous and sends only fresh held input after a fallback claim", async () => {
    const router = createTerminalInputRouter();
    const oldDestination = destination(token("old"));
    const fallback = destination(token("fallback"), {
      claimInputRoute: async (claim) => routeResult(claim, { accepted: true, inputRouteEpoch: "route-fallback" }),
    });
    const started = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!started.accepted) throw new Error(started.reason);
    router.retire(oldDestination.token, "direct route closed");
    expect((await started.result).status).toBe("ambiguous");
    const hold = router.hold("s1");
    const fresh = router.admit(oldDestination, "s1", new Uint8Array([2]));
    if (!fresh.accepted) throw new Error(fresh.reason);
    expect(await router.claim("s1", fallback)).toMatchObject({ accepted: true });
    hold.release(fallback);
    expect(oldDestination.inputs).toHaveLength(1);
    expect(fallback.inputs).toHaveLength(1);
    expect(fallback.inputs[0]).toMatchObject({ data: new Uint8Array([2]), inputRouteEpoch: "route-fallback" });
    router.dispose();
  });

  test("invalidates a claim across a same-connection domain refresh", async () => {
    const router = createTerminalInputRouter();
    const deferred = Promise.withResolvers<TerminalInputRouteResult>();
    const initial = destination(token("sync", 1n), { claimInputRoute: () => deferred.promise });
    const refreshed = destination(token("sync", 2n), { claimInputRoute: async (claim) => routeResult(claim, {}) });
    const hold = router.hold("s1");
    const claim = router.claim("s1", initial);
    expect(router.requiresRouteClaim("s1")).toBe(true);
    router.refresh(refreshed);
    deferred.resolve(routeResult(initial.claims[0]!, { accepted: true, inputRouteEpoch: "obsolete" }));
    expect(await claim).toMatchObject({ accepted: false, reason: "terminal input route claim was superseded" });
    hold.release(refreshed);
    expect(refreshed.inputs).toHaveLength(0);
    hold.release();
    expect(router.phase("s1")).toBe("blocked");
    router.dispose();
  });

  test("ignores a stale claim callback and retries one stale revision exactly once", async () => {
    const first = Promise.withResolvers<TerminalInputRouteResult>();
    const second = Promise.withResolvers<TerminalInputRouteResult>();
    const router = createTerminalInputRouter();
    let claimCall = 0;
    const direct = destination(token("candidate"), { claimInputRoute: () => ++claimCall === 1 ? first.promise : second.promise });
    router.hold("s1");
    const stale = router.claim("s1", direct);
    const current = router.claim("s1", direct);
    first.resolve(routeResult(direct.claims[0]!, { accepted: true, inputRouteEpoch: "obsolete" }));
    expect((await stale).accepted).toBe(false);
    second.resolve(routeResult(direct.claims[1]!, { accepted: true, inputRouteEpoch: "current" }));
    expect(await current).toEqual({ accepted: true, inputRouteEpoch: "current", revision: 2n });
    const retryRouter = createTerminalInputRouter();
    let retryCall = 0;
    const retry = destination(token("retry"), {
      claimInputRoute: async (claim) => ++retryCall === 1
        ? routeResult(claim, { reason: "stale_route_revision", latestRevision: 4n })
        : routeResult(claim, { accepted: true, inputRouteEpoch: "retry-route" }),
    });
    retryRouter.hold("s2");
    expect(await retryRouter.claim("s2", retry)).toEqual({ accepted: true, inputRouteEpoch: "retry-route", revision: 5n });
    expect(retry.claims.map((claim) => claim.revision)).toEqual([1n, 5n]);
    router.dispose(); retryRouter.dispose();
  });

  test("closes and refuses a route when its document revision would overflow", async () => {
    const revisions = new Map([["s1", MAX_TERMINAL_INPUT_ROUTE_REVISION]]);
    const router = createTerminalInputRouter(revisions);
    const closed: string[] = [];
    const direct = destination(token("overflow"), {
      claimInputRoute: async (claim) => routeResult(claim, {}), close: (reason) => closed.push(reason),
    });
    expect(await router.claim("s1", direct)).toEqual({ accepted: false, unsupported: false, reason: "terminal input route revision exhausted" });
    expect(closed).toEqual(["terminal input route revision exhausted"]);
    router.dispose();
  });

  test("disposal unregisters an isolated lane owner and its timing entry", () => {
    const baseline = inputMapSizes();
    const lanes = createTerminalInputLanes<null>();
    lanes.enqueue("s1", new Uint8Array([1]), undefined, null);
    expect(inputMapSizes()).toBeGreaterThan(baseline);
    lanes.dispose();
    expect(inputMapSizes()).toBe(baseline);
  });
});
