// One-shot terminal-find handoff tests for warm and cold pane mounts.
// They pin latest-pending semantics, identity-safe unregistration, preferred
// current-epoch metadata forwarding, and dashboard-bound registry reset.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { TerminalFind } from "../src/lib/terminalFindController.ts";
import type { TerminalFindQueryOptions } from "../src/lib/terminalFindHandoff.ts";
import {
  _resetTerminalFindIntentsForTest,
  registerTerminalFind,
  requestTerminalFind,
  resetTerminalFindIntentsForDashboardSwitch,
} from "../src/lib/terminalFindIntent.ts";

function fakeFind() {
  const openFind = mock(() => {});
  const queries: Array<{ query: string; options?: TerminalFindQueryOptions }> = [];
  const setQuery = mock((query: string, options?: TerminalFindQueryOptions) => {
    queries.push({ query, options });
  });
  return {
    find: { openFind, setQuery } as Pick<TerminalFind, "openFind" | "setQuery">,
    openFind,
    setQuery,
    queries,
  };
}

beforeEach(_resetTerminalFindIntentsForTest);
afterEach(_resetTerminalFindIntentsForTest);

describe("terminal find intent registry", () => {
  test("a warm pane consumes the literal intent immediately", () => {
    const mounted = fakeFind();
    const unregister = registerTerminalFind("session-a", mounted.find);

    requestTerminalFind("session-a", "Needle.*", {
      caseSensitive: true,
      preferredGlobalMatch: { gridEpoch: "grid-a:0", row: 41n, col: 7 },
    });

    expect(mounted.openFind).toHaveBeenCalledTimes(1);
    expect(mounted.queries).toEqual([{
      query: "Needle.*",
      options: {
        literal: true,
        caseSensitive: true,
        preferredMatch: { gridEpoch: "grid-a:0", row: 41n, col: 7 },
      },
    }]);
    unregister();
  });

  test("a cold pane consumes only the latest pending intent on mount", () => {
    requestTerminalFind("session-cold", "older");
    requestTerminalFind("session-cold", "newer", { caseSensitive: true });
    const mounted = fakeFind();

    registerTerminalFind("session-cold", mounted.find);

    expect(mounted.openFind).toHaveBeenCalledTimes(1);
    expect(mounted.queries).toEqual([{
      query: "newer",
      options: {
        literal: true,
        caseSensitive: true,
        preferredMatch: undefined,
      },
    }]);
  });

  test("an older disposer cannot unregister its replacement", () => {
    const first = fakeFind();
    const second = fakeFind();
    const unregisterFirst = registerTerminalFind("session-a", first.find);
    const unregisterSecond = registerTerminalFind("session-a", second.find);

    unregisterFirst();
    requestTerminalFind("session-a", "replacement");

    expect(first.openFind).not.toHaveBeenCalled();
    expect(second.openFind).toHaveBeenCalledTimes(1);
    expect(second.queries[0]?.query).toBe("replacement");
    unregisterSecond();
  });

  test("dashboard reset clears mounted callbacks and cold intents", () => {
    const retired = fakeFind();
    registerTerminalFind("mounted-old", retired.find);
    requestTerminalFind("cold-old", "old dashboard");

    resetTerminalFindIntentsForDashboardSwitch();
    requestTerminalFind("mounted-old", "new dashboard");
    expect(retired.openFind).not.toHaveBeenCalled();

    const coldReplacement = fakeFind();
    registerTerminalFind("cold-old", coldReplacement.find);
    expect(coldReplacement.openFind).not.toHaveBeenCalled();

    const mountedReplacement = fakeFind();
    registerTerminalFind("mounted-old", mountedReplacement.find);
    expect(mountedReplacement.queries[0]?.query).toBe("new dashboard");
  });
});
