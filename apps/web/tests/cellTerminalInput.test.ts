// Predictive terminal input must fail closed when transport outcome is uncertain.
// The input controller owns admission results; renderer and predictor state must
// return to the authoritative cursor before another frame can reconcile them.
// Transport is mocked so accepted and ambiguous outcomes are deterministic.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { PredictiveEcho } from "../src/lib/predictiveEcho.ts";
import type { InputAdmission, InputOutcome } from "../src/ws/sync-outbound.ts";
import {
  _resetTerminalFindIntentsForTest,
  requestTerminalFind,
} from "../src/lib/terminalFindIntent.ts";
import type { CellTerminalProps } from "../src/components/cell-terminal-types.ts";

let outcome = Promise.withResolvers<InputOutcome>();
const sendUserTerminalInput = mock((): InputAdmission => ({
  accepted: true,
  inputSeq: 1n,
  result: outcome.promise,
}));

mock.module("../src/lib/userTerminalInput.ts", () => ({
  resetUserTerminalInput: () => {},
  sendUserTerminalInput,
}));

const { createCellTerminalInput } = await import(
  "../src/components/cell-terminal-input.ts"
);
const { createCellTerminalRuntime } = await import(
  "../src/components/cell-terminal-runtime.ts"
);

const props = {
  session: {
    id: "session-a",
    worker_fp: "worker-a",
    cwd: "/tmp",
  },
} as unknown as CellTerminalProps;

beforeEach(() => {
  _resetTerminalFindIntentsForTest();
  outcome = Promise.withResolvers<InputOutcome>();
  sendUserTerminalInput.mockClear();
});
afterEach(_resetTerminalFindIntentsForTest);

function inputHarness() {
  const runtime = createCellTerminalRuntime("session-a", () => undefined);
  const predict = mock(() => {});
  const clear = mock(() => {});
  const setPredictedCursor = mock(() => {});
  runtime.predictor = { predict, clear } as unknown as PredictiveEcho;
  runtime.renderer = { setPredictedCursor } as unknown as CellGridRenderer;
  const input = createCellTerminalInput(props, runtime);
  return { input, predict, clear, setPredictedCursor };
}

describe("CellTerminal find handoff lifecycle", () => {
  test("registers the pane find controller and unregisters it on disposal", () => {
    const harness = inputHarness();
    requestTerminalFind("session-a", "handoff marker", {
      caseSensitive: true,
      preferredGlobalMatch: { gridEpoch: "grid-a:0", row: 9n, col: 2 },
    });
    expect(harness.input.find.open()).toBe(true);
    expect(harness.input.find.query()).toBe("handoff marker");
    expect(harness.input.find.caseSensitive()).toBe(true);
    expect(harness.input.find.regex()).toBe(false);

    harness.input.dispose();
    requestTerminalFind("session-a", "after disposal");
    expect(harness.input.find.query()).toBe("handoff marker");
  });
});

describe("CellTerminal input prediction admission", () => {
  test("ambiguous completion clears predicted cells and cursor", async () => {
    const harness = inputHarness();
    harness.input.sendControllerData("a");
    expect(harness.predict).toHaveBeenCalledTimes(1);

    outcome.resolve({
      status: "ambiguous",
      inputSeq: 1n,
      writtenBytes: 0,
      reason: "connection closed before acknowledgement",
    });
    await outcome.promise;
    await Promise.resolve();

    expect(harness.clear).toHaveBeenCalledTimes(1);
    expect(harness.setPredictedCursor).toHaveBeenCalledWith(null);
    harness.input.dispose();
  });

  test("accepted completion preserves pending prediction", async () => {
    const harness = inputHarness();
    harness.input.sendControllerData("a");
    outcome.resolve({ status: "accepted", inputSeq: 1n, writtenBytes: 1 });
    await outcome.promise;
    await Promise.resolve();

    expect(harness.clear).not.toHaveBeenCalled();
    expect(harness.setPredictedCursor).not.toHaveBeenCalled();
    harness.input.dispose();
  });
});
