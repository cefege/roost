import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";

interface TestState { socketGeneration: number; socketId: string; processEpoch: string; domainGeneration: bigint; ready: boolean }
interface TestOneof { case: string; value: Record<string, unknown> }
let state: TestState | null = { socketGeneration: 1, socketId: "socket-1", processEpoch: "epoch-1", domainGeneration: 11n, ready: true };
let controlHandler: ((control: TestOneof, state: TestState) => void) | null = null;
let generationHandler: ((state: TestState | null) => void) | null = null;
const sent: TestOneof[] = [];
let sessions: Record<string, { worker_fp: string }> = {};
let grant: { workerFp: string; workerEpoch: string; inputRouteSupported: boolean } | null = null;
let activeDirect: Record<string, unknown> | null = null;
mock.module("../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => state,
  sendSyncV2Command: (value: TestOneof) => { sent.push(value); return state?.ready === true; },
  registerSyncV2ControlHandler: (handler: (control: TestOneof, state: TestState) => void) => { controlHandler = handler; return () => { if (controlHandler === handler) controlHandler = null; }; },
  registerSyncV2GenerationHandler: (handler: (state: TestState | null) => void) => { generationHandler = handler; handler(state); return () => { if (generationHandler === handler) generationHandler = null; }; },
}));
mock.module("../src/store/root.ts", () => ({ rootStore: { get sessions() { return sessions; } } }));
mock.module("../src/store/transport/local-terminal-grants.ts", () => ({
  currentTerminalGrant: (workerFp: string) => grant?.workerFp === workerFp ? grant : null,
  resetTerminalGrants: () => { grant = null; },
}));
mock.module("../src/store/terminal-stream-transport.ts", () => ({
  terminalDirectRegistry: { activeForSession: () => activeDirect, reset: () => { activeDirect = null; } },
}));
// Mocks must precede singleton registration; this intentionally tests that load boundary.
const outbound = await import("../src/store/transport/sync-outbound.ts");
await Promise.resolve();
const inputRouter = await import("../src/store/transport/terminal-input-router.ts");
function emit(control: TestOneof): void { if (!state || !controlHandler) throw new Error("input control handler unavailable"); controlHandler(control, state); }
beforeEach(() => {
  vi.useFakeTimers(); outbound._resetTerminalOutboundForTest(); sent.length = 0;
  sessions = {};
  grant = null;
  activeDirect = null;
  state = { socketGeneration: 1, socketId: "socket-1", processEpoch: "epoch-1", domainGeneration: 11n, ready: true };
  generationHandler?.(state);
});
afterEach(() => { outbound._resetTerminalOutboundForTest(); vi.useRealTimers(); });
describe("Sync v2 terminal input outbound", () => {
  test("attributes mounted input while headless input remains admitted", async () => {
    const mounted = outbound.sendTerminalInput("s1", new TextEncoder().encode("abc"), "view-1");
    const headless = outbound.sendTerminalInput("s2", new TextEncoder().encode("x"));
    expect(mounted.accepted && headless.accepted).toBe(true);
    expect(sent[0]?.value).toMatchObject({ sessionId: "s1", viewId: "view-1" });
    expect(sent[1]?.value).toMatchObject({ sessionId: "s2" });
    expect("viewId" in (sent[1]?.value ?? {})).toBe(false);
    if (!mounted.accepted || !headless.accepted) throw new Error("input admission failed");
    for (const [admission, sessionId, writtenBytes] of [[mounted, "s1", 3], [headless, "s2", 1]] as const) emit({ case: "inputAccepted", value: { sessionId, inputSeq: admission.inputSeq, writtenBytes, domainGeneration: 11n } });
    expect((await mounted.result).status).toBe("accepted");
    expect((await headless.result).status).toBe("accepted");
  });
  test("never replays input after its socket closes", async () => {
    const admission = outbound.sendTerminalInput("s1", new Uint8Array([1]), "view-1");
    if (!admission.accepted) throw new Error(admission.reason);
    state = null; generationHandler?.(null);
    expect((await admission.result).status).toBe("ambiguous");
    state = { socketGeneration: 2, socketId: "socket-2", processEpoch: "epoch-1", domainGeneration: 12n, ready: true };
    generationHandler?.(state); expect(sent).toHaveLength(1);
  });
  test("a terminal domain reset on a live socket keeps an in-flight batch and settles it from the late result", async () => {
    const admission = outbound.sendTerminalInput("s1", new Uint8Array([1]), "view-1");
    if (!admission.accepted) throw new Error(admission.reason);
    state = { socketGeneration: 1, socketId: "socket-1", processEpoch: "epoch-1", domainGeneration: 12n, ready: true };
    generationHandler?.(state);
    emit({ case: "inputAccepted", value: { sessionId: "s1", inputSeq: admission.inputSeq, writtenBytes: 1, domainGeneration: 11n } });
    expect((await admission.result).status).toBe("accepted");
    expect(sent).toHaveLength(1);
  });
  test("a terminal domain reset rejects a batch that was never sent", async () => {
    state = { socketGeneration: 1, socketId: "socket-1", processEpoch: "epoch-1", domainGeneration: 11n, ready: false };
    const admission = outbound.sendTerminalInput("s1", new Uint8Array([1]), "view-1");
    if (!admission.accepted) throw new Error(admission.reason);
    expect(sent).toHaveLength(0);
    state = { socketGeneration: 1, socketId: "socket-1", processEpoch: "epoch-1", domainGeneration: 12n, ready: true };
    generationHandler?.(state);
    expect((await admission.result).status).toBe("rejected");
    expect(sent).toHaveLength(0);
  });
  test("assigns distinct local correlations to complete FIFO batches", async () => {
    const first = outbound.sendTerminalInput("s1", new Uint8Array([1, 2]), "view-a");
    const second = outbound.sendTerminalInput("s1", new Uint8Array([3, 4]), "view-b");
    if (!first.accepted || !second.accepted) throw new Error("input admission failed");
    expect(first.inputSeq).not.toBe(second.inputSeq);
    for (const admission of [first, second]) emit({ case: "inputAccepted", value: { sessionId: "s1", inputSeq: admission.inputSeq, writtenBytes: 2, domainGeneration: 11n } });
    expect((await first.result).status).toBe("accepted"); expect((await second.result).status).toBe("accepted");
  });
  test("routes an acknowledged Sync input-route claim through its original socket control lane", async () => {
    sessions = { s1: { worker_fp: "worker-a" } };
    grant = { workerFp: "worker-a", workerEpoch: "worker-epoch", inputRouteSupported: true };
    const destination = outbound.terminalInputDestinationForSession("s1");
    if (!destination) throw new Error("Sync destination unavailable");
    const release = inputRouter.holdTerminalInput("s1");
    const claim = inputRouter.claimTerminalInputRoute("s1", destination);
    const sentClaim = sent[0]?.value as Record<string, unknown> | undefined;
    expect(sent[0]?.case).toBe("inputRouteClaim");
    expect(sentClaim?.workerEpoch).toBe("worker-epoch");
    const requestId = sentClaim?.requestId;
    if (typeof requestId !== "string") throw new Error("route claim lacked request ID");
    emit({
      case: "inputRouteResult",
      value: {
        requestId,
        sessionId: "s1",
        revision: 1n,
        accepted: true,
        latestRevision: 1n,
        inputRouteEpoch: "route-epoch",
        workerEpoch: "worker-epoch",
        reason: "",
      },
    });
    expect(await claim).toEqual({ accepted: true, inputRouteEpoch: "route-epoch", revision: 1n });
    release.release(destination);
  });
  test("keeps claim-required input blocked while Sync capability is unavailable", async () => {
    sessions = { s1: { worker_fp: "worker-a" } };
    grant = { workerFp: "worker-a", workerEpoch: "worker-epoch", inputRouteSupported: true };
    const supported = outbound.terminalInputDestinationForSession("s1");
    if (!supported) throw new Error("Sync destination unavailable");
    void inputRouter.claimTerminalInputRoute("s1", supported);
    const blocked = inputRouter.holdTerminalInput("s1");
    blocked.release();
    grant = { workerFp: "worker-a", workerEpoch: "worker-epoch", inputRouteSupported: false };
    generationHandler?.(state);
    await Promise.resolve();
    expect(inputRouter.terminalInputPhase("s1")).toBe("blocked");
    expect(outbound.sendTerminalInput("s1", new Uint8Array([1])).accepted).toBe(false);
  });
  test("does not reclaim Sync ownership while a direct route remains elected", async () => {
    sessions = { s1: { worker_fp: "worker-a" } };
    grant = { workerFp: "worker-a", workerEpoch: "worker-epoch", inputRouteSupported: true };
    const supported = outbound.terminalInputDestinationForSession("s1");
    if (!supported) throw new Error("Sync destination unavailable");
    void inputRouter.claimTerminalInputRoute("s1", supported);
    const blocked = inputRouter.holdTerminalInput("s1");
    blocked.release();
    activeDirect = {
      kind: "loopback",
      workerFp: "worker-a",
      workerEpoch: "worker-epoch",
      connectionId: "loopback",
      inputRouteSupported: true,
      token: () => ({
        socketGeneration: 1,
        socketId: "loopback",
        processEpoch: "worker-epoch",
        domainGeneration: 1n,
        transportKind: "loopback",
        workerFp: "worker-a",
      }),
      allowsSession: () => true,
      sendInput: () => "accepted",
      claimInputRoute: async () => { throw new Error("unused"); },
    };
    const sentBeforeRecovery = sent.length;
    generationHandler?.(state);
    await Promise.resolve();
    expect(sent).toHaveLength(sentBeforeRecovery);
    expect(inputRouter.terminalInputPhase("s1")).toBe("blocked");
  });
  test("blocks resumed WebRTC input until a fresh worker probe succeeds", () => {
    let qualified = false;
    const connection = {
      kind: "webrtc",
      workerFp: "worker-a",
      workerEpoch: "worker-epoch",
      connectionId: "peer-connection",
      inputRouteSupported: true,
      token: () => ({
        socketGeneration: 1,
        socketId: "peer-socket",
        processEpoch: "worker-epoch",
        domainGeneration: 1n,
        transportKind: "webrtc",
        workerFp: "worker-a",
      }),
      telemetry: () => ({
        opaquePeerId: "peer-id",
        lastProbeAtMs: qualified ? 1 : null,
        rttMs: qualified ? 1 : null,
        livenessQualified: qualified,
        bufferedBytes: 0,
      }),
      sendInput: () => "accepted",
      claimInputRoute: async () => { throw new Error("unused"); },
      close: () => undefined,
    };
    expect(outbound.terminalInputDestinationForDirectConnection(connection as never)).toBeNull();
    expect(outbound.terminalInputDestinationForDirectConnection(connection as never, true)).not.toBeNull();
    qualified = true;
    expect(outbound.terminalInputDestinationForDirectConnection(connection as never)).not.toBeNull();
  });
});
