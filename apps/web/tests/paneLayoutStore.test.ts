// paneLayoutStore — per-key reactive layout persistence. Asserts the
// load-bearing behaviors of the perf rewrite (Lane B10, web-ui-perf-sweep):
//   1. per-key isolation: commitLayout(fkB) must not affect fkA's resolved
//      value (the reactive per-key subscription itself is untestable here —
//      bun test resolves solid-js to the SERVER build, which never re-runs
//      computations — so this pins the value contract the signal map rides on),
//   2. undo-snapshot immutability: a Layout captured via resolveLayout BEFORE a
//      commit must not reflect the commit (TerminalDeck doClose captures
//      `before` for the undo re-commit — a solid createStore would have merged
//      in place and corrupted it; this is why the store is a signal map),
//   3. persist is a trailing 300ms debounce with a single write per burst,
//      flushed on pagehide (stamps survive tab close).

import { expect, test, describe, beforeEach, vi } from "bun:test";
import type { Layout } from "../src/store/paneLayout.ts";

// bun test has no localStorage — stub it before importing the module under
// test (import-time: load() reads localStorage). The pagehide flush is
// exercised via the exported _flushPendingPersist seam, NOT a window stub:
// another suite may load the module first (shared module cache), so a
// capture-on-addEventListener would be order-fragile.
const _ls: Record<string, string> = {};
let _setItemCalls = 0;
(globalThis as unknown as { localStorage: Storage }).localStorage = {
  getItem: (k: string) => _ls[k] ?? null,
  setItem: (k: string, v: string) => { _setItemCalls++; _ls[k] = v; },
  removeItem: (k: string) => { delete _ls[k]; },
  clear: () => { for (const k of Object.keys(_ls)) delete _ls[k]; },
  key: () => null, length: 0,
} as Storage;

// Dynamic import is deliberate: the module must load AFTER the stub above
// (test exercises the module-loading boundary — ts-no-dynamic-import exception).
const { commitLayout, resolveLayout, _flushPendingPersist } = await import("../src/store/paneLayoutStore.ts");

const STORAGE_KEY = "roost.paneLayout.v1";
const DEBOUNCE_MS = 300; // PERSIST_DEBOUNCE_MS in paneLayoutStore.ts

function leafLayout(paneId: string, tabs: string[]): Layout {
  return { root: { kind: "leaf", paneId, tabs, selectedTab: tabs[0] ?? "" }, focusedPaneId: paneId };
}

describe("paneLayoutStore", () => {
  beforeEach(() => {
    _flushPendingPersist(); // flush any pending persist from the prior test
    _setItemCalls = 0;
  });

  test("per-key isolation: commits land on their own key only", () => {
    commitLayout("fk-a", leafLayout("pa", ["s1"]));
    commitLayout("fk-b", leafLayout("pb", ["s2"]));
    commitLayout("fk-b", leafLayout("pb2", ["s2"]));
    expect(resolveLayout("fk-a", ["s1"]).focusedPaneId).toBe("pa");
    expect(resolveLayout("fk-b", ["s2"]).focusedPaneId).toBe("pb2");
  });

  test("undo snapshot: a resolved Layout is immutable across later commits", () => {
    commitLayout("fk-undo", leafLayout("p1", ["s1", "s2"]));
    const before = resolveLayout("fk-undo", ["s1", "s2"]);
    commitLayout("fk-undo", leafLayout("p2", ["s1"])); // e.g. closeTab committed
    expect(before.focusedPaneId).toBe("p1");
    expect(before.root.kind === "leaf" && before.root.tabs).toEqual(["s1", "s2"]);
    commitLayout("fk-undo", before); // the undo path re-commits the snapshot
    expect(resolveLayout("fk-undo", ["s1", "s2"]).focusedPaneId).toBe("p1");
  });

  test("persist: one debounced write per burst; pagehide flushes pending", () => {
    vi.useFakeTimers();
    try {
      commitLayout("fk-p1", leafLayout("p1", ["s1"]));
      commitLayout("fk-p2", leafLayout("p2", ["s2"]));
      commitLayout("fk-p1", leafLayout("p1b", ["s1"]));
      expect(_setItemCalls).toBe(0); // nothing synchronous
      vi.advanceTimersByTime(DEBOUNCE_MS + 50);
      expect(_setItemCalls).toBe(1); // whole burst = one write
      const persisted = JSON.parse(_ls[STORAGE_KEY]!) as Record<string, Layout>;
      expect(persisted["fk-p1"]?.focusedPaneId).toBe("p1b");
      expect(persisted["fk-p2"]?.focusedPaneId).toBe("p2");

      commitLayout("fk-p3", leafLayout("p3", ["s3"]));
      expect(_setItemCalls).toBe(1); // still debounced
      _flushPendingPersist(); // what the pagehide listener invokes
      expect(_setItemCalls).toBe(2); // pagehide flushed immediately
      expect((JSON.parse(_ls[STORAGE_KEY]!) as Record<string, Layout>)["fk-p3"]?.focusedPaneId).toBe("p3");
      vi.advanceTimersByTime(DEBOUNCE_MS + 50);
      expect(_setItemCalls).toBe(2); // flush canceled the trailing timer — no double write
    } finally {
      vi.useRealTimers();
    }
  });
});
