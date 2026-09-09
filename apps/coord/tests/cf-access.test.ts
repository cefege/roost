import { beforeAll, describe, expect, test } from "bun:test";
import type { CoordConfig } from "@roost/shared/config";
import {
  createCloudflareAccessGate,
  type CloudflareAccessGate,
} from "../src/cf-access.ts";

const TEAM_DOMAIN = "owner.cloudflareaccess.com";
const AUDIENCE = "a".repeat(64);
const CONFIG = {
  cfAccessTeamDomain: TEAM_DOMAIN,
  cfAccessAud: AUDIENCE,
} as CoordConfig;

let signingKey: CryptoKey;
let publicJwk: JsonWebKey;

beforeAll(async () => {
  const pair = await crypto.subtle.generateKey({
    name: "RSASSA-PKCS1-v1_5",
    modulusLength: 2048,
    publicExponent: new Uint8Array([1, 0, 1]),
    hash: "SHA-256",
  }, true, ["sign", "verify"]);
  signingKey = pair.privateKey;
  publicJwk = {
    ...await crypto.subtle.exportKey("jwk", pair.publicKey),
    kid: "access-key-1",
    alg: "RS256",
    use: "sig",
  } as JsonWebKey;
});

function encodeJson(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString("base64url");
}

async function signedToken(
  claims: Record<string, unknown>,
  header: Record<string, unknown> = { alg: "RS256", kid: "access-key-1" },
): Promise<string> {
  const headerPart = encodeJson(header);
  const payloadPart = encodeJson(claims);
  const message = new TextEncoder().encode(`${headerPart}.${payloadPart}`);
  const signature = await crypto.subtle.sign(
    { name: "RSASSA-PKCS1-v1_5" },
    signingKey,
    message,
  );
  return `${headerPart}.${payloadPart}.${Buffer.from(signature).toString("base64url")}`;
}

function assertionHeaders(assertion: string): Headers {
  return new Headers({ "cf-access-jwt-assertion": assertion });
}

function validClaims(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  const nowSeconds = Math.floor(Date.now() / 1000);
  return {
    iss: `https://${TEAM_DOMAIN}`,
    aud: [AUDIENCE],
    exp: nowSeconds + 300,
    iat: nowSeconds,
    email: "owner@example.com",
    sub: "cf-subject-1",
    ...overrides,
  };
}

function installFakeJwks(): { calls: number; restore: () => void } {
  let calls = 0;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = (async () => {
    calls += 1;
    return new Response(JSON.stringify({ keys: [publicJwk] }), {
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;
  return {
    get calls() { return calls; },
    restore: () => { globalThis.fetch = originalFetch; },
  };
}

describe("Cloudflare Access gate", () => {
  test("accepts a valid RS256 assertion and caches its JWKS", async () => {
    const fakeJwks = installFakeJwks();
    try {
      const gate = createCloudflareAccessGate(CONFIG);
      expect(gate).not.toBeNull();
      const assertion = await signedToken(validClaims());
      const identity = await gate!.verify(assertionHeaders(assertion));
      expect(identity).toEqual({ email: "owner@example.com", subject: "cf-subject-1" });
      expect(await gate!.verify(assertionHeaders(assertion))).toEqual(identity);
      expect(fakeJwks.calls).toBe(1);
    } finally {
      fakeJwks.restore();
    }
  });

  test("fails closed for absent, malformed, unsigned, unknown, tampered, and bad claims", async () => {
    const fakeJwks = installFakeJwks();
    try {
      const gate = createCloudflareAccessGate(CONFIG) as CloudflareAccessGate;
      // Prime the cache so claim and signature rejection cases all exercise the
      // same fetched key rather than depending on fetch order.
      await gate.verify(assertionHeaders(await signedToken(validClaims())));

      expect(await gate.verify(new Headers())).toBeNull();
      expect(await gate.verify(assertionHeaders("not-a-jwt"))).toBeNull();

      const unsignedHeader = encodeJson({ alg: "none", kid: "access-key-1" });
      const unsignedPayload = encodeJson(validClaims());
      expect(await gate.verify(assertionHeaders(`${unsignedHeader}.${unsignedPayload}.AA`))).toBeNull();

      expect(await gate.verify(assertionHeaders(
        await signedToken(validClaims(), { alg: "RS256", kid: "unknown-key" }),
      ))).toBeNull();
      expect(await gate.verify(assertionHeaders(
        await signedToken(validClaims({ iss: "https://wrong.cloudflareaccess.com" })),
      ))).toBeNull();
      expect(await gate.verify(assertionHeaders(
        await signedToken(validClaims({ aud: ["different-audience"] })),
      ))).toBeNull();
      expect(await gate.verify(assertionHeaders(
        await signedToken(validClaims({ exp: Math.floor(Date.now() / 1000) - 120 })),
      ))).toBeNull();

      const tamperedParts = (await signedToken(validClaims())).split(".");
      tamperedParts[1] = encodeJson(validClaims({ email: "attacker@example.com" }));
      expect(await gate.verify(assertionHeaders(tamperedParts.join(".")))).toBeNull();
    } finally {
      fakeJwks.restore();
    }
  });

  test("is inert when Access configuration is absent", () => {
    expect(createCloudflareAccessGate({} as CoordConfig)).toBeNull();
  });
});
