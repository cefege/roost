// Typed UI state-report coverage at the browser wire boundary.
// The fixture proves runtime state exports through the portable document,
// optimistic client-only bindings stay local, and retired opaque fields remain
// absent. Off-terminal routes stay layout-free; client Solid drives hydration.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import { layoutDocumentFromProto } from "@roost/shared/layout-document-proto";
import type { LayoutDocumentV1 } from "@roost/shared/layout-document";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import {
  abortOptimisticSpawn,
  beginOptimisticSpawn,
  clearAborted,
  failOptimisticSpawn,
} from "../src/store/optimisticSpawn.ts";

// Bun's static Solid import selects inert SSR effects; this loads the client sibling.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const {
  createEffect,
  createRoot,
  createSignal,
  on,
} = await import(solidClientUrl.href) as typeof SolidApi;

const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const FOLDER_KEY = "worker::/work";
const WORKER_FP = asWorkerFp("aa".repeat(32));

function documentFor(sessionIds: readonly string[]): LayoutDocumentV1 {
  const slotKeys = sessionIds.map((_, index) => `slot-${index + 1}`);
  return {
    schema_version: 1,
    root: {
      kind: "leaf",
      leaf_key: "leaf-1",
      slot_keys: slotKeys,
      selected_slot_key: slotKeys[0] ?? null,
    },
    focused_leaf_key: "leaf-1",
    bindings: sessionIds.map((sessionId, index) => ({
      slot_key: slotKeys[index]!,
      session_id: sessionId,
    })),
  };
}

function anchorSession(): Session {
  return {
    id: asSessionId(SESSION_ID),
    worker_fp: WORKER_FP,
    channel: asChannelId(7),
    kind: "shell",
    cwd: "/work",
    spawn_cwd: "/work",
    workspace_id: null,
    status: "open",
    created_at: 1,
    closed_at: null,
    custom_title: null,
  };
}

const document = documentFor([SESSION_ID]);
const exportCalls: Array<{ folderKey: string; liveSessionIds: readonly string[] }> = [];
let visibleSessionIds: string[] = [SESSION_ID];

mock.module("../src/connect.ts", () => ({
  coordClient: { uiReportState: async () => ({}) },
}));
mock.module("../src/auth/tab-id.ts", () => ({
  getTabId: () => "tab-current",
}));
mock.module("../src/store/paneLayoutStore.ts", () => ({
  onLayoutCommit: () => () => {},
}));
mock.module("../src/store/paneLayoutDocument.ts", () => ({
  exportLayoutDocument: (folderKey: string, liveSessionIds: readonly string[]) => {
    exportCalls.push({ folderKey, liveSessionIds });
    return documentFor(liveSessionIds);
  },
}));
mock.module("../src/store/selectors.ts", () => ({
  activeSessionForPath: (path: string) => {
    const sessionId = path.match(/^\/s\/([^/]+)/)?.[1];
    return sessionId && visibleSessionIds.includes(sessionId)
      ? { id: sessionId, status: "open" }
      : null;
  },
  liveSessionIdsForFolder: () => [...visibleSessionIds],
}));
mock.module("../src/lib/folderKey.ts", () => ({
  folderKeyOf: () => FOLDER_KEY,
}));

// These transport/store mocks must install before the reporter evaluates imports.
const {
  _buildUiStateReport,
  authoritativeUiReportSessionId,
  scheduleUiStateReportOnSessionResolution,
} = await import("../src/lib/uiStateReport.ts");

beforeEach(() => {
  exportCalls.length = 0;
  visibleSessionIds = [SESSION_ID];
});

describe("typed browser UI state reports", () => {
  test("exports the active layout as typed LayoutDocumentV1", () => {
    const report = _buildUiStateReport(`/s/${SESSION_ID}`);
    expect(report.tabId).toBe("tab-current");
    expect(report.activePath).toBe(`/s/${SESSION_ID}`);
    expect(report.folderKey).toBe(FOLDER_KEY);
    expect(exportCalls).toEqual([{
      folderKey: FOLDER_KEY,
      liveSessionIds: [SESSION_ID],
    }]);
    expect(report.layoutDocument).toBeDefined();
    expect(layoutDocumentFromProto(report.layoutDocument!)).toEqual(document);
    for (const retiredField of [
      "layoutJson",
      "focusedPaneId",
      "visibleSessionIds",
    ]) expect(retiredField in report).toBe(false);
  });

  test("omits a pending spawn identity while exporting authoritative siblings", () => {
    const pendingSessionId = beginOptimisticSpawn(anchorSession());
    visibleSessionIds.push(pendingSessionId);
    try {
      const report = _buildUiStateReport(`/s/${pendingSessionId}`);
      expect(report.activePath).toBe("");
      expect(exportCalls).toEqual([{
        folderKey: FOLDER_KEY,
        liveSessionIds: [SESSION_ID],
      }]);
      const exported = layoutDocumentFromProto(report.layoutDocument!);
      expect(exported.bindings).toEqual([{
        slot_key: "slot-1",
        session_id: SESSION_ID,
      }]);
      expect(exported.bindings.some(
        (binding) => binding.session_id === pendingSessionId,
      )).toBe(false);
      expect(JSON.stringify(report)).not.toContain(pendingSessionId);
    } finally {
      abortOptimisticSpawn(pendingSessionId);
      clearAborted(pendingSessionId);
    }
  });

  test("scrubs a failed optimistic session path after placeholder removal", () => {
    const pendingSessionId = beginOptimisticSpawn(anchorSession());
    visibleSessionIds.push(pendingSessionId);
    failOptimisticSpawn(pendingSessionId, new Error("admission rejected"));
    visibleSessionIds = [SESSION_ID];

    const report = _buildUiStateReport(`/s/${pendingSessionId}`);
    expect(report.activePath).toBe("");
    expect(report.layoutDocument).toBeUndefined();
    expect(exportCalls).toEqual([]);
    expect(JSON.stringify(report)).not.toContain(pendingSessionId);
  });

  test("Solid deferred hydration schedules exactly one authoritative report", () => {
    const path = `/s/${SESSION_ID}`;
    visibleSessionIds = [];
    const [resolvedSessionId, setResolvedSessionId] = createSignal(
      authoritativeUiReportSessionId(path),
    );
    const previousSessionIds: Array<string | null | undefined> = [];
    const scheduleReport = mock(() => {});
    let dispose: () => void = () => undefined;

    createRoot((rootDispose) => {
      dispose = rootDispose;
      createEffect(on(
        resolvedSessionId,
        (currentSessionId, previousSessionId) => {
          previousSessionIds.push(previousSessionId);
          scheduleUiStateReportOnSessionResolution(
            currentSessionId,
            previousSessionId,
            scheduleReport,
          );
        },
        { defer: true },
      ));
    });

    try {
      expect(scheduleReport).toHaveBeenCalledTimes(0);
      expect(_buildUiStateReport(path).activePath).toBe("");

      visibleSessionIds = [SESSION_ID];
      setResolvedSessionId(authoritativeUiReportSessionId(path));

      expect(previousSessionIds).toEqual([undefined]);
      expect(scheduleReport).toHaveBeenCalledTimes(1);
      const hydratedReport = _buildUiStateReport(path);
      expect(hydratedReport.activePath).toBe(path);
      expect(hydratedReport.folderKey).toBe(FOLDER_KEY);
      expect(hydratedReport.layoutDocument).toBeDefined();
    } finally {
      dispose();
    }
  });

  test("Solid deferred hydration skips initial state and accepts tracked null", () => {
    const [resolvedSessionId, setResolvedSessionId] = createSignal<string | null>(
      authoritativeUiReportSessionId(`/s/${SESSION_ID}`),
    );
    const previousSessionIds: Array<string | null | undefined> = [];
    const scheduleReport = mock(() => {});
    let dispose: () => void = () => undefined;

    createRoot((rootDispose) => {
      dispose = rootDispose;
      createEffect(on(
        resolvedSessionId,
        (currentSessionId, previousSessionId) => {
          previousSessionIds.push(previousSessionId);
          scheduleUiStateReportOnSessionResolution(
            currentSessionId,
            previousSessionId,
            scheduleReport,
          );
        },
        { defer: true },
      ));
    });

    try {
      expect(scheduleReport).toHaveBeenCalledTimes(0);
      setResolvedSessionId(null);
      expect(scheduleReport).toHaveBeenCalledTimes(0);
      setResolvedSessionId(SESSION_ID);
      expect(previousSessionIds).toEqual([undefined, null]);
      expect(scheduleReport).toHaveBeenCalledTimes(1);
    } finally {
      dispose();
    }
  });

  test("reports an off-terminal path without exporting a layout", () => {
    const report = _buildUiStateReport("/settings/machines");
    expect(report).toMatchObject({
      tabId: "tab-current",
      activePath: "/settings/machines",
      folderKey: "",
    });
    expect(report.layoutDocument).toBeUndefined();
    expect(exportCalls).toEqual([]);
  });
});
