import { describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  GENERIC_CREDENTIAL_ERROR,
  loginManagedBrowser,
  MANAGED_LOGIN_CONNECTION_ERROR,
  MANAGED_LOGIN_SCOPE_ERROR,
  ManagedLoginScopeError,
  managedLoginErrorMessage,
  type ManagedLoginDependencies,
} from "../src/auth/managed-login.ts";
import { setDashboardAccess } from "../src/store/root.ts";
import { makePublicCoordinatorClient } from "../src/connect.ts";
import { X_ROOST_DASHBOARD_ID, X_ROOST_TAB_ID } from "@roost/shared/wire/headers";
const ROUTE_A = "a".repeat(64);


function resetAccess() {
  setDashboardAccess({
    account_id: "",
    organizations: [],
    dashboards: [],
    selected_dashboard_id: null,
    capabilities: [],
  });
}

function confirmedAccess(dashboardId: string) {
  setDashboardAccess({
    account_id: "account-owner",
    organizations: [{ id: "org-owner", slug: "owner", name: "Owner", role: "owner" }],
    dashboards: [{
      id: dashboardId,
      organization_id: "org-owner",
      slug: "default",
      name: "Default",
      organization_role: "owner",
      dashboard_role: "admin",
    }],
    selected_dashboard_id: dashboardId,
    capabilities: ["dashboard:member", "dashboard:admin"],
  });
}

describe("managed native login", () => {
  test("binds the persisted browser key before confirming signed dashboard access", async () => {
    resetAccess();
    const events: string[] = [];
    let sentRequest: Parameters<ManagedLoginDependencies["passwordLogin"]>[1] | undefined;
    const dependencies: ManagedLoginDependencies = {
      resolveRouteKey: async (email) => {
        events.push(`resolve:${email}`);
        return ROUTE_A;
      },
      prepareRouteSwitch: async (routeKey) => { events.push(`switch:${routeKey}`); },
      persistRouteKey: (routeKey) => {
        events.push(`persist:${routeKey}`);
        return true;
      },
      clearStaleCredential: () => { events.push("clear-credential"); },
      publicKeyB64: async () => {
        events.push("public-key");
        return "browser-public-key";
      },
      browserLabel: () => "Firefox — Linux",
      passwordLogin: async (routeKey, request) => {
        events.push(`password-login:${routeKey}`);
        sentRequest = request;
        return { dashboardId: "dashboard-owner" };
      },
      confirmDashboard: async (routeKey, dashboardId) => {
        events.push(`dashboard-access:${routeKey}:${dashboardId}`);
        return true;
      },
      rememberDashboardHint: (dashboardId) => { events.push(`remember:${dashboardId}`); },
      markKeyAuthorized: () => { events.push("authorized"); },
      replaceLocation: (path) => { events.push(`replace:${path}`); },
    };

    await expect(loginManagedBrowser({
      email: "  Owner@Example.com ",
      password: "correct horse battery staple",
    }, dependencies)).resolves.toEqual({ dashboardId: "dashboard-owner" });

    expect(sentRequest).toEqual({
      email: "Owner@Example.com",
      password: "correct horse battery staple",
      sshPubkeyB64: "browser-public-key",
      label: "Firefox — Linux",
    });
    expect(events).toEqual([
      "resolve:Owner@Example.com",
      `switch:${ROUTE_A}`,
      "clear-credential",
      "public-key",
      `password-login:${ROUTE_A}`,
      `persist:${ROUTE_A}`,
      `dashboard-access:${ROUTE_A}:dashboard-owner`,
      "remember:dashboard-owner",
      "authorized",
      "replace:/app",
    ]);
  });

  test("password bootstrap transport sends neither a device JWT nor dashboard authority", async () => {
    const originalFetch = globalThis.fetch;
    const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
    const originalStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
    const stored = new Map([
      ["roost.coordinatorUrl", "https://stale-coordinator.example"],
      ["roost.deploymentMode", "self-hosted"],
    ]);
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: { origin: "https://managed.example", hash: "" },
    });
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      value: {
        getItem: (key: string) => stored.get(key) ?? null,
        setItem: (key: string, value: string) => { stored.set(key, value); },
        removeItem: (key: string) => { stored.delete(key); },
      },
    });
    let request: Request | null = null;
    const interceptedFetch: typeof fetch = Object.assign(
      async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
        request = input instanceof Request ? input : new Request(input, init);
        throw new TypeError("captured");
      },
      { preconnect: originalFetch.preconnect },
    );
    globalThis.fetch = interceptedFetch;
    try {
      await makePublicCoordinatorClient(ROUTE_A).authPasswordLogin({
        email: "owner@example.com",
        password: "not-persisted",
        sshPubkeyB64: "browser-public-key",
        label: "browser",
      });
    } catch {
      // The focused fetch fake stops after the request interceptor has run.
    } finally {
      globalThis.fetch = originalFetch;
      if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
      else Reflect.deleteProperty(globalThis, "location");
      if (originalStorage) Object.defineProperty(globalThis, "localStorage", originalStorage);
      else Reflect.deleteProperty(globalThis, "localStorage");
    }
    // The async fetch callback owns the assignment, which control-flow
    // analysis cannot observe after the awaited client call.
    const capturedRequest = request as Request | null;
    if (!capturedRequest) throw new Error("public login request was not captured");
    expect(capturedRequest.headers.get("Authorization")).toBeNull();
    expect(capturedRequest.headers.get(X_ROOST_DASHBOARD_ID)).toBeNull();
    expect(capturedRequest.headers.get(X_ROOST_TAB_ID)).not.toBeNull();
    expect(new URL(capturedRequest.url).origin).toBe("https://managed.example");
    expect(new URL(capturedRequest.url).pathname).toStartWith(`/_roost/t/${ROUTE_A}/`);
  });

  test("never enters the app when AuthDashboardAccess does not confirm the login scope", async () => {
    resetAccess();
    let resumed = false;
    const dependencies: ManagedLoginDependencies = {
      resolveRouteKey: async () => ROUTE_A,
      prepareRouteSwitch: async () => {},
      persistRouteKey: () => true,
      clearStaleCredential: () => {},
      publicKeyB64: async () => "browser-public-key",
      browserLabel: () => "browser",
      passwordLogin: async () => ({ dashboardId: "dashboard-owner" }),
      confirmDashboard: async () => false,
      rememberDashboardHint: () => {},
      markKeyAuthorized: () => { resumed = true; },
      replaceLocation: () => { resumed = true; },
    };

    await expect(loginManagedBrowser({ email: "owner@example.com", password: "password" }, dependencies))
      .rejects.toThrow("managed login returned no confirmed dashboard");
    expect(resumed).toBe(false);
  });

  test("credential failures share one non-disclosing message", () => {
    expect(managedLoginErrorMessage(new ConnectError("unknown email", Code.Unauthenticated)))
      .toBe(GENERIC_CREDENTIAL_ERROR);
    expect(managedLoginErrorMessage(new ConnectError("wrong password", Code.PermissionDenied)))
      .toBe(GENERIC_CREDENTIAL_ERROR);
    expect(managedLoginErrorMessage(new ConnectError("rate limited", Code.ResourceExhausted)))
      .toBe(GENERIC_CREDENTIAL_ERROR);
  });

  test("connection and post-login scope failures remain actionable", () => {
    expect(managedLoginErrorMessage(new TypeError("fetch failed")))
      .toBe(MANAGED_LOGIN_CONNECTION_ERROR);
    expect(managedLoginErrorMessage(new ConnectError("timeout", Code.DeadlineExceeded)))
      .toBe(MANAGED_LOGIN_CONNECTION_ERROR);
    expect(managedLoginErrorMessage(new ManagedLoginScopeError()))
      .toBe(MANAGED_LOGIN_SCOPE_ERROR);
  });
});
