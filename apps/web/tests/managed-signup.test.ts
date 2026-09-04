import { describe, expect, test } from "bun:test";
import {
  getManagedAuthConfig,
  startManagedEmailSignup,
  verifyManagedEmailSignup,
} from "../src/auth/managed-auth-gateway.ts";
import {
  MANAGED_SIGNUP_UNAVAILABLE_MESSAGE,
  managedGoogleEnabled,
  managedSignupRouteEnabled,
} from "../src/auth/managed-signup-policy.ts";
import { ROUTES } from "../src/routes.ts";

function fakeFetch(run: (request: Request) => Response | Promise<Response>): typeof fetch {
  return Object.assign(
    async (input: string | URL | Request, init?: RequestInit) =>
      run(input instanceof Request
        ? input
        : new Request(new URL(String(input), "https://dashboard.roosttt.com"), init)),
    { preconnect: globalThis.fetch.preconnect },
  );
}

describe("managed signup gateway client", () => {
  test("loads only public feature configuration", async () => {
    const config = await getManagedAuthConfig(fakeFetch((request) => {
      expect(new URL(request.url).pathname).toBe("/__roost/auth/config");
      return Response.json({
        signupEnabled: true,
        googleEnabled: true,
        turnstileSiteKey: "site-key",
      });
    }));
    expect(config).toEqual({ signupEnabled: true, googleEnabled: true, turnstileSiteKey: "site-key" });
  });

  test("submits email and scrubbed verification credentials only in exact JSON bodies", async () => {
    const requests: Array<{ path: string; body: unknown }> = [];
    const fetchImpl = fakeFetch(async (request) => {
      const path = new URL(request.url).pathname;
      requests.push({ path, body: await request.json() });
      expect(request.method).toBe("POST");
      expect(request.headers.get("content-type")).toBe("application/json");
      return Response.json({
        state: path === "/__roost/signup/email/start" ? "verification-pending" : "pending",
      }, { status: 202 });
    });
    await startManagedEmailSignup({
      email: "owner@example.test",
      turnstileToken: "turnstile-token",
    }, fetchImpl);
    await verifyManagedEmailSignup("a".repeat(43), fetchImpl);
    expect(requests).toEqual([
      {
        path: "/__roost/signup/email/start",
        body: { email: "owner@example.test", turnstileToken: "turnstile-token" },
      },
      {
        path: "/__roost/signup/email/verify",
        body: { token: "a".repeat(43) },
      },
    ]);
  });
});

describe("managed signup surface policy", () => {
  test("fails both enrollment routes closed with the exact operator message", () => {
    const disabledConfig = {
      signupEnabled: false,
      googleEnabled: true,
      turnstileSiteKey: "stale-site-key",
    };

    expect(MANAGED_SIGNUP_UNAVAILABLE_MESSAGE)
      .toBe("Account creation is unavailable. Contact your Roost operator.");
    expect(managedSignupRouteEnabled(ROUTES.SIGNUP, disabledConfig)).toBeFalse();
    expect(managedSignupRouteEnabled(ROUTES.SIGNUP_VERIFY, disabledConfig)).toBeFalse();
    expect(managedSignupRouteEnabled(ROUTES.SIGNUP, undefined)).toBeFalse();
    expect(managedSignupRouteEnabled(ROUTES.SIGNUP_VERIFY, undefined)).toBeFalse();
  });

  test("retains enabled verification and requires a configured signup provider", () => {
    const enabledConfig = {
      signupEnabled: true,
      googleEnabled: false,
      turnstileSiteKey: "site-key",
    };

    expect(managedSignupRouteEnabled(ROUTES.SIGNUP, enabledConfig)).toBeTrue();
    expect(managedSignupRouteEnabled(ROUTES.SIGNUP_VERIFY, enabledConfig)).toBeTrue();
    expect(managedSignupRouteEnabled(ROUTES.SIGNUP, {
      ...enabledConfig,
      turnstileSiteKey: "  ",
    })).toBeFalse();
    expect(managedGoogleEnabled(enabledConfig)).toBeFalse();
    expect(managedGoogleEnabled({ ...enabledConfig, googleEnabled: true })).toBeTrue();
  });
});
