// Portable pane-layout adapter tests exercise stable preorder export and atomic
// fresh-ID materialization. Store seams prove rejected imports schedule no
// signals, subscriber work, timers, or browser-local persistence writes.

import { beforeEach, describe, expect, test, vi } from "bun:test";
import { LAYOUT_RATIO_MIN } from "@roost/shared/layout-document";
import type { LayoutDocumentV1 } from "@roost/shared/layout-document";
import type { Layout, PaneNode } from "../src/store/paneLayout.ts";

const STORAGE_KEY = "roost.paneLayout.v1";
const SESSION_A = "00000000-0000-4000-8000-000000000001";
const SESSION_B = "00000000-0000-4000-8000-000000000002";
const SESSION_C = "00000000-0000-4000-8000-000000000003";
const SESSION_D = "00000000-0000-4000-8000-000000000004";
const SESSION_EXTRA_A = "00000000-0000-4000-8000-000000000005";
const SESSION_EXTRA_B = "00000000-0000-4000-8000-000000000006";
const FOREIGN_SESSION = "00000000-0000-4000-8000-000000000099";

const storedValues: Record<string, string> = {};
let setItemCalls = 0;
const storageStub = {
  getItem: (key: string) => storedValues[key] ?? null,
  setItem: (key: string, value: string) => {
    setItemCalls++;
    storedValues[key] = value;
  },
  removeItem: (key: string) => { delete storedValues[key]; },
  clear: () => { for (const key of Object.keys(storedValues)) delete storedValues[key]; },
  key: () => null,
  length: 0,
} as Storage;
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: storageStub,
});

// Install localStorage before paneLayoutStore reads it at module initialization
// (test exercises the module-loading boundary — ts-no-dynamic-import exception).
const paneStore = await import("../src/store/paneLayoutStore.ts");
const layoutDocuments = await import("../src/store/paneLayoutDocument.ts");

function sourceLayout(): Layout {
  return {
    root: {
      kind: "split",
      id: "runtime-split-row",
      dir: "row",
      ratio: 0.1,
      a: {
        kind: "leaf",
        paneId: "runtime-pane-a",
        tabs: [SESSION_B, SESSION_A],
        selectedTab: SESSION_A,
      },
      b: {
        kind: "split",
        id: "runtime-split-col",
        dir: "col",
        ratio: 0.9,
        a: {
          kind: "leaf",
          paneId: "runtime-pane-b",
          tabs: [SESSION_C],
          selectedTab: SESSION_C,
        },
        b: {
          kind: "leaf",
          paneId: "runtime-pane-c",
          tabs: [SESSION_D],
          selectedTab: SESSION_D,
        },
      },
    },
    focusedPaneId: "runtime-pane-c",
  };
}

function expectedDocument(): LayoutDocumentV1 {
  return {
    schema_version: 1,
    root: {
      kind: "split",
      direction: "row",
      ratio: 0.1,
      first: {
        kind: "leaf",
        leaf_key: "leaf-1",
        slot_keys: ["slot-1", "slot-2"],
        selected_slot_key: "slot-2",
      },
      second: {
        kind: "split",
        direction: "col",
        ratio: 0.9,
        first: {
          kind: "leaf",
          leaf_key: "leaf-2",
          slot_keys: ["slot-3"],
          selected_slot_key: "slot-3",
        },
        second: {
          kind: "leaf",
          leaf_key: "leaf-3",
          slot_keys: ["slot-4"],
          selected_slot_key: "slot-4",
        },
      },
    },
    focused_leaf_key: "leaf-3",
    bindings: [
      { slot_key: "slot-1", session_id: SESSION_B },
      { slot_key: "slot-2", session_id: SESSION_A },
      { slot_key: "slot-3", session_id: SESSION_C },
      { slot_key: "slot-4", session_id: SESSION_D },
    ],
  };
}

function runtimeIds(node: PaneNode): string[] {
  if (node.kind === "leaf") return [node.paneId];
  return [node.id, ...runtimeIds(node.a), ...runtimeIds(node.b)];
}

function renamedRuntimeNode(node: PaneNode): PaneNode {
  if (node.kind === "leaf") return { ...node, paneId: `fresh-${node.paneId}` };
  return {
    ...node,
    id: `fresh-${node.id}`,
    a: renamedRuntimeNode(node.a),
    b: renamedRuntimeNode(node.b),
  };
}

beforeEach(() => {
  paneStore._flushPendingPersist();
  paneStore.clearPaneLayoutsForLogout();
  localStorage.clear();
  setItemCalls = 0;
});

describe("pane layout document export", () => {
  test("assigns stable preorder keys independent of every runtime UUID", () => {
    paneStore.commitLayout("folder-a", sourceLayout());
    expect(layoutDocuments.exportLayoutDocument(
      "folder-a",
      [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
    )).toEqual(expectedDocument());

    const original = sourceLayout();
    const differentRuntimeIds: Layout = {
      root: renamedRuntimeNode(original.root),
      focusedPaneId: `fresh-${original.focusedPaneId}`,
    };
    paneStore.commitLayout("folder-b", differentRuntimeIds);
    expect(layoutDocuments.exportLayoutDocument(
      "folder-b",
      [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
    )).toEqual(expectedDocument());
  });

  test("normalizes legacy UI ratios before strict export", () => {
    paneStore.commitLayout("legacy-ratio", {
      root: {
        kind: "split",
        id: "legacy-split",
        dir: "row",
        ratio: 1 / 12,
        a: { kind: "leaf", paneId: "legacy-a", tabs: [SESSION_A], selectedTab: SESSION_A },
        b: { kind: "leaf", paneId: "legacy-b", tabs: [SESSION_B], selectedTab: SESSION_B },
      },
      focusedPaneId: "legacy-a",
    });
    const document = layoutDocuments.exportLayoutDocument(
      "legacy-ratio",
      [SESSION_A, SESSION_B],
    );
    if (document.root.kind !== "split") throw new Error("expected exported split");
    expect(document.root.ratio).toBe(LAYOUT_RATIO_MIN);
  });

  test("exports one-session folders and no runtime or terminal material", () => {
    paneStore.commitLayout("single", {
      root: { kind: "leaf", paneId: "private-pane", tabs: [SESSION_A], selectedTab: SESSION_A },
      focusedPaneId: "private-pane",
    });
    const document = layoutDocuments.exportLayoutDocument("single", [SESSION_A]);
    expect(document).toEqual({
      schema_version: 1,
      root: {
        kind: "leaf",
        leaf_key: "leaf-1",
        slot_keys: ["slot-1"],
        selected_slot_key: "slot-1",
      },
      focused_leaf_key: "leaf-1",
      bindings: [{ slot_key: "slot-1", session_id: SESSION_A }],
    });
    const serialized = JSON.stringify(document);
    for (const forbidden of [
      "private-pane", "paneId", "focusedPaneId", "channel", "cells",
      "terminal", "viewport", "transcript", "browser", "splitId",
    ]) expect(serialized).not.toContain(forbidden);
    const imported = layoutDocuments.applyLayoutDocument(
      "single-import",
      document,
      [SESSION_A],
    );
    expect(imported.selectedSessionId).toBe(SESSION_A);
    expect(imported.layout.root.kind === "leaf" && imported.layout.root.tabs)
      .toEqual([SESSION_A]);
  });
});

describe("pane layout document apply", () => {
  test("round-trips topology, endpoint ratios, order, selection, and focus with fresh IDs", () => {
    const source = sourceLayout();
    paneStore.commitLayout("roundtrip", source);
    const document = layoutDocuments.exportLayoutDocument(
      "roundtrip",
      [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
    );
    const applied = layoutDocuments.applyLayoutDocument(
      "roundtrip",
      { ...document, bindings: [...document.bindings].reverse() },
      [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
    );

    const oldIds = new Set(runtimeIds(source.root));
    const newIds = runtimeIds(applied.layout.root);
    expect(new Set(newIds).size).toBe(newIds.length);
    expect(newIds.every((runtimeId) => !oldIds.has(runtimeId))).toBe(true);
    expect(applied.selectedSessionId).toBe(SESSION_D);
    expect(layoutDocuments.exportLayoutDocument(
      "roundtrip",
      [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
    )).toEqual(document);
  });

  test("round-trips intentional empty branches through future resolves", () => {
    const document: LayoutDocumentV1 = {
      schema_version: 1,
      root: {
        kind: "split",
        direction: "row",
        ratio: 0.5,
        first: {
          kind: "leaf",
          leaf_key: "leaf-1",
          slot_keys: [],
          selected_slot_key: null,
        },
        second: {
          kind: "leaf",
          leaf_key: "leaf-2",
          slot_keys: ["slot-1"],
          selected_slot_key: "slot-1",
        },
      },
      focused_leaf_key: "leaf-1",
      bindings: [{ slot_key: "slot-1", session_id: SESSION_A }],
    };
    const applied = layoutDocuments.applyLayoutDocument(
      "empty-branch",
      document,
      [SESSION_A],
    );
    expect(applied.selectedSessionId).toBeNull();
    const resolved = paneStore.resolveLayout("empty-branch", [SESSION_A]);
    expect(resolved.root.kind).toBe("split");
    expect(resolved.focusedPaneId).toBe(applied.layout.focusedPaneId);
    expect(layoutDocuments.exportLayoutDocument(
      "empty-branch",
      [SESSION_A],
    )).toEqual(document);
  });

  test("appends extra live sessions canonically to the focused leaf", () => {
    const document: LayoutDocumentV1 = {
      schema_version: 1,
      root: {
        kind: "split",
        direction: "row",
        ratio: 0.5,
        first: {
          kind: "leaf",
          leaf_key: "left",
          slot_keys: ["left-slot"],
          selected_slot_key: "left-slot",
        },
        second: {
          kind: "leaf",
          leaf_key: "right",
          slot_keys: [],
          selected_slot_key: null,
        },
      },
      focused_leaf_key: "right",
      bindings: [
        { slot_key: "left-slot", session_id: SESSION_A },
      ],
    };
    const applied = layoutDocuments.applyLayoutDocument(
      "extras",
      document,
      [SESSION_A, SESSION_B, SESSION_EXTRA_B, SESSION_EXTRA_A],
    );
    if (applied.layout.root.kind !== "split") throw new Error("expected split layout");
    expect(applied.layout.root.a.kind === "leaf" && applied.layout.root.a.tabs)
      .toEqual([SESSION_A]);
    expect(applied.layout.root.b.kind === "leaf" && applied.layout.root.b.tabs)
      .toEqual([SESSION_B, SESSION_EXTRA_B, SESSION_EXTRA_A]);
    expect(applied.selectedSessionId).toBe(SESSION_B);
  });

  test("rejects foreign-folder, foreign-worker, and closed-session bindings", () => {
    const foreign = expectedDocument();
    foreign.bindings[0] = { slot_key: "slot-1", session_id: FOREIGN_SESSION };
    for (const currentFolderSessions of [
      [SESSION_A, SESSION_C, SESSION_D],
      [SESSION_A, SESSION_B],
      [],
    ]) {
      expect(() => layoutDocuments.applyLayoutDocument(
        "current-folder",
        foreign,
        currentFolderSessions,
      )).toThrow(/not live in the current folder/);
    }
  });

  test("rejected imports leave record, signals, subscribers, timer, and storage byte-identical", () => {
    vi.useFakeTimers();
    paneStore.commitLayout("untouched-folder", {
      root: { kind: "leaf", paneId: "untouched-pane", tabs: [SESSION_A], selectedTab: SESSION_A },
      focusedPaneId: "untouched-pane",
    });
    paneStore._flushPendingPersist();
    setItemCalls = 0;
    let notifications = 0;
    const unsubscribe = paneStore.onLayoutCommit(() => { notifications++; });
    try {
      const beforeState = paneStore._paneLayoutStoreDebugSnapshot();
      const beforeStorage = localStorage.getItem(STORAGE_KEY);
      const invalid = { ...expectedDocument(), schema_version: 2 };
      const conflicting = expectedDocument();
      conflicting.bindings[0] = { slot_key: "slot-1", session_id: FOREIGN_SESSION };
      for (const candidate of [invalid, conflicting]) {
        expect(() => layoutDocuments.applyLayoutDocument(
          "never-observed-folder",
          candidate,
          [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
        )).toThrow();
      }
      expect(paneStore._paneLayoutStoreDebugSnapshot()).toEqual(beforeState);
      expect(localStorage.getItem(STORAGE_KEY)).toBe(beforeStorage);
      expect(notifications).toBe(0);
      vi.advanceTimersByTime(1_000);
      expect(setItemCalls).toBe(0);
      expect(localStorage.getItem(STORAGE_KEY)).toBe(beforeStorage);
    } finally {
      unsubscribe();
      vi.useRealTimers();
    }
  });

  test("successful apply emits one commit and one persistence burst", () => {
    vi.useFakeTimers();
    let notifications = 0;
    const unsubscribe = paneStore.onLayoutCommit(() => { notifications++; });
    try {
      layoutDocuments.applyLayoutDocument(
        "successful-apply",
        expectedDocument(),
        [SESSION_A, SESSION_B, SESSION_C, SESSION_D],
      );
      expect(notifications).toBe(1);
      expect(paneStore._paneLayoutStoreDebugSnapshot().persistScheduled).toBe(true);
      expect(setItemCalls).toBe(0);
      vi.advanceTimersByTime(350);
      expect(setItemCalls).toBe(1);
      expect(paneStore._paneLayoutStoreDebugSnapshot().persistScheduled).toBe(false);
      const persisted = JSON.parse(storedValues[STORAGE_KEY]!) as Record<string, Layout>;
      expect(persisted["successful-apply"]).toBeDefined();
    } finally {
      unsubscribe();
      vi.useRealTimers();
    }
  });
});

