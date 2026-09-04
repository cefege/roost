import { describe, expect, test } from "bun:test";
import {
  deploymentMode,
  isManagedPublicRoute,
  managedRouteDecision,
  type ManagedAccessState,
} from "../src/auth/managed-routes.ts";

function decide(input: {
  saasMode?: boolean;
  pathname: string;
  hasDashboardAccess?: boolean;
  accessState?: ManagedAccessState;
}) {
  return managedRouteDecision({
    mode: deploymentMode(input.saasMode),
    pathname: input.pathname,
    hasDashboardAccess: input.hasDashboardAccess ?? false,
    accessState: input.accessState ?? "idle",
  });
}

describe("managed SPA route decisions", () => {
  test("existing self-hosted routes remain renderable", () => {
    expect(decide({ saasMode: false, pathname: "/" })).toBe("render");
    expect(decide({ saasMode: false, pathname: "/pair" })).toBe("render");
    expect(decide({ saasMode: false, pathname: "/s/session-a" })).toBe("render");
  });

  test("every managed-only account path redirects to self-hosted onboarding", () => {
    for (const pathname of [
      "/login",
      "/signup",
      "/signup/verify",
      "/auth/google/complete",
      "/activate",
      "/forgot-password",
      "/reset-password",
      "/app",
    ]) {
      expect(decide({ saasMode: false, pathname }), pathname).toBe("self-hosted-home");
    }
  });

  test("every route waits for public deployment discovery", () => {
    for (const pathname of [
      "/", "/login", "/signup", "/signup/verify", "/auth/google/complete",
      "/activate", "/forgot-password", "/reset-password",
      "/app", "/pair", "/settings/devices", "/s/session-a",
      "/t/worker-a/home", "/w/workspace-a", "/help", "/design",
      "/file/worker-a/readme", "/browse/worker-a", "/search",
    ]) {
      expect(decide({ pathname }), pathname).toBe("loading");
    }
  });

  test("only exact public account routes bypass protected access probing", () => {
    for (const pathname of [
      "/login", "/signup", "/signup/verify", "/auth/google/complete",
      "/activate", "/forgot-password", "/reset-password",
    ]) {
      expect(isManagedPublicRoute(pathname), pathname).toBeTrue();
    }
    for (const pathname of ["/", "/app", "/pair", "/activate/extra", "/settings/account"]) {
      expect(isManagedPublicRoute(pathname), pathname).toBeFalse();
    }
  });

  test("managed account forms stay public while application routes need confirmed scope", () => {
    for (const pathname of [
      "/signup", "/signup/verify", "/auth/google/complete",
      "/activate", "/forgot-password", "/reset-password",
    ]) {
      expect(decide({ saasMode: true, pathname, accessState: "unauthorized" }), pathname)
        .toBe("render");
      expect(decide({ saasMode: true, pathname, accessState: "checking" }), pathname)
        .toBe("render");
      expect(decide({
        saasMode: true,
        pathname,
        hasDashboardAccess: true,
        accessState: "idle",
      }), pathname).toBe("render");
    }
    expect(decide({ saasMode: true, pathname: "/login", accessState: "unauthorized" }))
      .toBe("render");
    expect(decide({ saasMode: true, pathname: "/app", accessState: "checking" }))
      .toBe("loading");
    expect(decide({ saasMode: true, pathname: "/s/session-a", accessState: "unauthorized" }))
      .toBe("login");
    expect(decide({
      saasMode: true,
      pathname: "/app",
      hasDashboardAccess: true,
      accessState: "unauthorized",
    })).toBe("login");
    expect(decide({
      saasMode: true,
      pathname: "/login",
      hasDashboardAccess: true,
      accessState: "idle",
    })).toBe("app");
    expect(decide({
      saasMode: true,
      pathname: "/login",
      hasDashboardAccess: true,
      accessState: "unauthorized",
    })).toBe("render");
  });

  test("managed root and pairing URLs converge on the signed application entry", () => {
    expect(decide({ saasMode: true, pathname: "/", hasDashboardAccess: true }))
      .toBe("app");
    expect(decide({ saasMode: true, pathname: "/pair", hasDashboardAccess: true }))
      .toBe("app");
    expect(decide({ saasMode: true, pathname: "/s/session-a", hasDashboardAccess: true }))
      .toBe("render");
  });

  test("transient scope failures are retryable in place rather than looking like logout", () => {
    expect(decide({ saasMode: true, pathname: "/app", accessState: "error" }))
      .toBe("error");
  });
});
