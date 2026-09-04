// Dashboard cutover tests for server-confirmed selection and auth rejection.
// Deferred coordinator responses pin request-generation and Sync-hold races.
// Root, overlay, and transport cleanup run through the public selection API.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AuthDashboardAccessResponseSchema,
  type AuthDashboardAccessResponse,
} from "@roost/shared/proto/coordinator_pb";
import { activeRenameDialog, openRenameDialog } from "../src/store/renameDialog.ts";
import { queueTaskDialogStore } from "../src/store/queueTaskDialog.ts";
import { openTransferDialog, transferDialogOpen } from "../src/lib/transferDialog.ts";
import { addTransfer, transfers } from "../src/store/transfers.ts";
import { setSpotlightSessionId, spotlightSessionId } from "../src/store/spotlight.ts";

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

let interceptedRequest: Request | null = null;
const interceptedFetch: (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response> = async (input, init) => {
  interceptedRequest = input instanceof Request ? input : new Request(input, init);
  throw new Error("stop after request interception");
};
Object.assign(globalThis, { fetch: interceptedFetch });


// The remaining modules read browser globals during evaluation, so install the
// fake location and storage before importing them.
const root = await import("../src/store/root.ts");
const selection = await import("../src/store/dashboard-selection.ts");
const stream = await import("../src/store/terminal-stream.ts");
const terminalState = await import("../src/store/terminal-stream-state.ts");
const hydrated = await import("../src/store/sync-hydrated.ts");
const frame = await import("../src/store/sync-frame.ts");
const connect = await import("../src/connect.ts");
const sync = await import("../src/store/sync.ts");
const headers = await import("@roost/shared/wire/headers");

const accessA = {
  account_id: "account-a",
  organizations: [{ id: "org-a", slug: "org-a", name: "Organization A", role: "owner" }],
  dashboards: [{
    id: "dashboard-a",
    organization_id: "org-a",
    slug: "dashboard-a",
    name: "Dashboard A",
    organization_role: "owner",
    dashboard_role: "admin",
  }],
  selected_dashboard_id: "dashboard-a",
  capabilities: ["dashboard:member", "dashboard:admin"],
} as const;

const accessB = {
  account_id: "account-a",
  organizations: [{ id: "org-b", slug: "org-b", name: "Organization B", role: "member" }],
  dashboards: [{
    id: "dashboard-b",
    organization_id: "org-b",
    slug: "dashboard-b",
    name: "Dashboard B",
    organization_role: "member",
    dashboard_role: "member",
  }],
  selected_dashboard_id: "dashboard-b",
  capabilities: ["dashboard:member"],
} as const;

function protoAccess(
  snapshot: typeof accessA | typeof accessB,
): AuthDashboardAccessResponse {
  return create(AuthDashboardAccessResponseSchema, {
    accountId: snapshot.account_id,
    organizations: snapshot.organizations.map((organization) => ({
      id: organization.id,
      slug: organization.slug,
      name: organization.name,
      role: organization.role,
    })),
    dashboards: snapshot.dashboards.map((dashboard) => ({
      id: dashboard.id,
      organizationId: dashboard.organization_id,
      slug: dashboard.slug,
      name: dashboard.name,
      organizationRole: dashboard.organization_role,
      dashboardRole: dashboard.dashboard_role,
    })),
    selectedDashboardId: snapshot.selected_dashboard_id,
    capabilities: [...snapshot.capabilities],
  });
}

function openScopedOverlays(): void {
  openRenameDialog({
    currentTitle: "Old terminal",
    hasCustom: true,
    sessionId: "session-a",
  });
  queueTaskDialogStore.open({ cwd: "/old/scope", body: "old task" });
  openTransferDialog();
  addTransfer({
    id: "old-transfer",
    name: "old-scope.txt",
    dir: "down",
    bytes_total: 1,
    state: "active",
  });
  setSpotlightSessionId("session-a");
}

function expectScopedOverlaysCleared(): void {
  expect(activeRenameDialog()).toBeNull();
  expect(queueTaskDialogStore.isOpen()).toBe(false);
  expect(queueTaskDialogStore.prefillCwd()).toBeUndefined();
  expect(queueTaskDialogStore.prefillBody()).toBeUndefined();
  expect(transferDialogOpen()).toBe(false);
  expect(Object.keys(transfers)).toEqual([]);
  expect(spotlightSessionId()).toBeNull();
}

test("server-confirmed dashboard switch clears scope atomically and invalidates stale deep links", async () => {
  // A coordinator-confirmed initial membership is allowed. The persisted
  // cursor is intentionally present before the switch boundary runs.
  expect(root.setDashboardAccess(accessA)).toBe(true);
  expect(root.rootStore.selected_dashboard_id).toBe("dashboard-a");
  expect(frame.lastSeenSyncEventId()).toBe(41);

  root.setRootStore("workers", { "worker-a": {} as never });
  root.setRootStore("sessions", { "session-a": {} as never });
  root.setRootStore("workspaces", { "workspace-a": {} as never });
  root.setRootStore("tasks", { "task-a": {} as never });
  root.setRootStore("mcp_relays", { "relay-a": {} as never });
  root.setRootStore("pair_requests", { "pair-a": { ephemeral_id: "pair-a", label: "A", created_at_ms: 1 } });
  root.setRootStore("agent_status", { "session-a": {} as never });
  root.setRootStore("terminal_title", { "session-a": "A" });
  root.setRootStore("last_activity", { "session-a": 1 });
  root.setRootStore("session_viewers", { "session-a": [{ fp: "viewer-a", cols: 80, rows: 24 }] });
  hydrated.setSessionsHydrated(true);
  hydrated.setTerminalBootstrapStage("ready");

  const staleRoute = selection.captureDashboardResourceToken();
  const staleView = stream.createTerminalView("session-a");
  expect(terminalState.terminalSessions.has("session-a")).toBe(true);

  // A locally named dashboard that is absent from the server snapshot cannot
  // alter selection, persistence, or any already-loaded dashboard data.
  expect(selection.commitServerConfirmedDashboardAccess({
    ...accessA,
    selected_dashboard_id: "dashboard-b",
  })).toBe(false);
  expect(root.rootStore.selected_dashboard_id).toBe("dashboard-a");
  expect(root.rootStore.sessions["session-a"]).toBeDefined();
  expect(selection.isCurrentDashboardResourceToken(staleRoute)).toBe(true);

  // Only the server's membership-confirmed B snapshot crosses the boundary.
  expect(selection.commitServerConfirmedDashboardAccess(accessB)).toBe(true);
  expect(root.rootStore.selected_dashboard_id).toBe("dashboard-b");
  expect(root.rootStore.account_id).toBe("account-a");
  expect(Object.keys(root.rootStore.organizations)).toEqual(["org-b"]);
  expect(Object.keys(root.rootStore.dashboards)).toEqual(["dashboard-b"]);
  expect(root.rootStore.effective_capabilities).toEqual(["dashboard:member"]);
  expect(local.get("roost.dashboardId")).toBe("dashboard-b");

  // Every dashboard-scoped root slice, terminal replica/timer owner, Sync
  // hydration marker, and durable replay cursor is gone before redial.
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
  expect(terminalState.terminalSessions.size).toBe(0);
  expect(hydrated.sessionsHydrated()).toBe(false);
  expect(hydrated.terminalBootstrapStage()).toBe("sync");
  expect(frame.lastSeenSyncEventId()).toBe(0);
  expect(local.has("roost.syncLastEventId")).toBe(false);

  // The old /s/session-a route and its retained view handle cannot resurrect a
  // terminal action after the reset; it has no current scope token or replica.
  expect(selection.isCurrentDashboardResourceToken(staleRoute)).toBe(false);
  expect(root.rootStore.sessions["session-a"]).toBeUndefined();
  staleView.setViewport({ cols: 80, rows: 24 });
  expect(terminalState.terminalSessions.has("session-a")).toBe(false);

  // Both HTTP and raw Sync carry only the confirmed B ID, not the rejected
  // local candidate or any identifier retained by the stale deep link.
  const client = connect.makeCoordinatorClientForSigner(async () => "test-jwt");
  try {
    await client.workersList({});
  } catch {
    // The focused fetch fake stops after observing interceptor headers.
  }
  expect(interceptedRequest?.headers.get(headers.X_ROOST_DASHBOARD_ID)).toBe("dashboard-b");

  const syncUrl = sync.buildSyncWebSocketUrl(
    location.origin,
    frame.lastSeenSyncEventId(),
    "tab-dashboard-switch",
    root.rootStore.selected_dashboard_id,
  );
  expect(syncUrl).toContain("dashboard=dashboard-b");
});

test("same-candidate dashboard bootstrap is single-flight", async () => {
  const original = connect.coordClient.authDashboardAccess;
  let calls = 0;
  const deferred = Promise.withResolvers<AuthDashboardAccessResponse>();
  // Connect's generated client surface is structurally mutable at runtime; the
  // test replaces one method to hold the response at the single-flight barrier.
  const mutableCoordClient = connect.coordClient as unknown as {
    authDashboardAccess: typeof original;
  };
  mutableCoordClient.authDashboardAccess = (async () => {
    calls++;
    return deferred.promise;
  }) as typeof original;

  try {
    const first = selection.bootstrapDashboardAccess();
    const second = selection.bootstrapDashboardAccess();
    expect(calls).toBe(1);
    deferred.resolve(protoAccess(accessA));
    expect(await Promise.all([first, second])).toEqual([true, true]);
    expect(root.rootStore.selected_dashboard_id).toBe("dashboard-a");
    expect(calls).toBe(1);
  } finally {
    mutableCoordClient.authDashboardAccess = original;
  }
});

test("refresh cannot supersede a switch and switch start clears scoped overlays", async () => {
  selection.clearAccountScopedClientStateForLogout();
  expect(root.setDashboardAccess(accessA)).toBe(true);
  root.setRootStore("sessions", { "session-a": {} as never });
  openScopedOverlays();

  const original = connect.coordClient.authDashboardAccess;
  const switchResponse = Promise.withResolvers<AuthDashboardAccessResponse>();
  const refreshResponse = Promise.withResolvers<AuthDashboardAccessResponse>();
  let calls = 0;
  const mutableCoordClient = connect.coordClient as unknown as {
    authDashboardAccess: typeof original;
  };
  mutableCoordClient.authDashboardAccess = (async () => {
    calls++;
    return calls === 1 ? switchResponse.promise : refreshResponse.promise;
  }) as typeof original;
  let switchRequest: Promise<boolean> | null = null;
  let refreshRequest: Promise<boolean> | null = null;

  try {
    switchRequest = selection.selectDashboardFromServer("dashboard-b");
    expect(calls).toBe(1);
    expect(sync._syncDashboardSwitchHeld()).toBe(true);
    expect(Object.keys(root.rootStore.sessions)).toEqual([]);
    expectScopedOverlaysCleared();

    refreshRequest = selection.refreshDashboardAccess();
    expect(calls).toBe(1);
    refreshResponse.resolve(protoAccess(accessA));
    switchResponse.resolve(protoAccess(accessB));

    expect(await Promise.all([switchRequest, refreshRequest])).toEqual([true, false]);
    expect(root.rootStore.selected_dashboard_id).toBe("dashboard-b");
    expect(Object.keys(root.rootStore.organizations)).toEqual(["org-b"]);
    expect(Object.keys(root.rootStore.dashboards)).toEqual(["dashboard-b"]);
    expect(sync._syncDashboardSwitchHeld()).toBe(false);
  } finally {
    switchResponse.resolve(protoAccess(accessB));
    refreshResponse.resolve(protoAccess(accessA));
    await switchRequest?.catch(() => false);
    await refreshRequest?.catch(() => false);
    mutableCoordClient.authDashboardAccess = original;
    selection.clearAccountScopedClientStateForLogout();
  }
});

test("self-hosted auth rejection clears access, overlays, and an active switch hold", async () => {
  selection.clearAccountScopedClientStateForLogout();
  expect(root.setDashboardAccess(accessA)).toBe(true);
  root.setRootStore("coord_identity", {
    git_sha: "test",
    public_url: "http://127.0.0.1:65000",
    public_listener: false,
    saas_mode: false,
  });
  root.setRootStore("sessions", { "session-a": {} as never });
  selection.rememberDashboardSelectionHint("dashboard-a");

  const original = connect.coordClient.authDashboardAccess;
  const switchResponse = Promise.withResolvers<AuthDashboardAccessResponse>();
  const mutableCoordClient = connect.coordClient as unknown as {
    authDashboardAccess: typeof original;
  };
  mutableCoordClient.authDashboardAccess = (async () => switchResponse.promise) as typeof original;
  let switchRequest: Promise<boolean> | null = null;

  try {
    switchRequest = selection.selectDashboardFromServer("dashboard-b");
    expect(sync._syncDashboardSwitchHeld()).toBe(true);
    openScopedOverlays();

    selection.suspendDashboardScopedClientState();

    expect(root.rootStore.coord_identity?.saas_mode).toBe(false);
    expect(root.rootStore.account_id).toBeNull();
    expect(Object.keys(root.rootStore.organizations)).toEqual([]);
    expect(Object.keys(root.rootStore.dashboards)).toEqual([]);
    expect(root.rootStore.selected_dashboard_id).toBeNull();
    expect(root.rootStore.effective_capabilities).toEqual([]);
    expect(Object.keys(root.rootStore.sessions)).toEqual([]);
    expect(local.has("roost.dashboardId")).toBe(false);
    expectScopedOverlaysCleared();
    expect(sync._syncDashboardSwitchHeld()).toBe(false);

    switchResponse.resolve(protoAccess(accessB));
    expect(await switchRequest).toBe(false);
    expect(root.rootStore.selected_dashboard_id).toBeNull();
    expect(sync._syncDashboardSwitchHeld()).toBe(false);
  } finally {
    switchResponse.resolve(protoAccess(accessB));
    await switchRequest?.catch(() => false);
    mutableCoordClient.authDashboardAccess = original;
    selection.clearAccountScopedClientStateForLogout();
  }
});
