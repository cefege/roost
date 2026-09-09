// Command-palette catalog tests cover the closed action set, contextual
// visibility/delegation, projection-backed session rows, and cache fencing.
// Dependencies are mocked only at their existing action-owner boundaries.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { Navigator } from "@solidjs/router";
import type { WorkerFp } from "@roost/shared/wire";
import {
  CORE_ACTION_DEFINITIONS,
  buildDefaultItems,
  matchesQuery,
  clearCommandPaletteCacheForAccountBoundary,
  type CommandPaletteContext,
  type CommandPaletteDataDeps,
  type PaletteItem,
} from "../src/components/CommandPalette.data.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  clearQueueTaskDialogForLogout,
  queueTaskDialogStore,
} from "../src/store/queueTaskDialog.ts";

const WORKER_FP = "worker-a" as WorkerFp;
let projectedDocuments: Array<{
  sessionId: string;
  href: string;
  displayTitle: string;
  workerLabel: string;
  searchText: string;
  available: boolean;
}> = [];
const spawnSessionSibling = mock(async (
  _session: { worker_fp: WorkerFp; cwd: string },
  _navigate: Navigator,
) => {});

const paletteDeps: CommandPaletteDataDeps = {
  navigationSearchDocuments: () => projectedDocuments,
  spawnSessionSibling,
};

function paletteContext(
  overrides: Partial<CommandPaletteContext> = {},
): CommandPaletteContext {
  return {
    pathname: "/s/session-a",
    authGeneration: rootStore.auth_generation,
    activeSession: {
      id: "session-a",
      workerFp: WORKER_FP,
      cwd: "/work/project-a",
    },
    activeFolder: {
      id: "worker-a::/work/project-a",
      workerFp: WORKER_FP,
      cwd: "/work/project-a",
    },
    workerRoutable: true,
    ...overrides,
  };
}

function itemWithPrefix(items: readonly PaletteItem[], idPrefix: string): PaletteItem {
  const item = items.find((candidate) => candidate.id.startsWith(idPrefix));
  if (!item) throw new Error(`missing palette item ${idPrefix}`);
  return item;
}

beforeEach(() => {
  projectedDocuments = [];
  spawnSessionSibling.mockClear();
  clearQueueTaskDialogForLogout();
  clearCommandPaletteCacheForAccountBoundary();
  setRootStore("auth_generation", 7);
  setRootStore("workspaces", {});
});

test("the closed catalog contains exactly the four contextual core definitions", () => {
  expect(CORE_ACTION_DEFINITIONS.map((definition) => definition.id)).toEqual([
    "core.search.all",
    "core.attention.open",
    "core.task.queue-folder",
    "core.session.new-sibling",
  ]);
  expect(new Set(CORE_ACTION_DEFINITIONS.map((definition) => definition.id)).size).toBe(4);
});

test("palette matching requires every normalized metadata term", () => {
  expect(matchesQuery("Ｆｅａｔｕｒｅ/Search · Build Machine", [
    "feature/search",
    "build",
  ])).toBe(true);
  expect(matchesQuery("Feature/Search · Build Machine", [
    "feature/search",
    "offline",
  ])).toBe(false);
});

test("session rows reuse navigation metadata while workspace rows remain available", () => {
  projectedDocuments = [{
    sessionId: "session-a",
    href: "/s/session-a",
    displayTitle: "Build release",
    workerLabel: "Builder",
    searchText: "build release /work/project-a origin/main pull request 31 port 5173",
    available: false,
  }];
  setRootStore("workspaces", {
    "workspace-a": {
      id: "workspace-a",
      name: "Project A",
      session_ids: ["session-a"],
    } as never,
  });
  const navigate = mock((_href: string, _options?: unknown) => {});

  const items = buildDefaultItems(navigate as unknown as Navigator, paletteContext({ activeSession: null, activeFolder: null, workerRoutable: false }), paletteDeps);

  expect(itemWithPrefix(items, "session:session-a")).toMatchObject({
    label: "Build release",
    hint: "Builder · unavailable",
    search: projectedDocuments[0]!.searchText,
    href: "/s/session-a",
  });
  expect(itemWithPrefix(items, "workspace:workspace-a")).toMatchObject({
    label: "Project A",
    hint: "1 sessions",
    href: "/w/workspace-a",
  });
  expect(itemWithPrefix(items, "core.search.all").href).toBe("/search");
  expect(itemWithPrefix(items, "core.attention.open").href).toBe(
    "/search?scope=attention",
  );
});

describe("contextual action visibility and delegation", () => {
  test("an authenticated operator can queue the active folder and spawn a routable sibling", async () => {
    const navigate = mock((_href: string, _options?: unknown) => {});
    const items = buildDefaultItems(navigate as unknown as Navigator, paletteContext(), paletteDeps);
    expect(items.filter((item) => item.kind === "action")).toHaveLength(4);
    const queueItem = itemWithPrefix(items, "core.task.queue-folder:");
    const siblingItem = itemWithPrefix(items, "core.session.new-sibling:");

    expect(queueItem.id).toContain("worker-a::/work/project-a");
    expect(queueItem.id).toContain("generation:7");
    expect(siblingItem.id).toContain("session-a");
    expect(siblingItem.id).toContain("generation:7");

    await queueItem.action?.();
    expect(queueTaskDialogStore.isOpen()).toBe(true);
    expect(queueTaskDialogStore.prefillCwd()).toBe("/work/project-a");
    expect(queueTaskDialogStore.prefillWorkerFp()).toBe(WORKER_FP);

    await siblingItem.action?.();
    expect(spawnSessionSibling).toHaveBeenCalledWith({
      worker_fp: WORKER_FP,
      cwd: "/work/project-a",
    }, navigate);
  });

  test("an unroutable worker hides sibling spawn without hiding its queued task", () => {
    const items = buildDefaultItems((() => {}) as unknown as Navigator, paletteContext({ workerRoutable: false }), paletteDeps);

    expect(items.some((item) => item.id.startsWith("core.task.queue-folder:"))).toBe(true);
    expect(items.some((item) => item.id.startsWith("core.session.new-sibling:"))).toBe(false);
  });

  test("missing folder and session context leaves only navigation actions", () => {
    const items = buildDefaultItems((() => {}) as unknown as Navigator, paletteContext({
      activeSession: null,
      activeFolder: null,
      workerRoutable: false,
    }), paletteDeps);

    expect(items.filter((item) => item.kind === "action").map((item) => item.id)).toEqual([
      "core.search.all",
      "core.attention.open",
    ]);
  });
});

test("target and generation changes replace contextual cache identities", () => {
  const navigate = (() => {}) as unknown as Navigator;
  const first = buildDefaultItems(navigate, paletteContext(), paletteDeps);
  const firstQueue = itemWithPrefix(first, "core.task.queue-folder:");
  const firstSibling = itemWithPrefix(first, "core.session.new-sibling:");
  const repeated = buildDefaultItems(navigate, paletteContext(), paletteDeps);

  expect(itemWithPrefix(repeated, "core.task.queue-folder:")).toBe(firstQueue);
  expect(itemWithPrefix(repeated, "core.session.new-sibling:")).toBe(firstSibling);

  const retargeted = buildDefaultItems(navigate, paletteContext({
    pathname: "/s/session-b",
    activeSession: {
      id: "session-b",
      workerFp: WORKER_FP,
      cwd: "/work/project-b",
    },
    activeFolder: {
      id: "worker-a::/work/project-b",
      workerFp: WORKER_FP,
      cwd: "/work/project-b",
    },
  }), paletteDeps);
  expect(itemWithPrefix(retargeted, "core.task.queue-folder:").id).toContain(
    "worker-a::/work/project-b",
  );
  expect(itemWithPrefix(retargeted, "core.session.new-sibling:").id).toContain(
    "session-b",
  );
  expect(itemWithPrefix(retargeted, "core.task.queue-folder:")).not.toBe(firstQueue);
  expect(itemWithPrefix(retargeted, "core.session.new-sibling:")).not.toBe(firstSibling);

  setRootStore("auth_generation", 8);
  const regenerated = buildDefaultItems(navigate, paletteContext(), paletteDeps);
  expect(itemWithPrefix(regenerated, "core.task.queue-folder:").id).toContain(
    "generation:8",
  );
  expect(itemWithPrefix(regenerated, "core.session.new-sibling:").id).toContain(
    "generation:8",
  );
});

test("actions captured before an auth generation change no-op", async () => {
  const navigate = mock((_href: string, _options?: unknown) => {});
  const items = buildDefaultItems(navigate as unknown as Navigator, paletteContext(), paletteDeps);
  const staleQueue = itemWithPrefix(items, "core.task.queue-folder:");
  const staleSibling = itemWithPrefix(items, "core.session.new-sibling:");

  setRootStore("auth_generation", 8);
  await staleQueue.action?.();
  await staleSibling.action?.();

  expect(queueTaskDialogStore.isOpen()).toBe(false);
  expect(queueTaskDialogStore.prefillWorkerFp()).toBeUndefined();
  expect(spawnSessionSibling).not.toHaveBeenCalled();
  expect(navigate).not.toHaveBeenCalled();
});
