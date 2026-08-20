import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import { registerViewportRetryCases } from "./syncOutboundRebase.cases.ts";

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

const VIEWPORT_STORAGE_KEY = "roost.sync-v2.viewport-intents";
const sessionStorageValues = new Map<string, string>();
Object.defineProperty(globalThis, "sessionStorage", {
  configurable: true,
  value: {
    get length() { return sessionStorageValues.size; },
    clear: () => sessionStorageValues.clear(),
    getItem: (key: string) => sessionStorageValues.get(key) ?? null,
    key: () => null,
    removeItem: (key: string) => { sessionStorageValues.delete(key); },
    setItem: (key: string, value: string) => { sessionStorageValues.set(key, value); },
  } as Storage,
});

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

function viewportCommands(): TestOneof[] {
  return sent.filter((frame) => frame.case === "viewport");
}

function persistedViewportSequence(sessionId: string): string | undefined {
  const records = JSON.parse(sessionStorageValues.get(VIEWPORT_STORAGE_KEY) ?? "[]") as Array<{
    sessionId: string;
    sequence: string;
  }>;
  return records.find((record) => record.sessionId === sessionId)?.sequence;
}

function acceptViewport(clientSeq: bigint, domainGeneration = state?.domainGeneration ?? 0n): void {
  emitControl({
    case: "viewportAccepted",
    value: {
      sessionId: "s1",
      clientSeq,
      domainGeneration,
      effectiveCols: 120,
      effectiveRows: 40,
      channelResizeSeq: clientSeq,
    },
  });
}

function failViewport(
  controlCase: "viewportRejected" | "viewportAmbiguous",
  clientSeq: bigint,
  reason = "injected failure",
  options: { sequenceFloor?: bigint; domainGeneration?: bigint } = {},
): void {
  emitControl({
    case: controlCase,
    value: {
      sessionId: "s1",
      clientSeq,
      domainGeneration: options.domainGeneration ?? state?.domainGeneration ?? 0n,
      reason,
      ...(options.sequenceFloor === undefined ? {} : { sequenceFloor: options.sequenceFloor }),
    },
  });
}

beforeEach(() => {
  vi.useFakeTimers();
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

afterEach(() => {
  outbound._resetTerminalOutboundForTest();
  vi.useRealTimers();
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

  registerViewportRetryCases({
    claim: () => outbound.acquireTerminalViewportOwner("s1")
      .claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 9n }),
    sequences: () => viewportCommands().map((command) => command.value.clientSeq as bigint),
    fail: (kind, sequence, reason, sequenceFloor) =>
      failViewport(kind, sequence, reason, { sequenceFloor }),
    accept: acceptViewport,
    snapshot: () => outbound.terminalOutboundSnapshot("s1").claim,
    sequenceFloor: () => outbound.terminalOutboundSnapshot("s1").claim.sequence_floor,
    persistedSequence: () => persistedViewportSequence("s1"),
  });

  test("turns a lost result into a retry only after the ten-second result deadline", () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 1n });
    const firstSequence = viewportCommands()[0]?.value.clientSeq as bigint;
    expect(admission.sequence).toBe(firstSequence);

    vi.advanceTimersByTime(9_999);
    expect(viewportCommands()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("retrying");
    vi.advanceTimersByTime(249);
    expect(viewportCommands()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(viewportCommands()).toHaveLength(2);
    expect(viewportCommands()[1]?.value.clientSeq).toBeGreaterThan(firstSequence);
  });

  test("keeps a six-second acceptance current under the browser's ten-second deadline", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 1n });
    vi.advanceTimersByTime(6_000);

    acceptViewport(admission.sequence);
    expect((await admission.result).status).toBe("accepted");
    vi.advanceTimersByTime(4_000);
    expect(viewportCommands()).toHaveLength(1);
    expect(outbound.terminalOutboundSnapshot("s1").claim.confirmed?.client_seq)
      .toBe(admission.sequence.toString());
  });

  test("starts a retry when an accepted repair lacks a full frame for three seconds", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({
      cols: 120,
      rows: 40,
      cause: 3,
      repairRequired: true,
    });
    acceptViewport(admission.sequence);
    let settled = false;
    void admission.result.then(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);

    vi.advanceTimersByTime(2_999);
    expect(viewportCommands()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("retrying");
    vi.advanceTimersByTime(249);
    expect(viewportCommands()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(viewportCommands()).toHaveLength(2);
    expect(viewportCommands()[1]?.value.clientSeq).toBeGreaterThan(admission.sequence);
  });

  test("accepts a qualifying full frame that arrives before the control result", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({
      cols: 120,
      rows: 40,
      cause: 3,
      repairRequired: true,
    });
    owner.noteFullFrame({ seq: 21, gridEpoch: "grid-a" });
    acceptViewport(admission.sequence);

    expect((await admission.result).status).toBe("accepted");
    vi.advanceTimersByTime(10_000);
    expect(viewportCommands()).toHaveLength(1);
  });

  test("requires a full frame to advance the attempt's dispatch receipt floor", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    owner.noteFullFrame({ seq: 20, gridEpoch: "grid-a" });
    const admission = owner.claim({
      cols: 120,
      rows: 40,
      cause: 3,
      repairRequired: true,
    });
    acceptViewport(admission.sequence);
    owner.noteFullFrame({ seq: 20, gridEpoch: "grid-a" });
    let settled = false;
    void admission.result.then(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);

    owner.noteFullFrame({ seq: 21, gridEpoch: "grid-a" });
    expect((await admission.result).status).toBe("accepted");
  });

  test("heartbeat uses pending positive geometry and creates no result deadline", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({ cols: 101, rows: 31, cause: 1, heldCellSeq: 1n });
    expect(owner.heartbeat(20n)).toBeUndefined();
    const frames = viewportCommands();
    expect(frames).toHaveLength(2);
    expect(frames[1]?.value).toMatchObject({
      cols: 101,
      rows: 31,
      cause: 6,
      heldCellSeq: 20n,
      clientSeq: admission.sequence,
    });

    acceptViewport(admission.sequence);
    expect((await admission.result).status).toBe("accepted");
    vi.advanceTimersByTime(10_000);
    expect(viewportCommands()).toHaveLength(2);
  });

  test("fences a late result from the prior socket and domain generation", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 1n });
    const firstSequence = viewportCommands()[0]?.value.clientSeq as bigint;
    emitGeneration(null);
    emitGeneration({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "epoch-1",
      domainGeneration: 12n,
      ready: true,
    });
    const retrySequence = viewportCommands()[1]?.value.clientSeq as bigint;
    expect(retrySequence).toBeGreaterThan(firstSequence);

    acceptViewport(firstSequence, 11n);
    let settled = false;
    void admission.result.then(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);
    expect(outbound.terminalOutboundSnapshot("s1").claim.confirmed).toBeNull();

    acceptViewport(retrySequence, 12n);
    expect(await admission.result).toMatchObject({ status: "accepted", sequence: retrySequence });
  });

  test("coalesces equivalent claims and supersedes changed tuples immediately", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const first = owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 4n });
    const equivalent = owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 4n });
    expect(equivalent.sequence).toBe(first.sequence);
    expect(equivalent.result).toBe(first.result);
    expect(viewportCommands()).toHaveLength(1);

    const changed = owner.claim({ cols: 121, rows: 40, cause: 2, heldCellSeq: 4n });
    expect(viewportCommands()).toHaveLength(2);
    expect(changed.sequence).toBeGreaterThan(first.sequence);
    expect((await first.result).status).toBe("superseded");
    acceptViewport(changed.sequence);
    expect((await changed.result).status).toBe("accepted");
  });

  test("stale owner actions and disposal cannot affect its successor", async () => {
    const firstOwner = outbound.acquireTerminalViewportOwner("s1");
    const first = firstOwner.claim({ cols: 100, rows: 30, cause: 1, heldCellSeq: 1n });
    const successor = outbound.acquireTerminalViewportOwner("s1");
    expect((await first.result).status).toBe("superseded");
    const statuses: string[] = [];
    successor.subscribeStatus((status) => statuses.push(status.status));
    const current = successor.claim({ cols: 120, rows: 40, cause: 2, heldCellSeq: 1n });
    expect(viewportCommands()).toHaveLength(2);

    firstOwner.dispose();
    firstOwner.heartbeat(99n);
    const stale = firstOwner.claim({ cols: 80, rows: 20, cause: 2 });
    expect((await stale.result).status).toBe("superseded");
    expect(viewportCommands()).toHaveLength(2);

    acceptViewport(current.sequence);
    expect((await current.result).status).toBe("accepted");
    expect(statuses).toEqual(["pending", "ready"]);
  });

  test("adopts an equivalent optimistic preclaim and preserves its sequence floor", async () => {
    outbound.seedTerminalViewportIntent("s1", 7n, 90, 24, 1);
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const equivalent = owner.claim({ cols: 90, rows: 24, cause: 1, heldCellSeq: 1n });
    expect(equivalent.sequence).toBe(7n);
    expect(viewportCommands()).toHaveLength(0);
    expect((await equivalent.result).status).toBe("accepted");

    const changed = owner.claim({ cols: 91, rows: 24, cause: 2, heldCellSeq: 1n });
    expect(changed.sequence).toBe(8n);
    expect(viewportCommands()).toHaveLength(1);
  });

  test("reset cancels viewport deadlines and clears truthful diagnostics", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const admission = owner.claim({ cols: 120, rows: 40, cause: 1 });
    expect(outbound.terminalOutboundSnapshot("s1").claim.attempt?.client_seq)
      .toBe(admission.sequence.toString());

    outbound._resetTerminalOutboundForTest();
    expect((await admission.result).status).toBe("rejected");
    vi.advanceTimersByTime(20_000);
    expect(viewportCommands()).toHaveLength(1);
    expect(outbound.terminalOutboundSnapshot("s1").claim).toMatchObject({
      owner_token: null,
      sequence_floor: "0",
      desired: null,
      confirmed: null,
      attempt: null,
      retry: null,
    });
  });

  test("zero held watermark is ready only after a newer authoritative full frame", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const statuses: string[] = [];
    owner.subscribeStatus((status) => statuses.push(status.status));
    const admission = owner.claim({ cols: 120, rows: 40, cause: 1 });

    acceptViewport(admission.sequence);
    let settled = false;
    void admission.result.then(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);
    expect(statuses).toEqual(["pending", "repairing"]);

    owner.noteFullFrame({ seq: 1, gridEpoch: "grid-initial" });
    expect((await admission.result).status).toBe("accepted");
    expect(statuses).toEqual(["pending", "repairing", "ready"]);
  });

  test("producer process replacement advances sequence and requires a fresh full frame", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const statuses: string[] = [];
    owner.subscribeStatus((status) => statuses.push(status.status));
    const first = owner.claim({
      cols: 120,
      rows: 40,
      cause: 1,
      heldCellSeq: 7n,
    });
    acceptViewport(first.sequence);
    expect((await first.result).status).toBe("accepted");

    emitGeneration(null);
    emitGeneration({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "epoch-2",
      domainGeneration: 12n,
      ready: true,
    });
    const repairSequence = viewportCommands().at(-1)?.value.clientSeq as bigint;
    expect(repairSequence).toBeGreaterThan(first.sequence);
    acceptViewport(repairSequence);
    expect(statuses.at(-1)).toBe("repairing");

    owner.noteFullFrame({ seq: 1, gridEpoch: "grid-restarted" });
    expect(statuses.at(-1)).toBe("ready");
    expect(outbound.terminalOutboundSnapshot("s1").claim.confirmed?.client_seq)
      .toBe(repairSequence.toString());
  });

  test("a producing worker generation change replays the active owner with a zero held watermark", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const statuses: string[] = [];
    owner.subscribeStatus((status) => statuses.push(status.status));
    const first = owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 7n });
    acceptViewport(first.sequence);
    expect((await first.result).status).toBe("accepted");
    expect(statuses.at(-1)).toBe("ready");

    // `respawned` / worker reconcile `snapshot`: the same Sync socket and coord
    // process epoch, but a NEW keeper core that holds no claim and none of the
    // frames this tab applied.
    outbound.noteTerminalProducerGeneration(["s1"]);

    const replay = viewportCommands().at(-1)!;
    const replaySequence = replay.value.clientSeq as bigint;
    expect(replaySequence).toBeGreaterThan(first.sequence);
    expect(replay.value.heldCellSeq).toBe(0n);
    expect(replay.value.cols).toBe(120);
    expect(replay.value.rows).toBe(40);
    // The retired producer's acceptance is no longer a confirmation.
    expect(outbound.terminalOutboundSnapshot("s1").claim.confirmed).toBeNull();

    acceptViewport(replaySequence);
    expect(statuses.at(-1)).toBe("repairing");
    owner.noteFullFrame({ seq: 1, gridEpoch: "grid-respawned" });
    expect(statuses.at(-1)).toBe("ready");
  });

  test("a producer generation change supersedes an in-flight attempt at once", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const first = owner.claim({ cols: 100, rows: 30, cause: 1, heldCellSeq: 3n });
    expect(viewportCommands()).toHaveLength(1);

    outbound.noteTerminalProducerGeneration(["s1"]);
    expect(viewportCommands()).toHaveLength(2);
    const replaySequence = viewportCommands()[1]?.value.clientSeq as bigint;
    expect(replaySequence).toBeGreaterThan(first.sequence);
    expect(viewportCommands()[1]?.value.heldCellSeq).toBe(0n);

    // A result for the retired producer's sequence can no longer settle the owner.
    acceptViewport(first.sequence);
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("pending");
    acceptViewport(replaySequence);
    owner.noteFullFrame({ seq: 4, gridEpoch: "grid-respawned" });
    expect((await first.result)).toMatchObject({ status: "accepted", sequence: replaySequence });
  });

  test("a tab with no positive viewport owner replays nothing", () => {
    outbound.noteTerminalProducerGeneration(["s1"]);
    expect(viewportCommands()).toHaveLength(0);

    // A withdrawn 0×0 owner is not a viewer either.
    const owner = outbound.acquireTerminalViewportOwner("s1");
    owner.claim({ cols: 0, rows: 0, cause: 4 });
    const before = viewportCommands().length;
    outbound.noteTerminalProducerGeneration(["s1"]);
    expect(viewportCommands()).toHaveLength(before);
  });

  test("a producer change while Sync is down arms the repair for the next generation", () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    const first = owner.claim({ cols: 80, rows: 24, cause: 1, heldCellSeq: 5n });
    acceptViewport(first.sequence);
    emitGeneration(null);

    outbound.noteTerminalProducerGeneration(["s1"]);
    expect(viewportCommands()).toHaveLength(1);
    expect(outbound.terminalOutboundSnapshot("s1").claim.desired?.held_cell_seq).toBe("0");
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("pending");

    emitGeneration({
      socketGeneration: 2,
      socketId: "socket-2",
      processEpoch: "epoch-1",
      domainGeneration: 12n,
      ready: true,
    });
    const replay = viewportCommands().at(-1)!;
    expect(viewportCommands()).toHaveLength(2);
    expect(replay.value.heldCellSeq).toBe(0n);
    expect(replay.value.clientSeq as bigint).toBeGreaterThan(first.sequence);
  });

  test("optimistic preclaim accepts a full received before owner adoption", async () => {
    const owner = outbound.acquireTerminalViewportOwner("s1");
    owner.noteFullFrame({ seq: 1, gridEpoch: "spawn-grid" });
    outbound.seedTerminalViewportIntent("s1", 7n, 90, 24, 1);

    const admission = owner.claim({ cols: 90, rows: 24, cause: 1 });
    expect(viewportCommands()).toHaveLength(0);
    expect((await admission.result).status).toBe("accepted");
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("ready");
  });

  test("optimistic preclaim missing its full frame enters the three-second repair path", () => {
    outbound.seedTerminalViewportIntent("s1", 7n, 90, 24, 1);
    const owner = outbound.acquireTerminalViewportOwner("s1");
    owner.claim({ cols: 90, rows: 24, cause: 1 });
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("repairing");

    vi.advanceTimersByTime(2_999);
    expect(viewportCommands()).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(outbound.terminalOutboundSnapshot("s1").claim.status).toBe("retrying");
    vi.advanceTimersByTime(249);
    expect(viewportCommands()).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(viewportCommands()).toHaveLength(1);
    expect(viewportCommands()[0]?.value.clientSeq).toBeGreaterThan(7n);
  });
});
