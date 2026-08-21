import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";

interface TestState { socketGeneration: number; socketId: string; processEpoch: string; domainGeneration: bigint; ready: boolean }
interface TestOneof { case: string; value: Record<string, unknown> }
let state: TestState | null = { socketGeneration: 1, socketId: "socket-1", processEpoch: "epoch-1", domainGeneration: 11n, ready: true };
let controlHandler: ((control: TestOneof, state: TestState) => void) | null = null;
let generationHandler: ((state: TestState | null) => void) | null = null;
const sent: TestOneof[] = [];
mock.module("../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => state,
  sendSyncV2Command: (value: TestOneof) => { sent.push(value); return state?.ready === true; },
  registerSyncV2ControlHandler: (handler: (control: TestOneof, state: TestState) => void) => { controlHandler = handler; return () => { if (controlHandler === handler) controlHandler = null; }; },
  registerSyncV2GenerationHandler: (handler: (state: TestState | null) => void) => { generationHandler = handler; handler(state); return () => { if (generationHandler === handler) generationHandler = null; }; },
}));
mock.module("../src/lib/diag.ts", () => ({ getSessionTraceId: () => "trace" }));
// Mocks must precede singleton registration; this intentionally tests that load boundary.
const outbound = await import("../src/ws/sync-outbound.ts");
await Promise.resolve();
function emit(control: TestOneof): void { if (!state || !controlHandler) throw new Error("input control handler unavailable"); controlHandler(control, state); }
beforeEach(() => {
  vi.useFakeTimers(); outbound._resetTerminalOutboundForTest(); sent.length = 0;
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
  test("assigns distinct local correlations to complete FIFO batches", async () => {
    const first = outbound.sendTerminalInput("s1", new Uint8Array([1, 2]), "view-a");
    const second = outbound.sendTerminalInput("s1", new Uint8Array([3, 4]), "view-b");
    if (!first.accepted || !second.accepted) throw new Error("input admission failed");
    expect(first.inputSeq).not.toBe(second.inputSeq);
    for (const admission of [first, second]) emit({ case: "inputAccepted", value: { sessionId: "s1", inputSeq: admission.inputSeq, writtenBytes: 2, domainGeneration: 11n } });
    expect((await first.result).status).toBe("accepted"); expect((await second.result).status).toBe("accepted");
  });
});
