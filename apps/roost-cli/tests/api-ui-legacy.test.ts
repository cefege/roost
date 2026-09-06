// Legacy UI command tests pin exact argv parsing and publication requests.
// Options may precede positionals, but duplicates, unknowns, and wrong arity fail.
// A fake client proves malformed input never reaches the coordinator.

import { describe, expect, test } from "bun:test";
import {
  dispatchUiApi,
  type UiApiClient,
  type UiApiIo,
} from "../src/api-ui.ts";

type DispatchRequest = Parameters<UiApiClient["uiDispatch"]>[0];

type LegacyInvocation = {
  calls: DispatchRequest[];
  output: string[];
  errors: string[];
  exits: number[];
  promise: Promise<boolean>;
};

function invokeLegacy(args: readonly string[], delivered = 7): LegacyInvocation {
  const calls: DispatchRequest[] = [];
  const output: string[] = [];
  const errors: string[] = [];
  const exits: number[] = [];
  const client: UiApiClient = {
    async uiListStates() {
      throw new Error("legacy command must not list UI state");
    },
    async uiDispatch(request) {
      calls.push(request);
      return { delivered };
    },
    async uiApplyLayout() {
      throw new Error("legacy command must not apply a layout");
    },
  };
  const io: UiApiIo = {
    writeLine: (line) => output.push(line),
    writeError: (line) => errors.push(line),
    setExitCode: (code) => exits.push(code),
    readTextFile: async () => {
      throw new Error("legacy command must not read a file");
    },
    now: () => 0,
  };
  return {
    calls,
    output,
    errors,
    exits,
    promise: dispatchUiApi(client, "ui", args, io),
  };
}

const LEGACY_CASES = [
  {
    name: "navigate",
    args: ["navigate", "--tab", "tab-target", "/design"],
    command: { command: { case: "navigate", value: { path: "/design" } } },
  },
  {
    name: "place-split",
    args: ["place-split", "--tab", "tab-target", "--first", "session-a", "session-b", "row"],
    command: { command: { case: "placeSplit", value: {
      sessionId: "session-a",
      anchorSessionId: "session-b",
      dir: "row",
      insertFirst: true,
    } } },
  },
  {
    name: "select-tab",
    args: ["select-tab", "--tab", "tab-target", "session-a"],
    command: { command: { case: "selectTab", value: { sessionId: "session-a" } } },
  },
  {
    name: "focus-pane",
    args: ["focus-pane", "--tab", "tab-target", "session-a"],
    command: { command: { case: "focusPane", value: { sessionId: "session-a" } } },
  },
  {
    name: "move-tab",
    args: ["move-tab", "--tab", "tab-target", "session-a", "session-b"],
    command: { command: { case: "moveTab", value: {
      sessionId: "session-a",
      destSessionId: "session-b",
    } } },
  },
  {
    name: "arrange",
    args: ["arrange", "--tab", "tab-target", "balance"],
    command: { command: { case: "arrange", value: { preset: "balance" } } },
  },
  {
    name: "close-tab",
    args: ["close-tab", "--tab", "tab-target", "session-a"],
    command: { command: { case: "closeTab", value: { sessionId: "session-a" } } },
  },
  {
    name: "spotlight",
    args: ["spotlight", "--tab", "tab-target", "--off", "session-a"],
    command: { command: { case: "spotlight", value: { sessionId: "session-a", off: true } } },
  },
] as const;

const POSITIONAL_CASES = [
  ["navigate", "/design"],
  ["place-split", "session-a", "session-b", "row"],
  ["select-tab", "session-a"],
  ["focus-pane", "session-a"],
  ["move-tab", "session-a", "session-b"],
  ["arrange", "balance"],
  ["close-tab", "session-a"],
  ["spotlight", "session-a"],
] as const;

describe("legacy UI API commands", () => {
  test("removes a leading tab option and preserves all eight exact commands", async () => {
    for (const entry of LEGACY_CASES) {
      const invocation = invokeLegacy(entry.args);
      await expect(invocation.promise, entry.name).resolves.toBe(true);
      expect(invocation.output, entry.name).toEqual(["delivered=7"]);
      expect(invocation.errors, entry.name).toEqual([]);
      expect(invocation.exits, entry.name).toEqual([]);
      expect(invocation.calls, entry.name).toEqual([{
        targetTabId: "tab-target",
        command: entry.command,
      }]);
    }
  });

  test("keeps omitted tab targeting as an exact broadcast", async () => {
    const invocation = invokeLegacy(["navigate", "/design"]);
    await expect(invocation.promise).resolves.toBe(true);
    expect(invocation.calls[0]?.targetTabId).toBe("");
    expect(invocation.calls[0]?.command).toEqual({
      command: { case: "navigate", value: { path: "/design" } },
    });
  });

  test("preserves false defaults for command-specific flags", async () => {
    const place = invokeLegacy(["place-split", "a", "b", "col"]);
    const spotlight = invokeLegacy(["spotlight", "a"]);
    await expect(Promise.all([place.promise, spotlight.promise])).resolves.toEqual([true, true]);
    expect(place.calls[0]?.command).toEqual({ command: { case: "placeSplit", value: {
      sessionId: "a",
      anchorSessionId: "b",
      dir: "col",
      insertFirst: false,
    } } });
    expect(spotlight.calls[0]?.command).toEqual({
      command: { case: "spotlight", value: { sessionId: "a", off: false } },
    });
  });

  test("rejects duplicate and missing tab values for every command", async () => {
    const invalidSuffixes = [
      ["--tab", "one", "--tab", "two"],
      ["--tab"],
      ["--tab", ""],
      ["--tab", "   "],
      ["--tab", "--unknown"],
    ];
    for (const args of POSITIONAL_CASES) {
      for (const suffix of invalidSuffixes) {
        const invocation = invokeLegacy([...args, ...suffix]);
        await expect(invocation.promise, args[0]).rejects.toThrow(/^ui /);
        expect(invocation.calls, args[0]).toEqual([]);
      }
    }
    for (const args of [
      ["place-split", "a", "b", "row", "--first", "--first"],
      ["spotlight", "a", "--off", "--off"],
    ]) {
      const invocation = invokeLegacy(args);
      await expect(invocation.promise).rejects.toThrow(/^ui /);
      expect(invocation.calls).toEqual([]);
    }
  });

  test("rejects unknown options for every command, including unsupported tab equals", async () => {
    for (const args of POSITIONAL_CASES) {
      const invocation = invokeLegacy([...args, "--unknown"]);
      await expect(invocation.promise, args[0]).rejects.toThrow("unknown option");
      expect(invocation.calls, args[0]).toEqual([]);
    }
    for (const args of [
      ["navigate", "/", "--tab=tab-target"],
      ["navigate", "/", "--first"],
      ["navigate", "/", "--off"],
    ]) {
      const invocation = invokeLegacy(args);
      await expect(invocation.promise).rejects.toThrow("unknown option");
      expect(invocation.calls).toEqual([]);
    }
  });

  test("rejects missing and extra positionals for all eight commands", async () => {
    for (const args of POSITIONAL_CASES) {
      for (const malformed of [args.slice(0, -1), [...args, "extra"]]) {
        const invocation = invokeLegacy(malformed);
        await expect(invocation.promise, args[0]).rejects.toThrow(`ui ${args[0]}:`);
        expect(invocation.calls, args[0]).toEqual([]);
      }
    }
  });

  test("preserves invalid direction and preset exit behavior without publication", async () => {
    for (const [args, message] of [
      [["place-split", "a", "b", "diagonal"], "roost api: dir must be row|col, got \"diagonal\""],
      [["arrange", "cascade"], "roost api: preset must be even|rows|tiled|main-vertical|balance, got \"cascade\""],
    ] as const) {
      const invocation = invokeLegacy(args);
      await expect(invocation.promise).resolves.toBe(true);
      expect(invocation.output).toEqual([]);
      expect(invocation.errors).toEqual([message]);
      expect(invocation.exits).toEqual([1]);
      expect(invocation.calls).toEqual([]);
    }
  });

  test("keeps delivered=0 warning exact and successful", async () => {
    const invocation = invokeLegacy(["navigate", "/", "--tab", "tab-target"], 0);
    await expect(invocation.promise).resolves.toBe(true);
    expect(invocation.output).toEqual(["delivered=0"]);
    expect(invocation.errors).toEqual([
      "roost api: delivered=0 — no browser tab connected to coord; spatial commands need a live SPA (check `roost api ui-state`)",
    ]);
    expect(invocation.exits).toEqual([]);
    expect(invocation.calls).toHaveLength(1);
  });
});
