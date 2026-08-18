// Downstream terminal-control admission on CoordLink.
//
// The coordinator writes a RELATIVE `budget_ms` with every input/viewport
// request. CoordLink turns it into a live monotonic budget anchored at frame
// receipt, admits the two kinds against independent bounded slots, and answers
// with a typed phase whenever it refuses or the handler dies. What this pins:
//
//   * the budget counts real elapsed time and ignores the wall clock, so no
//     amount of coordinator/worker clock skew moves an expiry;
//   * a viewport handler parked on a keeper resize neither delays nor
//     duplicates the input frame behind it;
//   * over-cap admission fails closed with REJECTED + PRE_WRITE — provably no
//     mutation, safe to retry — instead of a silent drop;
//   * a thrown handler is AMBIGUOUS + UNKNOWN: it can never be presented as
//     unsent, because that is what would license a duplicate write.

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// ClientSeq is constructed inside startCoordLink; isolate its durable counter
// before importing/starting the link so this test cannot touch worker state.
process.env.ROOST_WORKER_DATA_DIR = mkdtempSync(join(tmpdir(), "coordlink-budget-test-"));

import { afterEach, expect, test, vi } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  DHelloAckSchema,
  DInputRequestSchema,
  DViewportRequestSchema,
  TerminalInputStatus,
  TerminalViewportStatus,
  TerminalWritePhase,
  type WInputResult,
  type WViewportResult,
} from "@roost/shared/proto/worker_transport_pb";
import type { WorkerFp } from "@roost/shared/wire";
import { startCoordLink, type CoordLink } from "../src/transport/CoordLink.ts";
import type { TerminalRequestBudget } from "../src/transport/CoordLink-types.ts";
import {
  INPUT_REQUEST_INFLIGHT_CAP,
  TERMINAL_REQUEST_BUDGET_CAP_MS,
} from "../src/transport/CoordLink-constants.ts";

const SESSION_ID = "00000000-0000-4000-8000-0000000000aa";

function helloAckBytes(): Uint8Array {
  return toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: { case: "helloAck", value: create(DHelloAckSchema, {
      coordPubkeyB64: "test-key", coordPubkeyKid: "test-kid",
    }) },
  }));
}

function inputRequestBytes(requestId: string, budgetMs: number): Uint8Array {
  return toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: { case: "inputRequest", value: create(DInputRequestSchema, {
      requestId, sessionId: SESSION_ID, inputSeq: 1n,
      data: Uint8Array.of(0x6c, 0x73), budgetMs,
    }) },
  }));
}

function viewportRequestBytes(requestId: string, budgetMs: number): Uint8Array {
  return toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
    frame: { case: "viewportRequest", value: create(DViewportRequestSchema, {
      requestId, sessionId: SESSION_ID, viewerId: "fp:tab", clientSeq: 1n,
      cols: 80, rows: 24, cause: 1, heldCellSeq: 0n, budgetMs,
    }) },
  }));
}

class ControlledWebSocket {
  binaryType = "blob";
  bufferedAmount = 0;
  readyState: number = WebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(private readonly onSend: (bytes: Uint8Array) => void) {}

  open(): void {
    this.readyState = WebSocket.OPEN;
    this.onopen?.();
  }

  receive(bytes: Uint8Array): void {
    this.onmessage?.({ data: Uint8Array.from(bytes).buffer });
  }

  send(data: unknown): void {
    const bytes = data instanceof ArrayBuffer
      ? new Uint8Array(data)
      : data instanceof Uint8Array
        ? data
        : null;
    if (!bytes) throw new Error("unexpected fake WebSocket payload");
    this.onSend(Uint8Array.from(bytes));
  }

  close(): void {
    if (this.readyState === WebSocket.CLOSED) return;
    this.readyState = WebSocket.CLOSED;
    this.onclose?.();
  }
}

/** Await the frame the link actually emitted instead of a guessed delay. */
class ResultStream<T> {
  readonly all: T[] = [];
  private cursor = 0;
  private waiters: Array<() => void> = [];

  push(value: T): void {
    this.all.push(value);
    const woken = this.waiters;
    this.waiters = [];
    for (const wake of woken) wake();
  }

  async next(): Promise<T> {
    while (this.cursor >= this.all.length) {
      await new Promise<void>((resolve) => this.waiters.push(resolve));
    }
    return this.all[this.cursor++]!;
  }
}

interface Harness {
  link: CoordLink;
  socket: ControlledWebSocket;
  inputResults: ResultStream<WInputResult>;
  viewportResults: ResultStream<WViewportResult>;
}

let activeLink: CoordLink | null = null;

afterEach(() => {
  activeLink?.dispose();
  activeLink = null;
  vi.useRealTimers();
});

async function startHarness(handlers: {
  onInputRequest?: (budget: TerminalRequestBudget) => Promise<void> | void;
  onViewportRequest?: (budget: TerminalRequestBudget) => Promise<void> | void;
}): Promise<Harness> {
  const socketCreated = Promise.withResolvers<ControlledWebSocket>();
  const opened = Promise.withResolvers<void>();
  const inputResults = new ResultStream<WInputResult>();
  const viewportResults = new ResultStream<WViewportResult>();

  const link = startCoordLink({
    coordHttpUrl: "http://coord.test:4102",
    workerFp: "budget-fp" as WorkerFp,
    workerVersion: "test",
    mintJwt: async () => "jwt",
    webSocketFactory: () => {
      const controlled = new ControlledWebSocket((bytes) => {
        const frame = fromBinary(CoordWorkerUpSchema, bytes);
        if (frame.frame.case === "inputResult") inputResults.push(frame.frame.value);
        if (frame.frame.case === "viewportResult") viewportResults.push(frame.frame.value);
      });
      socketCreated.resolve(controlled);
      return controlled as unknown as WebSocket;
    },
    onOpen: () => opened.resolve(),
    onInputRequest: (_request, budget) => handlers.onInputRequest?.(budget),
    onViewportRequest: (_request, budget) => handlers.onViewportRequest?.(budget),
  });
  activeLink = link;

  const socket = await socketCreated.promise;
  socket.open();
  await opened.promise;
  socket.receive(helloAckBytes());
  return { link, socket, inputResults, viewportResults };
}

test("the request budget is monotonic and immune to wall-clock skew", async () => {
  vi.useFakeTimers();
  const captured = Promise.withResolvers<TerminalRequestBudget>();
  const harness = await startHarness({
    onInputRequest: (budget) => { captured.resolve(budget); },
  });

  harness.socket.receive(inputRequestBytes("req-budget", 200));
  const budget = await captured.promise;
  expect(budget.remainingMs()).toBe(200);

  // Step the wall clock a day forward while the request waits for its
  // keeper-admission turn. A budget derived from Date.now() would either
  // expire instantly or never expire; a monotonic one just keeps counting.
  const realNow = Date.now;
  Date.now = () => realNow() + 86_400_000;
  try {
    vi.advanceTimersByTime(30);
    expect(budget.remainingMs()).toBe(170);
    Date.now = () => realNow() - 86_400_000;
    vi.advanceTimersByTime(70);
    expect(budget.remainingMs()).toBe(100);
  } finally {
    Date.now = realNow;
  }
  expect(budget.isCurrentConnection()).toBe(true);
});

test("an expired queue wait is visible as a non-positive budget before the write", async () => {
  vi.useFakeTimers();
  const captured = Promise.withResolvers<TerminalRequestBudget>();
  const harness = await startHarness({
    onInputRequest: (budget) => { captured.resolve(budget); },
  });

  harness.socket.receive(inputRequestBytes("req-expired", 15));
  const budget = await captured.promise;
  // A 15ms budget cannot survive a 40ms admission wait, so the keeper write is
  // gated off a provably pre-write expiry rather than a guess.
  vi.advanceTimersByTime(40);
  expect(budget.remainingMs()).toBeLessThanOrEqual(0);
});

test("a missing budget falls back to the bounded ceiling rather than running unbounded", async () => {
  vi.useFakeTimers();
  const captured = Promise.withResolvers<TerminalRequestBudget>();
  const harness = await startHarness({
    onInputRequest: (budget) => { captured.resolve(budget); },
  });

  harness.socket.receive(inputRequestBytes("req-nobudget", 0));
  const budget = await captured.promise;
  expect(budget.remainingMs()).toBe(TERMINAL_REQUEST_BUDGET_CAP_MS);
});

test("an oversized budget is clamped to the worker's own ceiling", async () => {
  vi.useFakeTimers();
  const captured = Promise.withResolvers<TerminalRequestBudget>();
  const harness = await startHarness({
    onInputRequest: (budget) => { captured.resolve(budget); },
  });

  harness.socket.receive(inputRequestBytes("req-huge", TERMINAL_REQUEST_BUDGET_CAP_MS * 10));
  const budget = await captured.promise;
  expect(budget.remainingMs()).toBe(TERMINAL_REQUEST_BUDGET_CAP_MS);
});

test("a parked viewport neither blocks nor duplicates the input frame behind it", async () => {
  vi.useFakeTimers();
  const order: string[] = [];
  let inputInvocations = 0;
  const inputSeen = Promise.withResolvers<void>();
  const harness = await startHarness({
    // Never settles: the viewport is stuck on a keeper resize for its budget.
    onViewportRequest: () => {
      order.push("viewport");
      return new Promise<void>(() => undefined);
    },
    onInputRequest: () => {
      order.push("input");
      inputInvocations += 1;
      inputSeen.resolve();
    },
  });

  harness.socket.receive(viewportRequestBytes("req-parked", 5_000));
  harness.socket.receive(inputRequestBytes("req-behind", 5_000));
  await inputSeen.promise;

  // Arrival order into the handlers is preserved, but the parked viewport
  // never gated the input dispatch behind it.
  expect(order).toEqual(["viewport", "input"]);
  expect(inputInvocations).toBe(1);
  // Nothing was fabricated for the viewport while its outcome is genuinely
  // unknown, and no second input was manufactured by the stall.
  vi.advanceTimersByTime(10_000);
  expect(harness.viewportResults.all).toHaveLength(0);
  expect(inputInvocations).toBe(1);
});

test("over-cap input admission fails closed with a provable pre-write rejection", async () => {
  vi.useFakeTimers();
  let invocations = 0;
  const harness = await startHarness({
    onInputRequest: () => {
      invocations += 1;
      return new Promise<void>(() => undefined);
    },
  });

  for (let i = 0; i < INPUT_REQUEST_INFLIGHT_CAP; i += 1) {
    harness.socket.receive(inputRequestBytes(`req-fill-${i}`, 5_000));
  }
  expect(invocations).toBe(INPUT_REQUEST_INFLIGHT_CAP);
  expect(harness.inputResults.all).toHaveLength(0);

  harness.socket.receive(inputRequestBytes("req-overflow", 5_000));
  // Refused without reaching the session manager, so the coordinator may
  // reject definitely and the browser may retry with no duplicate risk.
  expect(invocations).toBe(INPUT_REQUEST_INFLIGHT_CAP);
  const refused = await harness.inputResults.next();
  expect(refused.requestId).toBe("req-overflow");
  expect(refused.status).toBe(TerminalInputStatus.REJECTED);
  expect(refused.phase).toBe(TerminalWritePhase.PRE_WRITE);
  expect(refused.writtenBytes).toBe(0);

  // A viewport still admits: the two kinds hold independent slots, so a
  // saturated input lane cannot starve resize and vice versa.
  harness.socket.receive(viewportRequestBytes("req-viewport-ok", 5_000));
  expect(harness.viewportResults.all).toHaveLength(0);
});

test("a thrown handler is reported unknown, never as an unsent request", async () => {
  vi.useFakeTimers();
  const harness = await startHarness({
    // Synchronous throw: it must still produce a typed result rather than
    // escaping through ws.onmessage and stranding the correlation.
    onInputRequest: () => { throw new Error("keeper pool exploded mid-write"); },
    onViewportRequest: async () => { throw new Error("resize reconciliation threw"); },
  });

  harness.socket.receive(inputRequestBytes("req-throw", 5_000));
  harness.socket.receive(viewportRequestBytes("req-throw-viewport", 5_000));

  const input = await harness.inputResults.next();
  expect(input.requestId).toBe("req-throw");
  expect(input.status).toBe(TerminalInputStatus.AMBIGUOUS);
  expect(input.phase).toBe(TerminalWritePhase.UNKNOWN);
  expect(input.reason).toBe("keeper pool exploded mid-write");

  const viewport = await harness.viewportResults.next();
  expect(viewport.requestId).toBe("req-throw-viewport");
  expect(viewport.status).toBe(TerminalViewportStatus.AMBIGUOUS);
  expect(viewport.phase).toBe(TerminalWritePhase.UNKNOWN);
  expect(viewport.reason).toBe("resize reconciliation threw");
});

test("a superseded socket is reported as no longer current", async () => {
  vi.useFakeTimers();
  const captured = Promise.withResolvers<TerminalRequestBudget>();
  const harness = await startHarness({
    onInputRequest: (budget) => { captured.resolve(budget); },
  });

  harness.socket.receive(inputRequestBytes("req-generation", 5_000));
  const budget = await captured.promise;
  expect(budget.isCurrentConnection()).toBe(true);

  harness.socket.close();
  // The socket that carried this request is gone, so its result can no longer
  // reach the coordinator that asked for it.
  expect(budget.isCurrentConnection()).toBe(false);
});
