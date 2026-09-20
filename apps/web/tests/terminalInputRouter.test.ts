import { afterEach, describe, expect, test, vi } from "bun:test";
import type {
  InputCommand,
  TerminalInputRouteClaim,
  TerminalInputRouteResult,
} from "@roost/shared/proto/sync_pb";
import type { TerminalGenerationToken } from "../src/store/terminal-stream-types.ts";
import {
  HELD_INPUT_ADMISSION_TIMEOUT_MS,
  MAX_TERMINAL_INPUT_ROUTE_REVISION,
  createTerminalInputRouter,
  type TerminalInputDestination,
} from "../src/ws/terminal-input-router.ts";
import { createTerminalInputLanes, inputMapSizes } from "../src/ws/terminal-input-lanes.ts";

function token(
  socketId: string,
  domainGeneration = 1n,
): TerminalGenerationToken {
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
      claimInputRoute: async (claim) => routeResult(claim, {
        accepted: true,
        inputRouteEpoch: "route-candidate",
      }),
    });
    const started = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!started.accepted) throw new Error(started.reason);
    const release = router.hold("s1");
    const held = router.admit(oldDestination, "s1", new Uint8Array([2]));
    if (!held.accepted) throw new Error(held.reason);

    const drained = router.drain("s1", oldDestination.token);
    router.settle(oldDestination.token, {
      sessionId: "s1",
      inputSeq: started.inputSeq,
      status: "accepted",
      writtenBytes: 1,
    });
    await drained;

    expect((await router.claim("s1", candidate)).accepted).toBe(true);
    release(candidate);
    expect(candidate.inputs).toHaveLength(1);
    expect(candidate.inputs[0]?.inputRouteEpoch).toBe("route-candidate");
    router.settle(candidate.token, {
      sessionId: "s1",
      inputSeq: held.inputSeq,
      status: "accepted",
      writtenBytes: 1,
    });
    expect((await held.result).status).toBe("accepted");
    router.dispose();
  });
  test("preserves a claimed route epoch across a domain generation reset", async () => {
    const router = createTerminalInputRouter();
    const initial = destination(token("sync", 1n), {
      claimInputRoute: async (claim) => routeResult(claim, {
        accepted: true,
        inputRouteEpoch: "route-sync",
      }),
    });
    const release = router.hold("s1");
    expect((await router.claim("s1", initial)).accepted).toBe(true);
    release(initial);

    const refreshed = destination(token("sync", 2n));
    router.refresh(refreshed);
    const admitted = router.admit(refreshed, "s1", new Uint8Array([1]));
    if (!admitted.accepted) throw new Error(admitted.reason);
    expect(refreshed.inputs[0]).toMatchObject({
      domainGeneration: 2n,
      inputRouteEpoch: "route-sync",
    });
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
    const release = router.hold("s1");
    const held = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!held.accepted) throw new Error(held.reason);
    release();
    expect((await held.result).status).toBe("rejected");
    expect(oldDestination.inputs).toHaveLength(0);
    router.dispose();
  });

  test("settles a retired started batch as ambiguous and never replays it", async () => {
    const router = createTerminalInputRouter();
    const oldDestination = destination(token("old"));
    const replacement = destination(token("replacement"));
    const admitted = router.admit(oldDestination, "s1", new Uint8Array([1]));
    if (!admitted.accepted) throw new Error(admitted.reason);
    router.retire(oldDestination.token, "direct route closed");
    expect((await admitted.result).status).toBe("ambiguous");
    router.refresh(replacement);
    expect(replacement.inputs).toHaveLength(0);
    router.dispose();
  });
  test("ignores a stale claim callback and retries one stale revision exactly once", async () => {
    const first = Promise.withResolvers<TerminalInputRouteResult>();
    const second = Promise.withResolvers<TerminalInputRouteResult>();
    const router = createTerminalInputRouter();
    let claimCall = 0;
    const direct = destination(token("candidate"), {
      claimInputRoute: () => ++claimCall === 1 ? first.promise : second.promise,
    });
    router.hold("s1");
    const stale = router.claim("s1", direct);
    const current = router.claim("s1", direct);
    first.resolve(routeResult(direct.claims[0]!, {
      accepted: true,
      inputRouteEpoch: "obsolete",
    }));
    expect((await stale).accepted).toBe(false);
    second.resolve(routeResult(direct.claims[1]!, {
      accepted: true,
      inputRouteEpoch: "current",
    }));
    expect(await current).toEqual({ accepted: true, inputRouteEpoch: "current", revision: 2n });

    const retryRouter = createTerminalInputRouter();
    let retryCall = 0;
    const retry = destination(token("retry"), {
      claimInputRoute: async (claim) => ++retryCall === 1
        ? routeResult(claim, { reason: "stale_route_revision", latestRevision: 4n })
        : routeResult(claim, { accepted: true, inputRouteEpoch: "retry-route" }),
    });
    retryRouter.hold("s2");
    expect(await retryRouter.claim("s2", retry)).toEqual({
      accepted: true,
      inputRouteEpoch: "retry-route",
      revision: 5n,
    });
    expect(retry.claims.map((claim) => claim.revision)).toEqual([1n, 5n]);
    expect(retry.claims[0]?.requestId).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i);
    expect(retry.claims[0]?.requestId).not.toBe(retry.claims[1]?.requestId);
    router.dispose();
    retryRouter.dispose();
  });

  test("closes and refuses a route when its document revision would overflow", async () => {
    const revisions = new Map([["s1", MAX_TERMINAL_INPUT_ROUTE_REVISION]]);
    const router = createTerminalInputRouter(revisions);
    const closed: string[] = [];
    const direct = destination(token("overflow"), {
      claimInputRoute: async (claim) => routeResult(claim, {}),
      close: (reason) => closed.push(reason),
    });
    expect(await router.claim("s1", direct)).toEqual({
      accepted: false,
      unsupported: false,
      reason: "terminal input route revision exhausted",
    });
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
