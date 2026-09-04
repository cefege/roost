import { afterEach, describe, expect, test } from "bun:test";
import { generateKeyPairSync } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { canonicalJson, parseStrictJson } from "../src/saas-auth/canonical-json.ts";
import {
  parsePrivateIpcEnvelope,
  serializePrivateIpcEnvelope,
  signPrivateIpcEnvelope,
  verifyPrivateIpcEnvelope,
} from "../src/saas-auth/private-ipc.ts";
import {
  GatewayStateError,
  GatewayStateStore,
} from "../src/saas-auth/state-store.ts";
import { SaasAuthGateway, type SaasAuthGatewayOptions } from "../src/saas-auth/http-server.ts";
import { gatewayJson } from "../src/saas-auth/request-security.ts";
import { TurnstileVerifier } from "../src/saas-auth/turnstile.ts";

const roots: string[] = [];
afterEach(() => {
  while (roots.length > 0) rmSync(roots.pop()!, { recursive: true, force: true });
});

function stateStore(now = { value: 1_000 }) {
  const root = mkdtempSync(join(tmpdir(), "roost-signup-state-"));
  roots.push(root);
  const ids = [
    "11111111-1111-4111-8111-111111111111",
    "22222222-2222-4222-8222-222222222222",
    "33333333-3333-4333-8333-333333333333",
    "44444444-4444-4444-8444-444444444444",
    "55555555-5555-4555-8555-555555555555",
  ];
  return new GatewayStateStore({
    path: join(root, "auth.db"),
    oauthStateKey: Buffer.alloc(32, 7).toString("base64url"),
    now: () => now.value,
    createId: () => {
      const id = ids.shift();
      if (!id) throw new Error("fixture UUIDs exhausted");
      return id;
    },
  });
}

describe("gateway durable state", () => {
  test("leases signup email with CAS and keeps verification challenge hashed", () => {
    const now = { value: 1_000 };
    const store = stateStore(now);
    try {
      const challenge = store.createEmailChallenge({
        tokenHash: "a".repeat(64),
        emailNormalized: "owner@example.test",
        encryptedPayload: "encrypted-rendered-email",
      });
      expect(challenge).toMatchObject({ state: "pending", emailNormalized: "owner@example.test" });
      expect(store.verifyEmailChallenge("b".repeat(64))).toBeNull();
      expect(store.verifyEmailChallenge("a".repeat(64))?.state).toBe("verified");
      const [lease] = store.claimDueSignupEmails({ leaseDurationMs: 5_000, limit: 1 });
      expect(lease?.recipient).toBe("owner@example.test");
      expect(store.markSignupEmailSent({ ...lease!, leaseToken: "wrong" }, "provider", 1_100)).toBe(false);
      expect(store.markSignupEmailSent(lease!, "provider", 1_100)).toBe(true);
      expect(store.consumeEmailChallenge(challenge.id, 1_200)).toBe(true);
      expect(store.consumeEmailChallenge(challenge.id, 1_201)).toBe(false);
    } finally {
      store.close();
    }
  });

  test("invalidates prior browser attempts and binds a result to one device idempotently", () => {
    const now = { value: 10_000 };
    const store = stateStore(now);
    const browserCookie = "b".repeat(32);
    try {
      store.startOAuthAttempt({
        browserCookie,
        oauthCookie: "o".repeat(32),
        state: "s".repeat(32),
        pkceVerifier: "p".repeat(43),
        nonce: "n".repeat(32),
        intent: "login",
      });
      store.startOAuthAttempt({
        browserCookie,
        oauthCookie: "q".repeat(32),
        state: "t".repeat(32),
        pkceVerifier: "v".repeat(43),
        nonce: "m".repeat(32),
        intent: "login",
      });
      expect(store.consumeOAuthAttempt("o".repeat(32), "s".repeat(32))).toBeNull();
      expect(store.consumeOAuthAttempt("q".repeat(32), "t".repeat(32))?.pkceVerifier).toBe("v".repeat(43));

      const receipt = "r".repeat(32);
      store.createResultReceipt({
        receipt,
        browserCookie,
        jobId: "job-1",
        expiresAtMs: now.value + 60_000,
      });
      expect(store.setResultOutcome({
        jobId: "job-1",
        state: "awaiting-device",
        routeKey: "c".repeat(64),
        assertionInput: "canonical-input",
      })).toBe(true);
      expect(store.bindResultAssertion(receipt, "d".repeat(64), "signed-assertion")).toBe("signed-assertion");
      expect(store.bindResultAssertion(receipt, "d".repeat(64), "ignored-retry-value")).toBe("signed-assertion");
      expect(() => store.bindResultAssertion(receipt, "e".repeat(64), "other"))
        .toThrow(GatewayStateError);
    } finally {
      store.close();
    }
  });
});

describe("private provisioner protocol", () => {
  test("canonicalizes strict values and rejects duplicate keys, floats, and prototypes", () => {
    expect(canonicalJson({ z: 1, a: [true, null, "x"] })).toBe('{"a":[true,null,"x"],"z":1}');
    expect(() => canonicalJson({ value: 1.5 })).toThrow();
    expect(() => parseStrictJson('{"a":1,"a":2}')).toThrow();
    expect(() => canonicalJson(Object.create({ inherited: true }))).toThrow();
  });

  test("signs exact fresh envelopes and rejects body or signature tampering", () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const request = {
      purpose: "status" as const,
      body: { jobId: "11111111-1111-4111-8111-111111111111" },
    };
    const envelope = signPrivateIpcEnvelope(request, privateKey, {
      issuedAtMs: 50_000,
      nonce: Buffer.alloc(32, 9).toString("base64url"),
    });
    const parsed = parsePrivateIpcEnvelope(serializePrivateIpcEnvelope(envelope));
    expect(verifyPrivateIpcEnvelope(parsed, publicKey, 50_001)).toEqual(request);
    expect(() => verifyPrivateIpcEnvelope(parsed, publicKey, 90_001)).toThrow("clock skew");
    const tampered = { ...parsed, body: { jobId: "22222222-2222-4222-8222-222222222222" } };
    expect(() => verifyPrivateIpcEnvelope(tampered, publicKey, 50_001)).toThrow("signature");
  });
});

describe("public gateway route boundary", () => {
  function gateway() {
    const email = {
      start: async () => gatewayJson({ state: "verification-pending" }, 202),
      verify: async () => gatewayJson({ state: "pending" }, 202),
    };
    const google = {
      start: async () => gatewayJson({ authorizationUrl: "https://accounts.google.com/" }),
      callback: async () => new Response(null, { status: 303, headers: { location: "/auth/google/complete" } }),
    };
    const result = {
      get: async () => gatewayJson({ state: "pending" }, 202),
      bind: async () => gatewayJson({ state: "ready" }),
      completeLink: async () => gatewayJson({ state: "ready" }),
    };
    return new SaasAuthGateway({
      signupEnabled: false,
      googleEnabled: false,
      turnstileSiteKey: "",
      email,
      google,
      result,
    } as unknown as SaasAuthGatewayOptions);
  }

  function gatewayRequest(path: string, init: RequestInit = {}): Request {
    const headers = new Headers(init.headers);
    if (!headers.has("x-forwarded-for")) headers.set("x-forwarded-for", "203.0.113.7");
    if (!headers.has("cf-connecting-ip")) {
      headers.set("cf-connecting-ip", headers.get("x-forwarded-for") ?? "203.0.113.7");
    }
    return new Request(`https://dashboard.roosttt.com${path}`, { ...init, headers });
  }

  test("admits only exact method/path pairs through the root loopback bridge", async () => {
    const auth = gateway();
    const config = await auth.fetch(gatewayRequest("/__roost/auth/config"), "127.0.0.1");
    expect(config.status).toBe(200);
    expect(await config.json()).toEqual({
      signupEnabled: false,
      googleEnabled: false,
      turnstileSiteKey: "",
    });
    expect(config.headers.get("cache-control")).toBe("no-store");
    expect((await auth.fetch(gatewayRequest("/__roost/auth/config?x=1"), "127.0.0.1")).status).toBe(404);
    expect((await auth.fetch(gatewayRequest("/__roost/unknown"), "127.0.0.1")).status).toBe(404);
    expect((await auth.fetch(gatewayRequest("/__roost/auth/config"), "127.0.0.2")).status).toBe(400);
  });

  test("requires exact Origin, JSON, no encoding, and one canonical client IP", async () => {
    const auth = gateway();
    const validHeaders = {
      origin: "https://dashboard.roosttt.com",
      "content-type": "application/json",
    };
    const valid = await auth.fetch(gatewayRequest("/__roost/signup/email/start", {
      method: "POST",
      headers: validHeaders,
      body: JSON.stringify({ email: "owner@example.test", turnstileToken: "token" }),
    }), "127.0.0.1");
    expect(valid.status).toBe(202);
    for (const headers of [
      { "content-type": "application/json" },
      { ...validHeaders, "content-type": "application/json; charset=utf-8" },
      { ...validHeaders, "content-encoding": "gzip" },
      { ...validHeaders, "x-forwarded-for": "203.0.113.7, 198.51.100.2" },
    ]) {
      const response = await auth.fetch(gatewayRequest("/__roost/signup/email/start", {
        method: "POST",
        headers,
        body: "{}",
      }), "127.0.0.1");
      expect(response.status).toBe(400);
    }
  });
});

describe("Turnstile admission proof", () => {
  const NOW_MS = 1_700_000_000_000;

  function success(nowMs = NOW_MS, overrides: Record<string, unknown> = {}) {
    return {
      success: true,
      hostname: "dashboard.roosttt.com",
      action: "signup",
      challenge_ts: new Date(nowMs).toISOString(),
      ...overrides,
    };
  }

  test("reuses one provider idempotency UUID on retry and consumes a token once", async () => {
    const now = { value: NOW_MS };
    const store = stateStore(now);
    const bodies: string[] = [];
    let calls = 0;
    const verifier = new TurnstileVerifier({
      store,
      secret: "turnstile-test-secret",
      now: () => now.value,
      createId: () => "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      endpoint: "https://turnstile.fixture/siteverify",
      fetch: async (_input, init) => {
        bodies.push(String(init?.body));
        calls++;
        if (calls === 1) return new Response("{}", { status: 503 });
        return new Response(JSON.stringify(success(now.value)), { status: 200 });
      },
    });
    try {
      expect(await verifier.verify("browser-turnstile-token", "203.0.113.7")).toBe(true);
      expect(calls).toBe(2);
      const first = new URLSearchParams(bodies[0]);
      const second = new URLSearchParams(bodies[1]);
      expect(first.get("idempotency_key")).toBe("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
      expect(second.get("idempotency_key")).toBe(first.get("idempotency_key"));
      expect(second.get("response")).toBe("browser-turnstile-token");
      expect(second.get("remoteip")).toBe("203.0.113.7");
      expect(second.get("secret")).toBe("turnstile-test-secret");
      expect(await verifier.verify("browser-turnstile-token", "203.0.113.7")).toBe(false);
      expect(calls).toBe(2);
    } finally {
      store.close();
    }
  });

  test.each([
    ["wrong hostname", success(NOW_MS, { hostname: "evil.example" })],
    ["wrong action", success(NOW_MS, { action: "login" })],
    ["stale challenge", success(NOW_MS - 300_001)],
    ["future challenge", success(NOW_MS + 30_001)],
    ["explicit provider failure", { success: false, "error-codes": ["invalid-input-response"] }],
    ["provider duplicate", { success: false, "error-codes": ["timeout-or-duplicate"] }],
  ])("rejects %s permanently", async (_label, providerBody) => {
    const now = { value: NOW_MS };
    const store = stateStore(now);
    let calls = 0;
    const verifier = new TurnstileVerifier({
      store,
      secret: "turnstile-test-secret",
      now: () => now.value,
      createId: () => "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      endpoint: "https://turnstile.fixture/siteverify",
      fetch: async () => {
        calls++;
        return new Response(JSON.stringify(providerBody), { status: 200 });
      },
    });
    try {
      expect(await verifier.verify(`token-${_label}`, "203.0.113.7")).toBe(false);
      expect(calls).toBe(1);
      expect(await verifier.verify(`token-${_label}`, "203.0.113.7")).toBe(false);
      expect(calls).toBe(1);
    } finally {
      store.close();
    }
  });

  test("accepts exactly 2,048 bytes and a challenge exactly 300 seconds old", async () => {
    const now = { value: NOW_MS };
    const store = stateStore(now);
    let calls = 0;
    const verifier = new TurnstileVerifier({
      store,
      secret: "turnstile-test-secret",
      now: () => now.value,
      createId: () => "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
      fetch: async () => {
        calls++;
        return new Response(JSON.stringify(success(now.value - 300_000)), { status: 200 });
      },
    });
    try {
      expect(await verifier.verify("x".repeat(2_048), "203.0.113.7")).toBe(true);
      expect(calls).toBe(1);
      expect(await verifier.verify("y".repeat(2_049), "203.0.113.7")).toBe(false);
      expect(calls).toBe(1);
    } finally {
      store.close();
    }
  });

  test("fails closed on network outage and stops retrying after 300 seconds", async () => {
    const now = { value: NOW_MS };
    const store = stateStore(now);
    const ids: string[] = [];
    let calls = 0;
    const verifier = new TurnstileVerifier({
      store,
      secret: "turnstile-test-secret",
      now: () => now.value,
      createId: () => "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
      fetch: async (_input, init) => {
        calls++;
        ids.push(new URLSearchParams(String(init?.body)).get("idempotency_key") ?? "");
        throw new TypeError("fixture network outage");
      },
    });
    try {
      expect(await verifier.verify("network-failure-token", "203.0.113.7")).toBe(false);
      expect(calls).toBe(2);
      expect(ids).toEqual([
        "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
        "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
      ]);
      now.value += 300_000;
      expect(await verifier.verify("network-failure-token", "203.0.113.7")).toBe(false);
      expect(calls).toBe(2);
    } finally {
      store.close();
    }
  });

  test("fails closed on malformed internal provider responses", async () => {
    const now = { value: NOW_MS };
    const store = stateStore(now);
    let calls = 0;
    const verifier = new TurnstileVerifier({
      store,
      secret: "turnstile-test-secret",
      now: () => now.value,
      createId: () => "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
      fetch: async () => {
        calls++;
        return new Response('{"success":true,"success":false}', { status: 200 });
      },
    });
    try {
      expect(await verifier.verify("malformed-response-token", "203.0.113.7")).toBe(false);
      expect(calls).toBe(2);
    } finally {
      store.close();
    }
  });
});
