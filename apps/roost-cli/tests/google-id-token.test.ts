import { beforeAll, describe, expect, test } from "bun:test";
import { randomBytes } from "node:crypto";
import {
  createLocalJWKSet,
  createRemoteJWKSet,
  customFetch,
  exportJWK,
  generateKeyPair,
  SignJWT,
  type FetchImplementation,
  type JWTVerifyGetKey,
  type JWK,
} from "jose";
import {
  GOOGLE_IDENTITY_ISSUER,
  verifyGoogleIdToken,
} from "../src/saas-auth/google-id-token.ts";

const CLIENT_ID = "roost-test.apps.googleusercontent.com";
const NONCE = "test-nonce";
const REMOTE_JWKS_URL = "https://jwks.fixture.test/google";

interface SigningKey {
  privateKey: CryptoKey;
  kid: string;
  jwk: JWK;
}

async function signingKey(kid: string): Promise<SigningKey> {
  const { privateKey, publicKey } = await generateKeyPair("RS256");
  const jwk = await exportJWK(publicKey);
  return {
    privateKey,
    kid,
    jwk: { ...jwk, alg: "RS256", kid, use: "sig" },
  };
}

interface TokenOptions {
  claims?: Record<string, unknown>;
  omit?: string[];
  header?: { alg: string; kid?: string };
}

async function signToken(key: SigningKey, options: TokenOptions = {}): Promise<string> {
  const now = Math.floor(Date.now() / 1_000);
  const payload: Record<string, unknown> = {
    iss: GOOGLE_IDENTITY_ISSUER,
    aud: CLIENT_ID,
    sub: "google-subject-1",
    iat: now,
    exp: now + 300,
    nonce: NONCE,
    email: " Owner@Example.COM ",
    email_verified: true,
    azp: CLIENT_ID,
    ...options.claims,
  };
  for (const claim of options.omit ?? []) delete payload[claim];
  return new SignJWT(payload)
    .setProtectedHeader(options.header ?? { alg: "RS256", kid: key.kid })
    .sign(key.privateKey);
}

class RemoteJwksFixture {
  keys: SigningKey[];
  calls = 0;
  outage = false;
  beforeResponse: (() => Promise<void>) | undefined;

  constructor(...keys: SigningKey[]) {
    this.keys = keys;
  }

  readonly fetch: FetchImplementation = async (url, init) => {
    if (url !== REMOTE_JWKS_URL || init.method !== "GET" || init.redirect !== "manual") {
      throw new Error("unexpected JWKS request");
    }
    this.calls++;
    await this.beforeResponse?.();
    if (this.outage) throw new TypeError("fixture JWKS outage");
    return new Response(JSON.stringify({ keys: this.keys.map((key) => key.jwk) }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  resolver(options: { cooldownDuration?: number; cacheMaxAge?: number } = {}) {
    return createRemoteJWKSet(new URL(REMOTE_JWKS_URL), {
      timeoutDuration: 1_000,
      cooldownDuration: options.cooldownDuration ?? 0,
      cacheMaxAge: options.cacheMaxAge ?? 60_000,
      [customFetch]: this.fetch,
    });
  }
}

let trusted: SigningKey;
let rotated: SigningKey;
let attacker: SigningKey;
let localJwks: JWTVerifyGetKey;

beforeAll(async () => {
  [trusted, rotated, attacker] = await Promise.all([
    signingKey("trusted-key"),
    signingKey("rotated-key"),
    signingKey("attacker-key"),
  ]);
  localJwks = createLocalJWKSet({ keys: [trusted.jwk] });
});

function verify(token: string, jwks: JWTVerifyGetKey = localJwks, expectedNonce = NONCE) {
  return verifyGoogleIdToken(token, {
    clientId: CLIENT_ID,
    expectedNonce,
    jwks,
  });
}

describe("Google ID-token claims", () => {
  test.each([
    GOOGLE_IDENTITY_ISSUER,
    "accounts.google.com",
  ])("accepts issuer %s and canonicalizes the identity", async (issuer) => {
    await expect(verify(await signToken(trusted, { claims: { iss: issuer } }))).resolves.toEqual({
      issuer: GOOGLE_IDENTITY_ISSUER,
      subject: "google-subject-1",
      emailNormalized: "owner@example.com",
    });
  });

  test("accepts an absent optional azp claim", async () => {
    await expect(verify(await signToken(trusted, { omit: ["azp"] }))).resolves.toMatchObject({
      subject: "google-subject-1",
    });
  });

  test.each([
    ["audience", { aud: "another-client.apps.googleusercontent.com" }, NONCE],
    ["authorized party", { azp: "another-client.apps.googleusercontent.com" }, NONCE],
    ["nonce", { nonce: "token-nonce" }, "expected-nonce"],
    ["expiry", { exp: Math.floor(Date.now() / 1_000) - 1 }, NONCE],
    ["issuer", { iss: "https://issuer.invalid" }, NONCE],
    ["email verification", { email_verified: false }, NONCE],
  ])("rejects wrong %s", async (_label, claims, expectedNonce) => {
    await expect(verify(await signToken(trusted, { claims }), localJwks, expectedNonce))
      .rejects.toBeDefined();
  });

  test.each([
    "exp",
    "sub",
    "nonce",
    "email",
    "email_verified",
  ])("rejects a token missing required claim %s", async (claim) => {
    await expect(verify(await signToken(trusted, { omit: [claim] }))).rejects.toBeDefined();
  });
});

describe("Google ID-token signature boundary", () => {
  test("rejects a valid-looking token signed by another RSA key", async () => {
    await expect(verify(await signToken(attacker, {
      header: { alg: "RS256", kid: trusted.kid },
    }))).rejects.toBeDefined();
  });

  test("rejects an unknown key id", async () => {
    await expect(verify(await signToken(trusted, {
      header: { alg: "RS256", kid: "unknown-key" },
    }))).rejects.toBeDefined();
  });

  test("rejects a non-RS256 algorithm before key selection", async () => {
    const now = Math.floor(Date.now() / 1_000);
    const token = await new SignJWT({
      iss: GOOGLE_IDENTITY_ISSUER,
      aud: CLIENT_ID,
      sub: "google-subject-1",
      exp: now + 300,
      nonce: NONCE,
      email: "owner@example.com",
      email_verified: true,
    })
      .setProtectedHeader({ alg: "HS256", kid: trusted.kid })
      .sign(randomBytes(32));
    await expect(verify(token)).rejects.toBeDefined();
  });
});

describe("remote Google JWKS behavior", () => {
  test("coalesces concurrent verification into one remote fetch", async () => {
    const fixture = new RemoteJwksFixture(trusted);
    let announceStarted!: () => void;
    let releaseFetch!: () => void;
    const started = new Promise<void>((resolve) => { announceStarted = resolve; });
    const released = new Promise<void>((resolve) => { releaseFetch = resolve; });
    fixture.beforeResponse = async () => {
      announceStarted();
      await released;
    };
    const jwks = fixture.resolver();
    const token = await signToken(trusted);
    const pending = Array.from({ length: 12 }, () => verify(token, jwks));
    await started;
    expect(fixture.calls).toBe(1);
    releaseFetch();
    await expect(Promise.all(pending)).resolves.toHaveLength(12);
    expect(fixture.calls).toBe(1);
  });

  test("reloads once for a rotated key outside cooldown", async () => {
    const fixture = new RemoteJwksFixture(trusted);
    const jwks = fixture.resolver({ cooldownDuration: 0 });
    await expect(verify(await signToken(trusted), jwks)).resolves.toBeDefined();
    fixture.keys = [rotated];
    await expect(verify(await signToken(rotated), jwks)).resolves.toBeDefined();
    expect(fixture.calls).toBe(2);
  });

  test("does not refetch an unknown rotated kid during cooldown", async () => {
    const fixture = new RemoteJwksFixture(trusted);
    const jwks = fixture.resolver({ cooldownDuration: 60_000 });
    await expect(verify(await signToken(trusted), jwks)).resolves.toBeDefined();
    fixture.keys = [rotated];
    await expect(verify(await signToken(rotated), jwks)).rejects.toBeDefined();
    await expect(verify(await signToken(rotated), jwks)).rejects.toBeDefined();
    expect(fixture.calls).toBe(1);
    expect(jwks.coolingDown).toBe(true);
  });

  test("uses a fresh cached key while the JWKS endpoint is down", async () => {
    const fixture = new RemoteJwksFixture(trusted);
    const jwks = fixture.resolver({ cacheMaxAge: 60_000 });
    const token = await signToken(trusted);
    await expect(verify(token, jwks)).resolves.toBeDefined();
    fixture.outage = true;
    await expect(verify(token, jwks)).resolves.toBeDefined();
    expect(fixture.calls).toBe(1);
    expect(jwks.fresh).toBe(true);
  });

  test("fails closed on an outage with no usable cache", async () => {
    const fixture = new RemoteJwksFixture(trusted);
    fixture.outage = true;
    await expect(verify(await signToken(trusted), fixture.resolver())).rejects.toBeDefined();
    expect(fixture.calls).toBe(1);
  });

  test("fails closed when an expired cache cannot be refreshed", async () => {
    const fixture = new RemoteJwksFixture(trusted);
    const jwks = fixture.resolver({ cacheMaxAge: 0 });
    const token = await signToken(trusted);
    await expect(verify(token, jwks)).resolves.toBeDefined();
    fixture.outage = true;
    await expect(verify(token, jwks)).rejects.toBeDefined();
    expect(fixture.calls).toBe(2);
    expect(jwks.fresh).toBe(false);
  });
});
