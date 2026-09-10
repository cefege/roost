// Terminal-deck split rejection containment.
// The coordinator can reject a keeper update gate while a split is spawning.
// This pins the command-handler boundary: report the failure without layout or navigation mutation.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { Layout } from "../src/store/paneLayout.ts";

const splitFailure = new Error("coordinator keeper update preparation in progress");
const session = {
  id: "focused-session",
  worker_fp: "worker-a",
  status: "open",
} as never;
const rootStore = { sessions: { "focused-session": session } };

const spawnShellDetailed = mock(async () => {
  throw splitFailure;
});
const addToast = mock((_message: string, _kind: string) => {});

mock.module("@roost/shared/diag", () => ({ diag: () => undefined }));
mock.module("../src/store/root.ts", () => ({ rootStore }));
mock.module("../src/components/TerminalComposeButton.tsx", () => ({
  releaseActiveComposeFocus: () => undefined,
}));
mock.module("../src/lib/spawnSession.ts", () => ({
  spawnShellDetailed,
  spawnInWorkspaceDetailed: async () => ({ sessionId: "workspace-session" }),
  waitForSession: async () => undefined,
  maybeAutoLaunchAgent: () => undefined,
}));
mock.module("../src/store/optimisticSpawn.ts", () => ({
  beginOptimisticSpawn: () => "optimistic-session",
  clearAborted: () => undefined,
  endOptimisticSpawn: () => undefined,
  settleOptimisticSpawnAdmission: () => undefined,
  waitForMountedSpawnMeasurement: async () => undefined,
  wasAborted: () => false,
}));
mock.module("../src/store/auth-boundary.ts", () => ({
  captureAuthResourceToken: () => "current-auth",
  isCurrentAuthResourceToken: () => true,
}));
mock.module("../src/connect.ts", () => ({
  coordClient: { sessionsKill: async () => undefined },
}));
mock.module("../src/store/paneLayout.ts", () => ({
  findLeafOfTab: () => null,
  focusPane: <T>(layout: T) => layout,
  moveTab: <T>(layout: T) => layout,
  reorderTab: <T>(layout: T) => layout,
  selectTab: <T>(layout: T) => layout,
  setRatio: <T>(layout: T) => layout,
  splitLeaf: <T>(layout: T) => layout,
}));
mock.module("../src/store/spotlight.ts", () => ({
  clearSpotlight: () => undefined,
  setSpotlightSessionId: () => undefined,
  spotlightSessionId: () => null,
}));
mock.module("../src/lib/dropZones.ts", () => ({
  tileTargetFor: () => null,
  zoneRect: () => ({ x: 0, y: 0, w: 0, h: 0 }),
  zoneToSplit: () => null,
}));
mock.module("../src/lib/deckOps.ts", () => ({
  closeSessionOp: () => undefined,
  focusPaneOp: () => undefined,
}));
mock.module("../src/lib/resizeDrag.ts", () => ({ pulseArrange: () => undefined }));
mock.module("../src/store/paneLayoutPresets.ts", () => ({
  arrangeLayout: <T>(_kind: unknown, layout: T) => layout,
}));
mock.module("../src/lib/windowSizeClass.ts", () => ({ isCompact: () => false }));
mock.module("../src/lib/deckRouteSelection.ts", () => ({
  syncDeckPaneFocus: (_layout: unknown, _paneId: string, _compact: boolean, commit: () => void) => commit(),
}));
mock.module("../src/lib/folderKey.ts", () => ({ folderPathOf: () => "/workspace" }));
mock.module("../src/lib/uiStateReport.ts", () => ({ scheduleUiStateReport: () => undefined }));
mock.module("../src/store/toastStore.ts", () => ({ addToast }));
mock.module("../src/components/terminal-deck-shortcuts.ts", () => ({
  bindTerminalDeckShortcuts: () => undefined,
}));

// Dynamic import keeps all operation dependencies behind their Bun module mocks.
const { createTerminalDeckOperations } = await import("../src/components/terminal-deck-operations.ts");

async function invokeDiscardedAction(action: () => Promise<void>): Promise<Event[]> {
  const unhandled: Event[] = [];
  const observeUnhandledRejection = (event: Event): void => {
    event.preventDefault();
    unhandled.push(event);
  };
  globalThis.addEventListener("unhandledrejection", observeUnhandledRejection);
  try {
    const operation = action();
    void operation;
    await operation;
    await Promise.resolve();
    return unhandled;
  } finally {
    globalThis.removeEventListener("unhandledrejection", observeUnhandledRejection);
  }
}

function focusedLayout(): Layout {
  return {
    root: {
      kind: "leaf",
      paneId: "focused-pane",
      tabs: ["focused-session"],
      selectedTab: "focused-session",
    },
    focusedPaneId: "focused-pane",
  };
}

describe("createTerminalDeckOperations split", () => {
  beforeEach(() => {
    spawnShellDetailed.mockReset();
    spawnShellDetailed.mockImplementation(async () => {
      throw splitFailure;
    });
    addToast.mockClear();
  });

  test("contains a rejected coordinator update gate and reports the split failure", async () => {
    const layout = focusedLayout();
    const apply = mock((_transform: (current: Layout) => Layout) => {});
    const navigate = mock((_href: string) => {});
    const operations = createTerminalDeckOperations(
      { activeSessionId: "focused-session", surfaceVisible: true },
      {
        activeSession: () => session,
        apply,
        folderKey: () => "worker-a:/workspace",
        layout: () => layout,
        liveIds: () => ["focused-session"],
        navigate,
        opsCtx: {
          activeSessionId: () => "focused-session",
          folderKey: () => "worker-a:/workspace",
          layout: () => layout,
          navigate,
        },
        selectSession: () => undefined,
        setDragRatios: (() => undefined) as never,
        setDropOverlay: (() => undefined) as never,
        size: () => ({ w: 1200, h: 800 }),
        spotlightPane: () => null,
        stripH: () => 35,
        view: () => ({
          panes: [{
            paneId: "focused-pane",
            rect: { x: 0, y: 0, w: 1200, h: 800 },
            tabIds: ["focused-session"],
            selectedTab: "focused-session",
            focused: true,
          }],
        }),
      } as never,
      () => undefined,
    );

    const unhandled = await invokeDiscardedAction(() => operations.split("row"));

    expect(unhandled).toEqual([]);
    expect(apply).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(addToast).toHaveBeenCalledWith(
      "Split terminal failed: coordinator keeper update preparation in progress",
      "err",
    );
  });
});
