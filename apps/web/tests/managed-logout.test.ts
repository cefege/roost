import { describe, expect, test } from "bun:test";

const local = new Map<string, string>([
  ["roost.composerDrafts.v1", JSON.stringify({ "session-a": "secret draft" })],
  ["roost.lastTerminalPath", "/s/session-a"],
  ["roost.lastSessionByFolder", JSON.stringify({ "worker-a\u0000/home/owner": "session-a" })],
  ["roost.lastWorkspaceId.worker-a", "workspace-a"],
  ["roost.sidebar.recent", JSON.stringify(["session-a"])],
  ["roost.paneLayout.v1", JSON.stringify({})],
  ["roost.agentSeen.v1", JSON.stringify({ "session-a": 4 })],
  ["roost.notifications.prefs.v2", JSON.stringify({ desktop: true })],
  ["roost.keytermLexicon.v1", JSON.stringify({ "private-project": 8 })],
  ["roost.dashboardId", "dashboard-a"],
  ["roost.syncLastEventId", "41"],
  ["roost.theme", "dark"],
]);
const session = new Map<string, string>();

function storageFor(values: Map<string, string>): Storage {
  return {
    get length() { return values.size; },
    clear: () => values.clear(),
    key: (index: number) => [...values.keys()][index] ?? null,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  } as Storage;
}

Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storageFor(local) });
Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: storageFor(session) });

const {
  clearAccountSensitiveStateForLogout,
  logoutManagedBrowser,
  prepareManagedTenantRouteSwitch,
  MANAGED_LOGOUT_UNCONFIRMED_MESSAGE,
} = await import("../src/auth/managed-logout.ts");
const { getComposerDraft } = await import("../src/lib/composerDrafts.ts");
const { getLastTerminalPath, getLastSessionForFolder } = await import("../src/lib/lastVisited.ts");
const { rootStore, setDashboardAccess, setRootStore } = await import("../src/store/root.ts");
import type {
  ManagedLogoutDependencies,
  ManagedTenantSwitchDependencies,
} from "../src/auth/managed-logout.ts";

function dependencies(events: string[], overrides: Partial<ManagedLogoutDependencies> = {}): ManagedLogoutDependencies {
  return {
    authLogout: async () => { events.push("server"); return { ok: true }; },
    probeCurrentDevice: async () => { events.push("probe"); return "authorized"; },
    unsubscribePush: async () => { events.push("push"); },
    clearClientState: () => { events.push("state"); },
    clearWebKeyMaterial: async () => { events.push("key"); },
    replaceLocation: (path) => { events.push(`replace:${path}`); },
    ...overrides,
  };
}

describe("managed browser logout", () => {
  test("revokes before best-effort Push and local destruction", async () => {
    const events: string[] = [];
    await logoutManagedBrowser(dependencies(events, {
      unsubscribePush: async () => { events.push("push"); throw new Error("unavailable"); },
    }));
    expect(events).toEqual(["server", "push", "state", "key", "replace:/login"]);
  });

  test("proves rejection when the logout response is lost", async () => {
    const events: string[] = [];
    await logoutManagedBrowser(dependencies(events, {
      authLogout: async () => { events.push("server"); throw new TypeError("response lost"); },
      probeCurrentDevice: async () => { events.push("probe"); return "device-rejected"; },
    }));
    expect(events).toEqual(["server", "probe", "push", "state", "key", "replace:/login"]);
  });

  test("preserves local authority when revocation cannot be proven", async () => {
    for (const result of ["authorized", "ambiguous"] as const) {
      const events: string[] = [];
      const deps = dependencies(events, {
        authLogout: async () => { events.push("server"); return { ok: false }; },
        probeCurrentDevice: async () => { events.push("probe"); return result; },
      });
      await expect(logoutManagedBrowser(deps)).rejects.toThrow(MANAGED_LOGOUT_UNCONFIRMED_MESSAGE);
      expect(events).toEqual(["server", "probe"]);
    }
  });

  test("clears account records, drafts, paths, voice, Push, and selection persistence", () => {
    expect(getComposerDraft("session-a")).toBe("secret draft");
    setDashboardAccess({
      account_id: "account-a",
      organizations: [{ id: "account-a", slug: "personal", name: "owner@example.com", role: "owner" }],
      dashboards: [{ id: "dashboard-a", organization_id: "account-a", slug: "default", name: "Personal", organization_role: "owner", dashboard_role: "admin" }],
      selected_dashboard_id: "dashboard-a",
      capabilities: ["dashboard:admin"],
    });
    setRootStore("sessions", { "session-a": {} as never });
    setRootStore("workers", { "worker-a": {} as never });

    clearAccountSensitiveStateForLogout();

    expect(rootStore.account_id).toBeNull();
    expect(rootStore.selected_dashboard_id).toBeNull();
    expect(Object.keys(rootStore.organizations)).toEqual([]);
    expect(Object.keys(rootStore.dashboards)).toEqual([]);
    expect(Object.keys(rootStore.sessions)).toEqual([]);
    expect(Object.keys(rootStore.workers)).toEqual([]);
    expect(getComposerDraft("session-a")).toBe("");
    expect(getLastTerminalPath()).toBeNull();
    expect(getLastSessionForFolder("worker-a", "/home/owner")).toBeNull();
    for (const key of [
      "roost.composerDrafts.v1", "roost.lastTerminalPath", "roost.lastSessionByFolder",
      "roost.lastWorkspaceId.worker-a", "roost.sidebar.recent", "roost.paneLayout.v1",
      "roost.agentSeen.v1", "roost.notifications.prefs.v2", "roost.keytermLexicon.v1",
      "roost.dashboardId", "roost.syncLastEventId",
    ]) expect(local.has(key)).toBe(false);
    expect(local.get("roost.theme")).toBe("dark");
  });

  test("route switching destroys prior Push, state, and key before persistence", async () => {
    const routeA = "a".repeat(64);
    const routeB = "b".repeat(64);
    local.set("roost.tenantRouteKey", routeA);
    const events: string[] = [];
    const switchDependencies: ManagedTenantSwitchDependencies = {
      revokePreviousDevice: async (routeKey) => { events.push(`revoke:${routeKey}`); },
      unsubscribePreviousPush: async (routeKey) => { events.push(`push:${routeKey}`); },
      clearClientState: () => { events.push("state"); },
      clearWebKeyMaterial: async () => { events.push("key"); },
    };

    await expect(prepareManagedTenantRouteSwitch(routeB, switchDependencies)).resolves.toBe(true);
    expect(events).toEqual([`revoke:${routeA}`, `push:${routeA}`, "state", "key"]);
    expect(local.get("roost.tenantRouteKey")).toBe(routeA);

    events.length = 0;
    await expect(prepareManagedTenantRouteSwitch(routeA, switchDependencies)).resolves.toBe(false);
    expect(events).toEqual([]);
  });

});
