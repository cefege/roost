// Human-confirmed layout import degrades stale bindings; the unattended apply
// path does not. These pin the dropped-session count the confirm dialog shows,
// the collapse of the panes those bindings emptied, the focus fallback, and the
// rejection the acknowledged `roost api ui apply-layout` command still returns.

import { beforeEach, describe, expect, test } from "bun:test";
import type { LayoutDocumentNode, LayoutDocumentV1 } from "@roost/shared/layout-document";
import { UiApplyLayoutOutcome, type UiCommandFrame } from "@roost/shared/proto/sync_pb";
import {
  UI_LAYOUT_APPLY_REJECTION,
  executeTargetedUiLayoutApply,
  type UiLayoutApplyDependencies,
  type UiLayoutApplyResult,
} from "../src/lib/uiLayoutApplyCore.ts";

const PANE_COUNT = 10;
const DEAD_PANE = 4;
const sessionId = (index: number): string =>
  `00000000-0000-4000-8000-0000000000${String(index).padStart(2, "0")}`;
const ALL_SESSIONS = Array.from({ length: PANE_COUNT }, (_, idx) => sessionId(idx + 1));
const LIVE_SESSIONS = ALL_SESSIONS.filter((id) => id !== sessionId(DEAD_PANE));

const storedValues: Record<string, string> = {};
const storageStub = {
  getItem: (key: string) => storedValues[key] ?? null,
  setItem: (key: string, value: string) => { storedValues[key] = value; },
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

/** A right-leaning spine of `PANE_COUNT` leaves, one session each. */
function savedDocument(focusedLeafKey = `leaf-${PANE_COUNT}`): LayoutDocumentV1 {
  const leafFor = (index: number): LayoutDocumentNode => ({
    kind: "leaf",
    leaf_key: `leaf-${index}`,
    slot_keys: [`slot-${index}`],
    selected_slot_key: `slot-${index}`,
  });
  let root = leafFor(1);
  for (let index = 2; index <= PANE_COUNT; index++) {
    root = {
      kind: "split",
      direction: index % 2 === 0 ? "row" : "col",
      ratio: 0.5,
      first: root,
      second: leafFor(index),
    };
  }
  return {
    schema_version: 1,
    root,
    focused_leaf_key: focusedLeafKey,
    bindings: ALL_SESSIONS.map((session, idx) => ({
      slot_key: `slot-${idx + 1}`,
      session_id: session,
    })),
  };
}

function documentSessions(document: LayoutDocumentV1): string[] {
  const sessionBySlot = new Map(
    document.bindings.map((binding) => [binding.slot_key, binding.session_id]),
  );
  const sessions: string[] = [];
  const visit = (node: LayoutDocumentNode): void => {
    if (node.kind === "split") {
      visit(node.first);
      visit(node.second);
      return;
    }
    for (const slotKey of node.slot_keys) sessions.push(sessionBySlot.get(slotKey)!);
    if (node.slot_keys.length === 0) sessions.push("<empty>");
  };
  visit(document.root);
  return sessions;
}

beforeEach(() => {
  paneStore._flushPendingPersist();
  paneStore.clearPaneLayoutsForLogout();
  localStorage.clear();
});

describe("human-confirmed layout import degradation", () => {
  test("drops one dead binding, collapses its pane, and reports the count", () => {
    const degraded = layoutDocuments.degradeLayoutDocumentToLiveSessions(
      savedDocument(),
      LIVE_SESSIONS,
    );

    expect(degraded.droppedSessionCount).toBe(1);
    expect(documentSessions(degraded.document)).toEqual(LIVE_SESSIONS);
    expect(degraded.document.bindings.map((binding) => binding.session_id))
      .toEqual(LIVE_SESSIONS);

    const applied = layoutDocuments.applyLayoutDocument(
      "degraded",
      degraded.document,
      LIVE_SESSIONS,
    );
    expect(applied.layout.root.kind).toBe("split");
    expect(applied.selectedSessionId).toBe(sessionId(PANE_COUNT));
  });

  test("falls back to a surviving pane when the dropped binding held focus", () => {
    const degraded = layoutDocuments.degradeLayoutDocumentToLiveSessions(
      savedDocument(`leaf-${DEAD_PANE}`),
      LIVE_SESSIONS,
    );

    expect(degraded.droppedSessionCount).toBe(1);
    const focusedSlots = documentSessions({
      ...degraded.document,
      root: focusedLeaf(degraded.document),
    });
    expect(focusedSlots).not.toContain("<empty>");
    expect(LIVE_SESSIONS).toContain(focusedSlots[0]!);
  });

  test("leaves a fully live document untouched so the preview is not re-keyed", () => {
    const saved = savedDocument();
    const degraded = layoutDocuments.degradeLayoutDocumentToLiveSessions(saved, ALL_SESSIONS);

    expect(degraded.droppedSessionCount).toBe(0);
    expect(degraded.document).toEqual(saved);
  });

  test("degrades to one empty root leaf when no saved session is live", () => {
    const degraded = layoutDocuments.degradeLayoutDocumentToLiveSessions(
      savedDocument(),
      [sessionId(99)],
    );

    expect(degraded.droppedSessionCount).toBe(PANE_COUNT);
    expect(degraded.document.root).toEqual({
      kind: "leaf",
      leaf_key: "leaf-1",
      slot_keys: [],
      selected_slot_key: null,
    });
    expect(degraded.document.bindings).toEqual([]);

    const applied = layoutDocuments.applyLayoutDocument("emptied", degraded.document, []);
    expect(applied.layout.root).toEqual({
      kind: "leaf",
      paneId: applied.layout.focusedPaneId,
      tabs: [],
      selectedTab: "",
    });
    expect(applied.selectedSessionId).toBeNull();
  });

  test("still rejects invalid documents instead of degrading them", () => {
    expect(() => layoutDocuments.degradeLayoutDocumentToLiveSessions(
      { ...savedDocument(), schema_version: 2 },
      LIVE_SESSIONS,
    )).toThrow();
  });
});

describe("unattended layout apply stays exact", () => {
  test("rejects the same stale document the confirm dialog would degrade", () => {
    const results: UiLayoutApplyResult[] = [];
    const dependencies: UiLayoutApplyDependencies = {
      currentTabId: () => "tab-current",
      currentSocketId: () => "socket-current",
      activeFolder: () => ({
        folderKey: "worker::/work",
        activeSessionId: LIVE_SESSIONS[0]!,
        liveSessionIds: LIVE_SESSIONS,
        hasClientOnlySession: false,
      }),
      decodeDocument: () => savedDocument(),
      applyDocument: layoutDocuments.applyLayoutDocument,
      clearSpotlight: () => { throw new Error("a rejected apply must not clear spotlight"); },
      navigateToSession: () => { throw new Error("a rejected apply must not navigate"); },
      sendResult: (result) => { results.push(result); return true; },
      recordDiagnostic: () => {},
    };
    const frame = {
      targetTabId: "tab-current",
      targetSocketId: "socket-current",
      correlationId: "correlation-1",
      command: { command: { case: "applyLayout", value: { document: {} } } },
    } as unknown as UiCommandFrame;
    const storeBefore = paneStore._paneLayoutStoreDebugSnapshot();

    expect(executeTargetedUiLayoutApply(frame, dependencies)).toBe(true);
    expect(results).toEqual([{
      correlationId: "correlation-1",
      outcome: UiApplyLayoutOutcome.REJECTED,
      reason: UI_LAYOUT_APPLY_REJECTION.invalidDocument,
    }]);
    expect(paneStore._paneLayoutStoreDebugSnapshot()).toEqual(storeBefore);
  });
});

function focusedLeaf(document: LayoutDocumentV1): LayoutDocumentNode {
  const find = (node: LayoutDocumentNode): LayoutDocumentNode | null => {
    if (node.kind === "leaf") return node.leaf_key === document.focused_leaf_key ? node : null;
    return find(node.first) ?? find(node.second);
  };
  const leaf = find(document.root);
  if (!leaf) throw new Error("the degraded document names a focused leaf that does not exist");
  return leaf;
}
