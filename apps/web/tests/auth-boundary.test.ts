// Credential-boundary teardown for the authenticated client-state owner.
// Overlays, runtime timer owners, the durable replay cursor, and every
// authenticated root replica must be released before another socket can open.
// Tokens captured before the boundary must stop being current afterwards.

import { expect, test } from "bun:test";
import { activeRenameDialog, openRenameDialog } from "../src/store/renameDialog.ts";
import { queueTaskDialogStore } from "../src/store/queueTaskDialog.ts";
import { addTransfer, transfers } from "../src/store/transfers.ts";
import { setSpotlightSessionId, spotlightSessionId } from "../src/store/spotlight.ts";
import { registerAuthBoundContentSearch } from "../src/lib/globalContentSearchRuntime.ts";
import {
  _resetTerminalFindIntentsForTest,
  registerTerminalFind,
  requestTerminalFind,
} from "../src/lib/terminalFindIntent.ts";

const local = new Map<string, string>([
  ["roost.syncLastEventId", "41"],
  ["roost.coordinatorUrl", "http://127.0.0.1:65000"],
]);
const session = new Map<string, string>();

function storageFor(values: Map<string, string>) {
  return {
    get length() { return values.size; },
    key: (index: number) => [...values.keys()][index] ?? null,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
}

Object.assign(globalThis, {
  localStorage: storageFor(local),
  sessionStorage: storageFor(session),
  location: {
    origin: "http://127.0.0.1:65000",
    protocol: "http:",
    host: "127.0.0.1:65000",
    hostname: "127.0.0.1",
    href: "http://127.0.0.1:65000/",
    pathname: "/",
    search: "",
    hash: "",
  },
});

// Install the fake location and storage before these modules read browser
// globals during evaluation (module-loading boundary — ts-no-dynamic-import
// exception).
const root = await import("../src/store/root.ts");
const boundary = await import("../src/store/auth-boundary.ts");
const stream = await import("../src/store/terminal-stream.ts");
const terminalState = await import("../src/store/terminal-stream-state.ts");
const hydrated = await import("../src/store/sync-hydrated.ts");
const frame = await import("../src/store/sync-frame.ts");

test("suspending authenticated client state releases overlays, runtime owners, and root replicas", () => {
  // The persisted cursor is intentionally present before the boundary runs.
  expect(frame.lastSeenSyncEventId()).toBe(41);
  root.setRootStore("coord_identity", {
    git_sha: "test",
    public_url: "http://127.0.0.1:65000",
  });
  root.setRootStore("browser_unauthorized", true);
  root.setRootStore("workers", { "worker-a": {} as never });
  root.setRootStore("sessions", { "session-a": {} as never });
  root.setRootStore("workspaces", { "workspace-a": {} as never });
  root.setRootStore("tasks", { "task-a": {} as never });
  root.setRootStore("mcp_relays", { "relay-a": {} as never });
  root.setRootStore("pair_requests", {
    "pair-a": {
      ephemeral_id: "pair-a", label: "A", created_at_ms: 1,
      userAgent: "", clientBrowser: "", clientOs: "", clientDeviceType: "",
      sourceIp: "", countryCode: "", region: "", city: "",
      edgeIdentityProvider: "", edgeIdentity: "", edgeIdentityVerified: false,
      expiresAtMs: 0,
    },
  });
  root.setRootStore("agent_status", { "session-a": {} as never });
  root.setRootStore("terminal_title", { "session-a": "A" });
  root.setRootStore("last_activity", { "session-a": 1 });
  root.setRootStore("session_viewers", {
    "session-a": [{ fp: "viewer-a", cols: 80, rows: 24 }],
  });
  hydrated.setSessionsHydrated(true);
  hydrated.setWorkersHydrated(true);
  hydrated.setTerminalBootstrapStage("ready");

  openRenameDialog({
    currentTitle: "Retired terminal",
    hasCustom: true,
    sessionId: "session-a",
  });
  queueTaskDialogStore.open({
    cwd: "/retired",
    body: "retired task",
    workerFp: "worker-a",
  });
  addTransfer({
    id: "retired-transfer",
    name: "retired.txt",
    dir: "down",
    bytes_total: 1,
    state: "active",
  });
  setSpotlightSessionId("session-a");

  const contentSearchTransitions = { resets: 0, resumes: 0 };
  const unregisterContentSearch = registerAuthBoundContentSearch({
    resetForAuthBoundary: () => { contentSearchTransitions.resets++; },
    resumeAfterAuthBoundary: () => { contentSearchTransitions.resumes++; },
  });
  const retiredFindQueries: string[] = [];
  registerTerminalFind("session-find", {
    openFind: () => {},
    setQuery: (next) => { retiredFindQueries.push(next); },
  });
  requestTerminalFind("cold-find", "retired needle");

  const retainedView = stream.createTerminalView("session-a");
  expect(terminalState.terminalSessions.has("session-a")).toBe(true);

  boundary.suspendAuthenticatedClientState();

  // Overlays that retain a path, name, or action are closed and emptied.
  expect(activeRenameDialog()).toBeNull();
  expect(queueTaskDialogStore.isOpen()).toBe(false);
  expect(queueTaskDialogStore.prefillCwd()).toBeUndefined();
  expect(queueTaskDialogStore.prefillBody()).toBeUndefined();
  expect(queueTaskDialogStore.prefillWorkerFp()).toBeUndefined();
  expect(Object.keys(transfers)).toEqual([]);
  expect(spotlightSessionId()).toBeNull();

  // Content search is suspended, not resumed: the next credential resumes it.
  expect(contentSearchTransitions).toEqual({ resets: 1, resumes: 0 });

  // A retired pane callback and a cold find intent cannot reach a new pane.
  requestTerminalFind("session-find", "next needle");
  expect(retiredFindQueries).toEqual([]);
  const coldFindQueries: string[] = [];
  const unregisterColdFind = registerTerminalFind("cold-find", {
    openFind: () => {},
    setQuery: (next) => { coldFindQueries.push(next); },
  });
  expect(coldFindQueries).toEqual([]);
  unregisterColdFind();
  unregisterContentSearch();
  _resetTerminalFindIntentsForTest();

  // Every authenticated root slice, terminal replica, hydration marker, and
  // durable replay cursor is gone before the next socket opens.
  expect(Object.keys(root.rootStore.workers)).toEqual([]);
  expect(Object.keys(root.rootStore.sessions)).toEqual([]);
  expect(Object.keys(root.rootStore.workspaces)).toEqual([]);
  expect(Object.keys(root.rootStore.tasks)).toEqual([]);
  expect(Object.keys(root.rootStore.mcp_relays)).toEqual([]);
  expect(Object.keys(root.rootStore.pair_requests)).toEqual([]);
  expect(Object.keys(root.rootStore.agent_status)).toEqual([]);
  expect(Object.keys(root.rootStore.terminal_title)).toEqual([]);
  expect(Object.keys(root.rootStore.last_activity)).toEqual([]);
  expect(Object.keys(root.rootStore.session_viewers)).toEqual([]);
  expect(root.rootStore.browser_unauthorized).toBe(false);
  expect(terminalState.terminalSessions.size).toBe(0);
  expect(hydrated.sessionsHydrated()).toBe(false);
  expect(hydrated.workersHydrated()).toBe(false);
  expect(hydrated.terminalBootstrapStage()).toBe("sync");
  expect(frame.lastSeenSyncEventId()).toBe(0);
  expect(local.has("roost.syncLastEventId")).toBe(false);

  // Coordinator discovery survives so the pairing surfaces still render.
  expect(root.rootStore.coord_identity?.git_sha).toBe("test");

  // A retained view handle cannot resurrect the released replica.
  retainedView.setViewport({ cols: 80, rows: 24 });
  expect(terminalState.terminalSessions.has("session-a")).toBe(false);
});

test("a resource token captured before the credential boundary stops being current", () => {
  const heldToken = boundary.captureAuthResourceToken();
  expect(boundary.isCurrentAuthResourceToken(heldToken)).toBe(true);

  boundary.suspendAuthenticatedClientState();

  // The held token belongs to the retired credential; work resumed under the
  // new one captures a token that is current again.
  expect(boundary.isCurrentAuthResourceToken(heldToken)).toBe(false);
  expect(boundary.isCurrentAuthResourceToken(boundary.captureAuthResourceToken())).toBe(true);
});
