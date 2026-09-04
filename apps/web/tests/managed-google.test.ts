import { describe, expect, test } from "bun:test";
import {
  ManagedAuthGatewayError,
  bindManagedGoogleDevice,
  getManagedAuthResult,
  startManagedGoogle,
} from "../src/auth/managed-auth-gateway.ts";

function fakeFetch(run: (request: Request) => Response | Promise<Response>): typeof fetch {
  return Object.assign(
    async (input: string | URL | Request, init?: RequestInit) =>
      run(input instanceof Request
        ? input
        : new Request(new URL(String(input), "https://dashboard.roosttt.com"), init)),
    { preconnect: globalThis.fetch.preconnect },
  );
}

describe("managed Google gateway client", () => {
  test("starts only the fixed Google authorization URL with exact intent body", async () => {
    const seen: unknown[] = [];
    const url = await startManagedGoogle({ intent: "login" }, fakeFetch(async (request) => {
      expect(new URL(request.url).pathname).toBe("/__roost/auth/google/start");
      seen.push(await request.json());
      return Response.json({
        authorizationUrl: "https://accounts.google.com/o/oauth2/v2/auth?client_id=client&scope=openid+email&state=opaque",
      });
    }));
    expect(seen).toEqual([{ intent: "login" }]);
    expect(url).toStartWith("https://accounts.google.com/o/oauth2/v2/auth?");
    await expect(startManagedGoogle({ intent: "login" }, fakeFetch(() => Response.json({
      authorizationUrl: "https://evil.example/callback",
    })))).rejects.toBeInstanceOf(ManagedAuthGatewayError);
  });

  test("parses capability-bound awaiting-device and binds only through an exact JSON body", async () => {
    const routeKey = "a".repeat(64);
    const awaiting = await getManagedAuthResult(fakeFetch(() => Response.json({
      state: "awaiting-device",
      routeKey,
    }, { status: 202, headers: { "retry-after": "1" } })));
    expect(awaiting).toEqual({ state: "awaiting-device", routeKey });

    let body: unknown;
    const bound = await bindManagedGoogleDevice("browser-public-key", fakeFetch(async (request) => {
      expect(new URL(request.url).pathname).toBe("/__roost/auth/bind-device");
      expect(request.method).toBe("POST");
      body = await request.json();
      return Response.json({ state: "ready", routeKey, assertion: "header.payload.signature" });
    }));
    expect(body).toEqual({ sshPubkeyB64: "browser-public-key" });
    expect(bound).toEqual({ routeKey, assertion: "header.payload.signature" });
  });
});
