// Import-controller tests own the seam between a picked layout file and the
// confirm dialog's props: which folder's live set fences the read, what the
// dropped-session count reports, and that Apply commits the previewed tree.
// File picking, folder membership, dashboard identity, toasts, and spotlight
// are mocked so no DOM, coordinator traffic, or root store is required.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import { createRoot } from "solid-js";
import { allLeaves } from "../src/store/paneLayout.ts";
import { sessionHref } from "../src/routes.ts";
import type { LayoutDocumentControls } from "../src/lib/layoutDocumentControls.ts";
import type { LayoutDocumentNode, LayoutDocumentV1 } from "@roost/shared/layout-document";

const FOLDER_KEY = "worker::/work";
const PANE_COUNT = 10;
const DEAD_PANE = 4;
const sessionId = (index: number): string =>
  `00000000-0000-4000-8000-0000000000${String(index).padStart(2, "0")}`;
const ALL_SESSIONS = Array.from({ length: PANE_COUNT }, (_, idx) => sessionId(idx + 1));
const LIVE_SESSIONS = ALL_SESSIONS.filter((id) => id !== sessionId(DEAD_PANE));

const storedValues: Record<string, string> = {};
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: {
    getItem: (key: string) => storedValues[key] ?? null,
    setItem: (key: string, value: string) => { storedValues[key] = value; },
    removeItem: (key: string) => { delete storedValues[key]; },
    clear: () => { for (const key of Object.keys(storedValues)) delete storedValues[key]; },
    key: () => null,
    length: 0,
  } as Storage,
});

let pickedFile: File | null = null;
let liveSessionIds: readonly string[] = LIVE_SESSIONS;

mock.module("../src/lib/layoutDocumentFile.ts", () => ({
  pickLayoutDocumentFile: (accept: (file: File) => void) => {
    if (pickedFile) accept(pickedFile);
  },
  downloadLayoutDocument: () => {},
  serializeLayoutDocument: () => "{}",
}));
mock.module("../src/store/selectors.ts", () => ({
  liveSessionIdsForFolder: (folderKey: string) => (folderKey === FOLDER_KEY ? liveSessionIds : []),
}));
mock.module("../src/store/dashboard-selection.ts", () => ({
  captureDashboardResourceToken: () => "dashboard-token",
  isCurrentDashboardResourceToken: () => true,
}));
mock.module("../src/store/toastStore.ts", () => ({ addToast: () => {} }));
mock.module("../src/store/spotlight.ts", () => ({ clearSpotlight: () => {} }));

// Install localStorage and the mocks before paneLayoutStore reads storage at
// module initialization (module-loading boundary — ts-no-dynamic-import exception).
const paneStore = await import("../src/store/paneLayoutStore.ts");
const controls = await import("../src/lib/layoutDocumentControls.ts");

/** A right-leaning spine of `PANE_COUNT` leaves, one saved session each. */
function savedDocument(): LayoutDocumentV1 {
  const leafFor = (index: number): LayoutDocumentNode => ({
    kind: "leaf",
    leaf_key: `leaf-${index}`,
    slot_keys: [`slot-${index}`],
    selected_slot_key: `slot-${index}`,
  });
  let root: LayoutDocumentNode = leafFor(1);
  for (let index = 2; index <= PANE_COUNT; index++) {
    root = { kind: "split", direction: "row", ratio: 0.5, first: root, second: leafFor(index) };
  }
  return {
    schema_version: 1,
    root,
    focused_leaf_key: `leaf-${PANE_COUNT}`,
    bindings: ALL_SESSIONS.map((session, idx) => ({
      slot_key: `slot-${idx + 1}`,
      session_id: session,
    })),
  };
}

function createImportControls(
  navigated: string[],
): { controls: LayoutDocumentControls; dispose: () => void } {
  let dispose = (): void => {};
  const created = createRoot((disposeRoot) => {
    dispose = disposeRoot;
    return controls.createLayoutDocumentControls({
      folderKey: () => FOLDER_KEY,
      navigate: (href) => { navigated.push(href); },
    });
  });
  return { controls: created, dispose };
}

async function openPreview(document: unknown, navigated: string[] = []): Promise<{
  controls: LayoutDocumentControls;
  dispose: () => void;
}> {
  pickedFile = new File([JSON.stringify(document)], "team-grid.json", {
    type: "application/json",
  });
  const created = createImportControls(navigated);
  created.controls.importLayout();
  for (let idx = 0; idx < 50 && created.controls.preview()?.reading !== false; idx++) {
    await Promise.resolve();
  }
  return created;
}

beforeEach(() => {
  paneStore._flushPendingPersist();
  paneStore.clearPaneLayoutsForLogout();
  localStorage.clear();
  liveSessionIds = LIVE_SESSIONS;
  pickedFile = null;
});

describe("layout import preview", () => {
  test("reports the sessions it dropped and previews only the live ones", async () => {
    const { controls: imported, dispose } = await openPreview(savedDocument());
    try {
      const preview = imported.preview();
      expect(preview?.error).toBeNull();
      expect(preview?.fileName).toBe("team-grid.json");
      expect(preview?.droppedSessionCount).toBe(1);
      expect(preview?.document?.bindings.map((binding) => binding.session_id))
        .toEqual(LIVE_SESSIONS);
    } finally {
      dispose();
    }
  });

  test("reports nothing dropped when every saved session is live", async () => {
    liveSessionIds = ALL_SESSIONS;
    const document = savedDocument();
    const { controls: imported, dispose } = await openPreview(document);
    try {
      expect(imported.preview()?.droppedSessionCount).toBe(0);
      expect(imported.preview()?.document).toEqual(document);
    } finally {
      dispose();
    }
  });

  test("applying a degraded preview commits the surviving panes and navigates", async () => {
    const navigated: string[] = [];
    const { controls: imported, dispose } = await openPreview(savedDocument(), navigated);
    try {
      imported.applyImportedLayout();
      expect(imported.preview()).toBeNull();
      const resolved = paneStore.resolveLayout(FOLDER_KEY, [...LIVE_SESSIONS]);
      const leaves = allLeaves(resolved.root);
      expect(leaves.map((leaf) => leaf.tabs)).toEqual(LIVE_SESSIONS.map((session) => [session]));
      expect(navigated).toEqual([sessionHref(sessionId(PANE_COUNT))]);
    } finally {
      dispose();
    }
  });

  test("surfaces a read failure instead of a preview", async () => {
    const { controls: imported, dispose } = await openPreview({ schema_version: 2 });
    try {
      expect(imported.preview()?.document).toBeNull();
      expect(imported.preview()?.error).toBeTruthy();
      expect(paneStore._paneLayoutStoreDebugSnapshot().persistScheduled).toBe(false);
    } finally {
      dispose();
    }
  });
});
