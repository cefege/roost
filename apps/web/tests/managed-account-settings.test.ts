import { describe, expect, test } from "bun:test";
import {
  MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE,
  ManagedAccountBusyGate,
  ManagedCredentialBindingError,
  ManagedGoogleIdentityUnavailableError,
  addManagedPassword,
  beginManagedGoogleLink,
  completeManagedGoogleLink,
  isManagedGoogleLinkAssertion,
  getManagedCredentials,
  type ManagedCredentialsDependencies,
} from "../src/auth/managed-credentials.ts";
import { ManagedNewPasswordError } from "../src/auth/managed-account.ts";

const ROUTE = "a".repeat(64);
const OTHER_ROUTE = "b".repeat(64);
const FINGERPRINT = "c".repeat(64);
const LINK_TICKET = "e30.e30.signature";

interface FakeClient {
  authCredentialsGet(request: Record<string, never>): Promise<{
    email: string;
    hasPassword: boolean;
    googleLinked: boolean;
  }>;
  authPasswordAdd(request: { newPassword: string }): Promise<{ ok: boolean }>;
  authFederatedLinkBegin(request: Record<string, never>): Promise<{ linkTicket: string }>;
  authFederatedLink(request: { assertion: string }): Promise<{ ok: boolean }>;
}

function defaultClient(): FakeClient {
  return {
    async authCredentialsGet() {
      return { email: "owner@example.com", hasPassword: false, googleLinked: true };
    },
    async authPasswordAdd() { return { ok: true }; },
    async authFederatedLinkBegin() { return { linkTicket: LINK_TICKET }; },
    async authFederatedLink() { return { ok: true }; },
  };
}

function dependencies(overrides: Partial<ManagedCredentialsDependencies> = {}): ManagedCredentialsDependencies {
  let nowMs = 1_000;
  const client = defaultClient();
  return {
    storedRouteKey: () => ROUTE,
    currentWebKeyInfo: async () => ({ fingerprint: FINGERPRINT, extractable: false }),
    clientForRoute: () => client,
    fetch: (async () => new Response(JSON.stringify({ state: "ready" }), {
      status: 200,
      headers: { "content-type": "application/json" },
    })) as unknown as typeof fetch,
    now: () => nowMs,
    sleep: async (delayMs) => { nowMs += delayMs; },
    ...overrides,
  };
}

function linkAssertion(routeKey = ROUTE, fingerprint = FINGERPRINT): string {
  const payload = Buffer.from(JSON.stringify({
    purpose: "link",
    route_key: routeKey,
    device_fp: fingerprint,
  }), "utf8").toString("base64url");
  return `e30.${payload}.signature`;
}

describe("managed Account credential state", () => {
  test("keeps the protected credential response in the caller only and represents both methods", async () => {
    const storageWrites: string[] = [];
    const client = defaultClient();
    client.authCredentialsGet = async () => ({
      email: "canonical@example.com",
      hasPassword: true,
      googleLinked: true,
    });
    const result = await getManagedCredentials(dependencies({
      clientForRoute: (routeKey) => {
        expect(routeKey).toBe(ROUTE);
        return client;
      },
      storedRouteKey: () => {
        // The selected route is the only persistent value this flow reads.
        storageWrites.push("route-read");
        return ROUTE;
      },
    }));

    expect(result).toEqual({
      email: "canonical@example.com",
      hasPassword: true,
      googleLinked: true,
    });
    expect(storageWrites).toEqual(["route-read", "route-read", "route-read", "route-read"]);
    expect(JSON.stringify(storageWrites)).not.toContain("canonical@example.com");
  });

  test("admits only one device, Google, password, revoke, or sign-out mutation", () => {
    const gate = new ManagedAccountBusyGate();
    expect(gate.begin("google")).toBe(true);
    expect(gate.begin("password")).toBe(false);
    expect(gate.begin("devices")).toBe(false);
    expect(gate.begin("logout")).toBe(false);
    expect(gate.finish("password")).toBe(false);
    expect(gate.current).toBe("google");
    expect(gate.finish("google")).toBe(true);
    expect(gate.begin("devices")).toBe(true);
  });
});

describe("managed Google linking", () => {
  test("uses only a bounded purpose hint to dispatch link completion", () => {
    expect(isManagedGoogleLinkAssertion(linkAssertion())).toBe(true);
    const continuePayload = Buffer.from(JSON.stringify({
      purpose: "continue",
      route_key: ROUTE,
      device_fp: FINGERPRINT,
    }), "utf8").toString("base64url");
    expect(isManagedGoogleLinkAssertion(`e30.${continuePayload}.signature`)).toBe(false);
    expect(isManagedGoogleLinkAssertion("not-a-jwt")).toBe(false);
  });

  test("starts with the exact stored route and current protected device ticket", async () => {
    const calls: string[] = [];
    const client = defaultClient();
    client.authFederatedLinkBegin = async (request) => {
      calls.push("link-begin");
      expect(request).toEqual({});
      return { linkTicket: LINK_TICKET };
    };
    const fetchImpl = (async (input: string | URL | Request, init?: RequestInit) => {
      calls.push("google-start");
      expect(input).toBe("/__roost/auth/google/start");
      expect(init?.method).toBe("POST");
      expect(init?.credentials).toBe("same-origin");
      expect(init?.referrerPolicy).toBe("no-referrer");
      expect(JSON.parse(String(init?.body))).toEqual({
        intent: "link",
        routeKey: ROUTE,
        linkTicket: LINK_TICKET,
      });
      return new Response(JSON.stringify({
        authorizationUrl: "https://accounts.google.com/o/oauth2/v2/auth?client_id=client&state=opaque",
      }), { status: 200 });
    }) as unknown as typeof fetch;

    const result = await beginManagedGoogleLink(dependencies({
      clientForRoute: (routeKey) => {
        expect(routeKey).toBe(ROUTE);
        return client;
      },
      fetch: fetchImpl,
    }));

    expect(calls).toEqual(["link-begin", "google-start"]);
    expect(result.authorizationUrl).toStartWith("https://accounts.google.com/o/oauth2/v2/auth?");
  });

  test("fails closed before central or tenant mutation when route or assertion device changes", async () => {
    let route = ROUTE;
    let linkCalls = 0;
    let gatewayCalls = 0;
    const client = defaultClient();
    client.authFederatedLink = async () => {
      linkCalls++;
      return { ok: true };
    };
    const deps = dependencies({
      storedRouteKey: () => route,
      clientForRoute: () => client,
      fetch: (async () => {
        gatewayCalls++;
        return new Response("{}");
      }) as unknown as typeof fetch,
    });

    await expect(completeManagedGoogleLink({
      routeKey: ROUTE,
      assertion: linkAssertion(ROUTE, "d".repeat(64)),
    }, deps)).rejects.toBeInstanceOf(ManagedCredentialBindingError);
    expect(linkCalls).toBe(0);
    expect(gatewayCalls).toBe(0);

    route = OTHER_ROUTE;
    await expect(completeManagedGoogleLink({
      routeKey: ROUTE,
      assertion: linkAssertion(),
    }, deps)).rejects.toBeInstanceOf(ManagedCredentialBindingError);
    expect(linkCalls).toBe(0);
  });

  test("proves the tenant link before idempotent central completion and refreshes credentials", async () => {
    const calls: string[] = [];
    let completionCall = 0;
    const client = defaultClient();
    client.authFederatedLink = async ({ assertion }) => {
      calls.push("tenant-link");
      expect(assertion).toBe(linkAssertion());
      return { ok: true };
    };
    client.authCredentialsGet = async () => {
      calls.push("credentials-refresh");
      return { email: "owner@example.com", hasPassword: true, googleLinked: true };
    };
    const fetchImpl = (async (input: string | URL | Request, init?: RequestInit) => {
      expect(input).toBe("/__roost/auth/link/complete");
      expect(init?.body).toBe("{}");
      calls.push("central-complete");
      completionCall++;
      if (completionCall === 1) {
        return new Response(JSON.stringify({ state: "pending" }), {
          status: 202,
          headers: { "retry-after": "1" },
        });
      }
      return new Response(JSON.stringify({ state: "ready" }), { status: 200 });
    }) as unknown as typeof fetch;
    const deps = dependencies({ clientForRoute: () => client, fetch: fetchImpl });

    const first = await completeManagedGoogleLink({ routeKey: ROUTE, assertion: linkAssertion() }, deps);
    const second = await completeManagedGoogleLink({ routeKey: ROUTE, assertion: linkAssertion() }, deps);

    expect(first).toEqual({ email: "owner@example.com", hasPassword: true, googleLinked: true });
    expect(second).toEqual(first);
    expect(calls).toEqual([
      "tenant-link",
      "central-complete",
      "central-complete",
      "credentials-refresh",
      "tenant-link",
      "central-complete",
      "credentials-refresh",
    ]);
  });

  test("uses the bounded authenticated unavailable message", async () => {
    const error = await beginManagedGoogleLink(dependencies({
      fetch: (async () => new Response(JSON.stringify({ error: "provider detail" }), { status: 503 })) as unknown as typeof fetch,
    })).catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(ManagedGoogleIdentityUnavailableError);
    expect((error as Error).message).toBe(MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE);
    expect((error as Error).message).not.toContain("provider detail");
  });
});

describe("managed password addition", () => {
  test("enforces 12–1,024 characters before RPC and returns Google plus password state", async () => {
    const submitted: string[] = [];
    const client = defaultClient();
    client.authPasswordAdd = async ({ newPassword }) => {
      submitted.push(newPassword);
      return { ok: true };
    };
    client.authCredentialsGet = async () => ({
      email: "owner@example.com",
      hasPassword: true,
      googleLinked: true,
    });
    const deps = dependencies({ clientForRoute: () => client });

    await expect(addManagedPassword({ password: "short", confirmation: "short" }, deps))
      .rejects.toBeInstanceOf(ManagedNewPasswordError);
    await expect(addManagedPassword({ password: "x".repeat(12), confirmation: "y".repeat(12) }, deps))
      .rejects.toBeInstanceOf(ManagedNewPasswordError);
    await expect(addManagedPassword({ password: "x".repeat(1_025), confirmation: "x".repeat(1_025) }, deps))
      .rejects.toBeInstanceOf(ManagedNewPasswordError);
    expect(submitted).toEqual([]);

    const result = await addManagedPassword({
      password: "a secure password",
      confirmation: "a secure password",
    }, deps);
    expect(submitted).toEqual(["a secure password"]);
    expect(result).toEqual({ email: "owner@example.com", hasPassword: true, googleLinked: true });
  });
});
