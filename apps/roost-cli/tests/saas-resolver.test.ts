import { afterEach, describe, expect, test } from "bun:test";
import { createHmac } from "node:crypto";
import {
  TENANT_RESOLVER_HEALTH_PATH,
  TENANT_RESOLVER_HOST,
  TENANT_RESOLVER_MAX_BODY_BYTES,
  TENANT_RESOLVER_MAX_IP_HEADER_BYTES,
  TENANT_RESOLVER_ORIGIN,
  TENANT_RESOLVER_PATH,
  TenantResolver,
  loadTenantResolverRuntimeConfig,
  startTenantResolver,
  tenantResolverClientIp,
  type TenantResolverService,
} from "../src/saas/resolver.ts";

const KNOWN_ROUTE_KEY = "a".repeat(64);
const HMAC_KEY = new Uint8Array(32).fill(7);
const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";

const services: TenantResolverService[] = [];
afterEach(() => {
  while (services.length > 0) services.pop()!.stop();
});

function lookup() {
  return {
    getRouteKeyByEmail(email: string): string | null {
      return email === "owner@example.com" ? KNOWN_ROUTE_KEY : null;
    },
  };
}

function resolverRequest(
  body: BodyInit = JSON.stringify({ email: "owner@example.com" }),
  options: {
    url?: string;
    method?: string;
    origin?: string | null;
    contentType?: string | null;
    headers?: Record<string, string>;
  } = {},
): Request {
  const headers = new Headers(options.headers);
  if (options.origin !== null) headers.set("origin", options.origin ?? TENANT_RESOLVER_ORIGIN);
  if (options.contentType !== null) headers.set("content-type", options.contentType ?? "application/json");
  return new Request(options.url ?? `http://resolver.test${TENANT_RESOLVER_PATH}`, {
    method: options.method ?? "POST",
    headers,
    body,
  });
}

async function routeKey(response: Response): Promise<string> {
  const parsed: unknown = JSON.parse(await response.text());
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed) || !("routeKey" in parsed)) {
    throw new Error("resolver response has no routeKey");
  }
  const value = parsed.routeKey;
  if (typeof value !== "string") throw new Error("resolver routeKey is not a string");
  return value;
}

describe("SaaS tenant resolver", () => {
  test("known and unknown accounts have the same public response shape, length, and status", async () => {
    const resolver = new TenantResolver({ registry: lookup(), hmacKey: HMAC_KEY });
    try {
      const known = await resolver.fetch(resolverRequest(), "203.0.113.10");
      const unknown = await resolver.fetch(
        resolverRequest(JSON.stringify({ email: "missing@example.com" })),
        "203.0.113.11",
      );
      expect(known.status).toBe(200);
      expect(unknown.status).toBe(known.status);
      expect(unknown.headers.get("content-type")).toBe(known.headers.get("content-type"));
      expect(unknown.headers.get("cache-control")).toBe("no-store");

      const knownBody = await known.text();
      const unknownBody = await unknown.text();
      expect(Buffer.byteLength(unknownBody)).toBe(Buffer.byteLength(knownBody));
      expect(Object.keys(JSON.parse(knownBody))).toEqual(["routeKey"]);
      expect(Object.keys(JSON.parse(unknownBody))).toEqual(["routeKey"]);
      expect(knownBody).not.toContain(ACCOUNT_ID);
      expect(knownBody).not.toContain(COORDINATOR_ID);
      expect(unknownBody).not.toContain(ACCOUNT_ID);
      expect(unknownBody).not.toContain(COORDINATOR_ID);
      expect(JSON.parse(knownBody)).toEqual({ routeKey: KNOWN_ROUTE_KEY });
    } finally {
      resolver.close();
    }
  });

  test("unknown keys are normalized, stable HMAC-SHA256 values across resolver restarts", async () => {
    const first = new TenantResolver({ registry: lookup(), hmacKey: HMAC_KEY });
    const second = new TenantResolver({ registry: lookup(), hmacKey: HMAC_KEY });
    try {
      const firstKey = await routeKey(await first.fetch(
        resolverRequest(JSON.stringify({ email: " Missing@Example.COM " })),
        "203.0.113.20",
      ));
      const repeatedKey = await routeKey(await first.fetch(
        resolverRequest(JSON.stringify({ email: "missing@example.com" })),
        "203.0.113.20",
      ));
      const restartedKey = await routeKey(await second.fetch(
        resolverRequest(JSON.stringify({ email: "missing@example.com" })),
        "203.0.113.20",
      ));
      const expected = createHmac("sha256", HMAC_KEY)
        .update("roost-tenant-route-v1\0")
        .update("missing@example.com")
        .digest("hex");
      expect(firstKey).toBe(expected);
      expect(repeatedKey).toBe(firstKey);
      expect(restartedKey).toBe(firstKey);
      expect(firstKey).toMatch(/^[0-9a-f]{64}$/);
    } finally {
      first.close();
      second.close();
    }
  });

  test("requires the exact path, Origin, JSON media type, shape, and bounded body", async () => {
    const resolver = new TenantResolver({ registry: lookup(), hmacKey: HMAC_KEY });
    try {
      const invalidRequests: Array<[Request, number]> = [
        [resolverRequest(undefined, { origin: null }), 400],
        [resolverRequest(undefined, { origin: "https://attacker.example" }), 400],
        [resolverRequest(undefined, { contentType: "application/json; charset=utf-8" }), 400],
        [resolverRequest(undefined, { headers: { "content-encoding": "identity" } }), 400],
        [resolverRequest(JSON.stringify({ email: "owner@example.com", reveal: true })), 400],
        [resolverRequest(JSON.stringify(["owner@example.com"])), 400],
        [resolverRequest(JSON.stringify({ email: "not-an-email" })), 400],
        [resolverRequest("{"), 400],
        [resolverRequest(JSON.stringify({ email: "a".repeat(TENANT_RESOLVER_MAX_BODY_BYTES) })), 400],
        [resolverRequest(undefined, { url: `http://resolver.test${TENANT_RESOLVER_PATH}?email=owner@example.com` }), 404],
        [resolverRequest(undefined, { url: "http://resolver.test/__roost/tenant/resolve/" }), 404],
        [resolverRequest(undefined, { method: "PUT" }), 404],
      ];
      for (const [request, status] of invalidRequests) {
        const response = await resolver.fetch(request, "203.0.113.30");
        expect(response.status).toBe(status);
        const body = await response.text();
        expect(body).not.toContain("owner@example.com");
        expect(body).not.toContain(ACCOUNT_ID);
      }
    } finally {
      resolver.close();
    }
  });

  test("bounds and rate-limits canonical client IPs without unbounded bucket churn", async () => {
    let now = 1_000;
    const resolver = new TenantResolver({
      registry: lookup(),
      hmacKey: HMAC_KEY,
      now: () => now,
      rateLimit: 2,
      rateWindowMs: 60_000,
      maxRateBuckets: 1,
    });
    try {
      expect((await resolver.fetch(resolverRequest(), "203.0.113.40")).status).toBe(200);
      expect((await resolver.fetch(resolverRequest(), "203.0.113.40")).status).toBe(200);
      const limited = await resolver.fetch(resolverRequest(), "203.0.113.40");
      expect(limited.status).toBe(429);
      expect(limited.headers.get("retry-after")).toBe("60");
      expect((await resolver.fetch(resolverRequest(), "203.0.113.41")).status).toBe(429);

      now += 60_000;
      expect((await resolver.fetch(resolverRequest(), "203.0.113.41")).status).toBe(200);

      const oversizedIp = await resolver.fetch(resolverRequest(undefined, {
        headers: { "x-forwarded-for": "1".repeat(TENANT_RESOLVER_MAX_IP_HEADER_BYTES + 1) },
      }), "127.0.0.1");
      expect(oversizedIp.status).toBe(400);
      const invalidIp = await resolver.fetch(resolverRequest(undefined, {
        headers: { "x-forwarded-for": "203.0.113.50, not-an-ip" },
      }), "127.0.0.1");
      expect(invalidIp.status).toBe(400);
    } finally {
      resolver.close();
    }
  });

  test("accepts one proxy-sanitized client IP and ignores spoofable Cloudflare headers", () => {
    const forwarded = resolverRequest(undefined, {
      headers: { "x-forwarded-for": "2001:0db8:0:0:0:0:0:1" },
    });
    expect(tenantResolverClientIp(forwarded, "127.0.0.1")).toBe("2001:db8::1");

    const ambiguous = resolverRequest(undefined, {
      headers: { "x-forwarded-for": "198.51.100.2, 127.0.0.1" },
    });
    expect(tenantResolverClientIp(ambiguous, "127.0.0.1")).toBeNull();

    const spoofedCloudflare = resolverRequest(undefined, {
      headers: { "cf-connecting-ip": "203.0.113.60" },
    });
    expect(tenantResolverClientIp(spoofedCloudflare, "127.0.0.1")).toBe("127.0.0.1");
  });

  test("binds only loopback and exposes an exact health endpoint", async () => {
    const service = startTenantResolver({ registry: lookup(), hmacKey: HMAC_KEY, port: 0 });
    services.push(service);
    expect(service.server.hostname).toBe(TENANT_RESOLVER_HOST);
    const base = `http://${TENANT_RESOLVER_HOST}:${service.server.port}`;
    const healthy = await fetch(`${base}${TENANT_RESOLVER_HEALTH_PATH}`);
    expect(healthy.status).toBe(200);
    expect(await healthy.json()).toEqual({ ok: true });
    expect((await fetch(`${base}${TENANT_RESOLVER_HEALTH_PATH}?full=1`)).status).toBe(404);
  });

  test("rejects undersized keys and relative runtime paths", () => {
    expect(() => new TenantResolver({ registry: lookup(), hmacKey: new Uint8Array(31) })).toThrow("32");
    expect(() => loadTenantResolverRuntimeConfig({
      ROOST_SAAS_ROOT: "relative",
      ROOST_SAAS_RESOLVER_HMAC_KEY_FILE: "/etc/roost/key",
    })).toThrow("ROOST_SAAS_ROOT");
    expect(loadTenantResolverRuntimeConfig({
      ROOST_SAAS_ROOT: "/srv/test-roost",
      ROOST_SAAS_RESOLVER_HMAC_KEY_FILE: "/etc/roost/test.key",
    })).toEqual({
      rootDir: "/srv/test-roost",
      registryPath: "/srv/test-roost/control.db",
      hmacKeyFile: "/etc/roost/test.key",
    });
  });
});
