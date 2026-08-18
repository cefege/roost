import { beforeEach, describe, expect, mock, spyOn, test } from "bun:test";
import type { InputAdmission } from "../src/ws/sync-outbound.ts";

let events: string[] = [];
let nextAdmission: InputAdmission = { accepted: false, reason: "not ready" };

const sendTerminalInput = mock((_sessionId: string, _bytes: Uint8Array): InputAdmission => {
  events.push("admit");
  return nextAdmission;
});
const diag = mock((_event: string, _facts: Record<string, unknown>) => {});
const signal = mock((_event: string, _facts: Record<string, unknown>) => {});

mock.module("../src/ws/sync-outbound.ts", () => ({ sendTerminalInput }));
mock.module("@roost/shared/diag", () => ({ diag, signal }));

// Dynamic import is required so Bun installs the transport and diagnostic
// module mocks before evaluating the router's static dependencies.
const {
  _resetUserTerminalInputForTest,
  registerUserTerminalInput,
  sendUserTerminalInput,
} = await import("../src/lib/userTerminalInput.ts");

const bytes = new Uint8Array([1, 2, 3]);

function acceptedAdmission(inputSeq = 1n): InputAdmission {
  return {
    accepted: true,
    inputSeq,
    result: Promise.resolve({ status: "accepted", inputSeq, writtenBytes: bytes.byteLength }),
  };
}

beforeEach(() => {
  _resetUserTerminalInputForTest();
  events = [];
  nextAdmission = { accepted: false, reason: "not ready" };
  sendTerminalInput.mockClear();
  diag.mockClear();
  signal.mockClear();
});

describe("sendUserTerminalInput", () => {
  test("rejection invokes no registered callback", () => {
    const callback = mock(() => {});
    registerUserTerminalInput("s1", callback);
    const rejection = nextAdmission;

    expect(sendUserTerminalInput("s1", bytes)).toBe(rejection);
    expect(sendTerminalInput).toHaveBeenCalledTimes(1);
    expect(callback).not.toHaveBeenCalled();
  });

  test("acceptance invokes the current callback synchronously exactly once", () => {
    const replaced = mock(() => events.push("replaced"));
    const current = mock(() => events.push("callback"));
    registerUserTerminalInput("s1", replaced);
    registerUserTerminalInput("s1", current);
    nextAdmission = acceptedAdmission();

    events.push("before");
    const returned = sendUserTerminalInput("s1", bytes);
    events.push("after");

    expect(returned).toBe(nextAdmission);
    expect(events).toEqual(["before", "admit", "callback", "after"]);
    expect(replaced).not.toHaveBeenCalled();
    expect(current).toHaveBeenCalledTimes(1);
  });

  test("callback failure is diagnosed and cannot replace or throw the admission", () => {
    const failure = new Error("transition failed");
    registerUserTerminalInput("s1", () => { throw failure; });
    nextAdmission = acceptedAdmission(2n);
    const warning = spyOn(console, "warn").mockImplementation(() => {});

    try {
      expect(sendUserTerminalInput("s1", bytes)).toBe(nextAdmission);
    } finally {
      warning.mockRestore();
    }

    expect(diag).toHaveBeenCalledWith("input.user_callback_failed", {
      sid: "s1",
      detail: "transition failed",
    });
    expect(signal).toHaveBeenCalledWith("diag.corruption_signal", {
      kind: "user_terminal_input_callback_failed",
      sid: "s1",
      detail: "transition failed",
      cooldownKey: "s1",
    });
  });
});

describe("registerUserTerminalInput", () => {
  test("stale unregister cannot remove a newer registration of the same callback", () => {
    const callback = mock(() => {});
    const unregisterStale = registerUserTerminalInput("s1", callback);
    registerUserTerminalInput("s1", callback);
    unregisterStale();
    nextAdmission = acceptedAdmission();

    sendUserTerminalInput("s1", bytes);

    expect(callback).toHaveBeenCalledTimes(1);
  });
});
