import { beforeEach, describe, expect, mock, test } from "bun:test";

interface TestState {
  socketGeneration: number;
  socketId: string;
  processEpoch: string;
  domainGeneration: bigint;
  ready: boolean;
}

interface TestOneof {
  case: string;
  value: Record<string, unknown>;
}

let state: TestState | null = {
  socketGeneration: 1,
  socketId: "socket-1",
  processEpoch: "epoch-1",
  domainGeneration: 11n,
  ready: true,
};
let controlHandler: ((control: TestOneof, state: TestState) => void) | null = null;
let generationHandler: ((state: TestState | null) => void) | null = null;
const sent: TestOneof[] = [];

mock.module("../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => state,
  sendSyncV2Command: (value: TestOneof) => {
    sent.push(value);
    return true;
  },
  registerSyncV2ControlHandler: (handler: (control: TestOneof, state: TestState) => void) => {
    controlHandler = handler;
    return () => { if (controlHandler === handler) controlHandler = null; };
  },
  registerSyncV2GenerationHandler: (handler: (state: TestState | null) => void) => {
    generationHandler = handler;
    handler(state);
    return () => { if (generationHandler === handler) generationHandler = null; };
  },
}));

mock.module("../src/lib/diag.ts", () => ({
  getSessionTraceId: () => "trace",
  markPhase: () => undefined,
}));

// Intentional module-loading boundary: Bun must install the transport mocks
// before the singleton outbound module registers its generation handlers.

const outbound = await import("../src/ws/sync-outbound.ts");

function emitControl(control: TestOneof): void {
  if (!state || !controlHandler) throw new Error("test Sync control handler is unavailable");
  controlHandler(control, state);
}

function emitGeneration(next: TestState | null): void {
  state = next;
  generationHandler?.(next);
}

beforeEach(() => {
  outbound._resetTerminalOutboundForTest();
  sent.length = 0;
  state = {
    socketGeneration: 1,
    socketId: "socket-1",
    processEpoch: "epoch-1",
    domainGeneration: 11n,
    ready: true,
  };
  generationHandler?.(state);
});

describe("Sync v2 terminal outbound", () => {
  test("never replays an input batch after its socket generation closes", async () => {
    const admission = outbound.sendTerminalInput("s1", new TextEncoder().encode("x"));
    expect(admission.accepted).toBe(true);
    if (!admission.accepted) throw new Error(admission.reason);
    expect(sent.map((frame) => frame.case)).toEqual(["input"]);

    emitGeneration(null);
    expect((await admission.result).status).toBe("ambiguous");
    emitGeneration({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "epoch-1",
      domainGeneration: 12n,
      ready: true,
    });
    expect(sent.map((frame) => frame.case)).toEqual(["input"]);
  });

  test("accepts input only when the keeper-completed byte count is exact", async () => {
    const admission = outbound.sendTerminalInput("s1", new TextEncoder().encode("abc"));
    if (!admission.accepted) throw new Error(admission.reason);
    emitControl({
      case: "inputAccepted",
      value: {
        sessionId: "s1",
        inputSeq: admission.inputSeq,
        domainGeneration: 11n,
        writtenBytes: 2,
      },
    });
    const result = await admission.result;
    expect(result.status).toBe("ambiguous");
    expect(result.writtenBytes).toBe(2);
  });

  test("rejects an oversized atomic batch before writing any command", () => {
    const admission = outbound.sendTerminalInput("s1", new Uint8Array(64 * 1024 + 1));
    expect(admission.accepted).toBe(false);
    expect(sent).toHaveLength(0);
  });

  test("replays the latest viewport intent on a new ready generation and ignores stale acceptance", async () => {
    const admission = outbound.sendTerminalViewportIntent("s1", {
      cols: 120,
      rows: 40,
      cause: 1,
      heldCellSeq: 9n,
    });
    expect(sent).toHaveLength(1);
    emitGeneration(null);
    emitGeneration({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "epoch-1",
      domainGeneration: 12n,
      ready: true,
    });
    expect(sent).toHaveLength(2);
    expect(sent[1]?.value.clientSeq).toBe(admission.sequence);

    emitControl({
      case: "viewportAccepted",
      value: {
        sessionId: "s1",
        clientSeq: admission.sequence,
        domainGeneration: 11n,
        effectiveCols: 120,
        effectiveRows: 40,
        channelResizeSeq: 1n,
      },
    });
    let settled = false;
    void admission.result.then(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);

    emitControl({
      case: "viewportAccepted",
      value: {
        sessionId: "s1",
        clientSeq: admission.sequence,
        domainGeneration: 12n,
        effectiveCols: 100,
        effectiveRows: 35,
        channelResizeSeq: 2n,
      },
    });
    expect((await admission.result).status).toBe("accepted");
  });

  test("sends same-sequence heartbeats instead of suppressing them", () => {
    const claim = outbound.sendTerminalViewportIntent("s1", {
      cols: 100,
      rows: 30,
      cause: 1,
    });
    outbound.sendTerminalViewportIntent("s1", {
      cols: 100,
      rows: 30,
      cause: 6,
      heldCellSeq: 20n,
    });
    expect(sent).toHaveLength(2);
    expect(sent[0]?.value.clientSeq).toBe(claim.sequence);
    expect(sent[1]?.value.clientSeq).toBe(claim.sequence);
  });

  test("suppresses only the equivalent initial claim seeded by optimistic spawn", async () => {
    outbound.seedTerminalViewportIntent("s1", 7n, 90, 24, 1);
    const equivalent = outbound.sendTerminalViewportIntent("s1", {
      cols: 90,
      rows: 24,
      cause: 1,
    });
    expect(equivalent.sequence).toBe(7n);
    expect(sent).toHaveLength(0);
    expect((await equivalent.result).status).toBe("accepted");

    const changed = outbound.sendTerminalViewportIntent("s1", {
      cols: 91,
      rows: 24,
      cause: 2,
    });
    expect(changed.sequence).toBe(8n);
    expect(sent).toHaveLength(1);
  });
});
