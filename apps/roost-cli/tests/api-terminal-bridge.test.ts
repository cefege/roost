// Pins the machine-readable terminal bridge without enrollment or a coordinator.
// Injected stdin and client calls prove byte limits, one-shot delivery, and stable output.
// RPC behavior is asserted at the CLI boundary rather than through a mock echo.

import { describe, expect, test } from "bun:test";
import {
  dispatchTerminalBridgeApi,
  TERMINAL_BRIDGE_MAX_INPUT_BYTES,
  type TerminalBridgeIo,
} from "../src/api-terminal-bridge.ts";

type TerminalBridgeClient = Parameters<typeof dispatchTerminalBridgeApi>[0];

function harness(options: {
  stdin?: Uint8Array;
  accepted?: boolean;
  inputError?: Error;
  session?: { spawnCwd?: string; customTitle?: string };
} = {}) {
  const output: string[] = [];
  const exits: number[] = [];
  const calls: Array<{ sessionId: string; data: Uint8Array }> = [];
  let stdinReads = 0;
  const client = {
    async sessionsList() {
      return {
        sessions: [{
          id: "session-1",
          workerFp: "worker-fingerprint",
          kind: "shell",
          cwd: "/work",
          spawnCwd: "/spawn",
          customTitle: "Roost",
          status: "open",
          ...options.session,
        }],
      };
    },
    async sessionsInput(request: { sessionId: string; data: Uint8Array }) {
      calls.push(request);
      if (options.inputError) throw options.inputError;
      return { accepted: options.accepted ?? true };
    },
  } as unknown as TerminalBridgeClient;
  const io: TerminalBridgeIo = {
    async readStdin() {
      stdinReads += 1;
      return options.stdin ?? new Uint8Array();
    },
    writeLine(line) {
      output.push(line);
    },
    writeExitCode(code) {
      exits.push(code);
    },
  };
  return { client, io, output, exits, calls, stdinReads: () => stdinReads };
}

describe("terminal bridge API CLI", () => {
  test("prints the existing tabular session projection without --json", async () => {
    const subject = harness();

    await expect(dispatchTerminalBridgeApi(subject.client, "sessions", [], subject.io)).resolves.toBe(true);

    expect(subject.output).toEqual(["session-1\tworker-fingerprint\tshell\t/work\tRoost"]);
  });

  test("prints the stable machine-readable session projection", async () => {
    const subject = harness();

    await dispatchTerminalBridgeApi(subject.client, "sessions", ["--json"], subject.io);

    expect(subject.output).toEqual([JSON.stringify([{
      id: "session-1",
      workerFp: "worker-fingerprint",
      cwd: "/work",
      spawnCwd: "/spawn",
      title: "Roost",
      status: "open",
    }])]);
  });

  test("normalizes absent optional session strings into stable JSON fields", async () => {
    const subject = harness({ session: { spawnCwd: undefined, customTitle: undefined } });

    await dispatchTerminalBridgeApi(subject.client, "sessions", ["--json"], subject.io);

    expect(JSON.parse(subject.output[0]!)).toEqual([expect.objectContaining({
      spawnCwd: "",
      title: "",
    })]);
  });

  test("reads stdin once, preserves exact bytes, and appends one carriage return", async () => {
    const source = new Uint8Array([0x00, 0x0a, 0xff]);
    const subject = harness({ stdin: source });

    await dispatchTerminalBridgeApi(subject.client, "input", ["session-1", "--stdin", "--enter"], subject.io);

    expect(subject.stdinReads()).toBe(1);
    expect(subject.calls).toEqual([{ sessionId: "session-1", data: new Uint8Array([0x00, 0x0a, 0xff, 0x0d]) }]);
    expect(subject.output).toEqual(['{"ok":true,"accepted":true}']);
    expect(subject.exits).toEqual([]);
  });

  test("preserves escaped positional input and one carriage return", async () => {
    const subject = harness();

    await dispatchTerminalBridgeApi(subject.client, "input", ["session-1", "alpha\\nbeta", "--enter"], subject.io);

    expect(new TextDecoder().decode(subject.calls[0]?.data)).toBe("alpha\nbeta\r");
  });

  test("rejects invalid input forms before reading stdin or sending an RPC", async () => {
    for (const args of [
      [] as string[],
      ["session-1"],
      ["session-1", "text", "--stdin"],
      ["session-1", "--unknown"],
      ["session-1", "text", "another"],
    ]) {
      const subject = harness();
      await expect(dispatchTerminalBridgeApi(subject.client, "input", args, subject.io)).rejects.toThrow("input:");
      expect(subject.stdinReads()).toBe(0);
      expect(subject.calls).toEqual([]);
    }
  });

  test("enforces byte boundaries before one-shot terminal delivery", async () => {
    for (const [args, bytes] of [
      [["session-1", "--stdin"] as string[], TERMINAL_BRIDGE_MAX_INPUT_BYTES],
      [["session-1", "--stdin", "--enter"] as string[], TERMINAL_BRIDGE_MAX_INPUT_BYTES - 1],
    ] as const) {
      const subject = harness({ stdin: new Uint8Array(bytes) });
      await dispatchTerminalBridgeApi(subject.client, "input", args, subject.io);
      expect(subject.calls[0]?.data.byteLength).toBe(TERMINAL_BRIDGE_MAX_INPUT_BYTES);
    }
    for (const [args, bytes] of [
      [["session-1", "--stdin"] as string[], TERMINAL_BRIDGE_MAX_INPUT_BYTES + 1],
      [["session-1", "--stdin", "--enter"] as string[], TERMINAL_BRIDGE_MAX_INPUT_BYTES],
    ] as const) {
      const subject = harness({ stdin: new Uint8Array(bytes) });
      await expect(dispatchTerminalBridgeApi(subject.client, "input", args, subject.io)).rejects.toThrow("byte limit");
      expect(subject.calls).toEqual([]);
    }
  });

  test("prints rejected input as a stable failure and exits two", async () => {
    const subject = harness({ accepted: false });

    await dispatchTerminalBridgeApi(subject.client, "input", ["session-1", "--stdin"], subject.io);

    expect(subject.calls).toHaveLength(1);
    expect(subject.output).toEqual(['{"ok":false,"accepted":false,"error":"terminal input was not accepted"}']);
    expect(subject.exits).toEqual([2]);
  });

  test("propagates a transport failure after one terminal write attempt", async () => {
    const subject = harness({ inputError: new Error("transport interrupted") });

    await expect(dispatchTerminalBridgeApi(subject.client, "input", ["session-1", "--stdin"], subject.io))
      .rejects.toThrow("transport interrupted");

    expect(subject.calls).toHaveLength(1);
  });

  test("advertises the stdin form without a terminal RPC", async () => {
    const subject = harness();

    await dispatchTerminalBridgeApi(subject.client, "input", ["--help"], subject.io);

    expect(subject.output).toEqual(["Usage: roost api input <sessionId> (<escaped-text> | --stdin) [--enter]"]);
    expect(subject.calls).toEqual([]);
  });
});
