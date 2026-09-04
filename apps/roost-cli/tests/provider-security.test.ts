import { afterEach, beforeAll, describe, expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createLocalJWKSet,
  exportJWK,
  generateKeyPair,
  SignJWT,
  type JWK,
  type JWTVerifyGetKey,
} from "jose";
import {
  GOOGLE_CALLBACK_URL,
  GoogleOAuthProtocol,
  OAUTH_COOKIE,
  RESULT_COOKIE,
} from "../src/saas-auth/google-oauth.ts";
import { GOOGLE_IDENTITY_ISSUER } from "../src/saas-auth/google-id-token.ts";
import type { ProvisionerClient } from "../src/saas-auth/provisioner-client.ts";
import { GatewayStateStore } from "../src/saas-auth/state-store.ts";
import type { TurnstileVerifier } from "../src/saas-auth/turnstile.ts";

const CLIENT_ID = "roost-provider-fixture.apps.googleusercontent.com";
const CLIENT_SECRET = "fixture-client-secret-never-persist";
const TOKEN_ENDPOINT = "https://token.fixture.test/oauth2/token";
const NOW_MS = 1_700_000_000_000;
const roots: string[] = [];

interface SigningKey {
  privateKey: CryptoKey;
  jwk: JWK;
}

let signingKey: SigningKey;
let localJwks: JWTVerifyGetKey;

beforeAll(async () => {
  const { privateKey, publicKey } = await generateKeyPair("RS256");
  signingKey = {
    privateKey,
    jwk: { ...await exportJWK(publicKey), alg: "RS256", kid: "oauth-fixture", use: "sig" },
  };
  localJwks = createLocalJWKSet({ keys: [signingKey.jwk] });
});

afterEach(() => {
  while (roots.length > 0) rmSync(roots.pop()!, { recursive: true, force: true });
});

function openStore(): GatewayStateStore {
  const root = mkdtempSync(join(tmpdir(), "roost-provider-security-"));
  roots.push(root);
  let sequence = 0;
  return new GatewayStateStore({
    path: join(root, "auth.db"),
    oauthStateKey: Buffer.alloc(32, 17).toString("base64url"),
    now: () => NOW_MS,
    createId: () => {
      sequence++;
      return `00000000-0000-4000-8000-${sequence.toString().padStart(12, "0")}`;
    },
  });
}

async function idToken(nonce: string): Promise<string> {
  const now = Math.floor(Date.now() / 1_000);
  return new SignJWT({
    nonce,
    email: " Owner@Example.Test ",
    email_verified: true,
    azp: CLIENT_ID,
  })
    .setProtectedHeader({ alg: "RS256", kid: "oauth-fixture" })
    .setIssuer(GOOGLE_IDENTITY_ISSUER)
    .setAudience(CLIENT_ID)
    .setSubject("google-provider-subject")
    .setIssuedAt(now)
    .setExpirationTime(now + 300)
    .sign(signingKey.privateKey);
}

interface CapturedTokenRequest {
  url: string;
  init: RequestInit | undefined;
}

interface ProtocolFixture {
  store: GatewayStateStore;
  protocol: GoogleOAuthProtocol;
  tokenRequests: CapturedTokenRequest[];
  submissions: unknown[];
  tokenResponse: { create: () => Response };
}

function protocolFixture(): ProtocolFixture {
  const store = openStore();
  const tokenRequests: CapturedTokenRequest[] = [];
  const submissions: unknown[] = [];
  const tokenResponse = {
    create: () => new Response("{}", { status: 500 }),
  };
  const provisioner = {
    submit: async (submission: unknown) => {
      submissions.push(submission);
      return { state: "pending" as const, jobId: "11111111-1111-4111-8111-111111111111" };
    },
  } as unknown as ProvisionerClient;
  const turnstile = { verify: async () => true } as unknown as TurnstileVerifier;
  const protocol = new GoogleOAuthProtocol({
    store,
    turnstile,
    provisioner,
    googleEnabled: true,
    signupEnabled: true,
    clientId: CLIENT_ID,
    clientSecret: CLIENT_SECRET,
    now: () => NOW_MS,
    jwks: localJwks,
    tokenEndpoint: TOKEN_ENDPOINT,
    fetch: async (input, init) => {
      tokenRequests.push({ url: String(input), init });
      return tokenResponse.create();
    },
  });
  return { store, protocol, tokenRequests, submissions, tokenResponse };
}

interface StartedOAuth {
  state: string;
  nonce: string;
  codeChallenge: string;
  oauthCookie: string;
}

function responseCookies(response: Response): string[] {
  const headers = response.headers as Headers & { getSetCookie?: () => string[] };
  if (headers.getSetCookie) return headers.getSetCookie();
  const combined = response.headers.get("set-cookie");
  return combined === null ? [] : combined.split(/,(?=\s*__Host-)/u);
}

function findCookie(response: Response, name: string): string | null {
  const prefix = `${name}=`;
  for (const cookie of responseCookies(response)) {
    const trimmed = cookie.trim();
    if (trimmed.startsWith(prefix)) return trimmed.slice(prefix.length).split(";", 1)[0] ?? null;
  }
  return null;
}

async function startLogin(fixture: ProtocolFixture): Promise<StartedOAuth> {
  const response = await fixture.protocol.start(
    new Request("https://dashboard.roosttt.com/__roost/auth/google/start", { method: "POST" }),
    { intent: "login" },
    "203.0.113.7",
  );
  expect(response.status).toBe(200);
  const payload = await response.json() as { authorizationUrl: string };
  const authorization = new URL(payload.authorizationUrl);
  expect(authorization.origin).toBe("https://accounts.google.com");
  const state = authorization.searchParams.get("state");
  const nonce = authorization.searchParams.get("nonce");
  const codeChallenge = authorization.searchParams.get("code_challenge");
  const oauthCookie = findCookie(response, OAUTH_COOKIE);
  if (!state || !nonce || !codeChallenge || !oauthCookie) throw new Error("OAuth fixture start was incomplete");
  return { state, nonce, codeChallenge, oauthCookie };
}

function callbackRequest(started: StartedOAuth, query: Record<string, string>): { request: Request; url: URL } {
  const url = new URL(GOOGLE_CALLBACK_URL);
  for (const [key, value] of Object.entries({ state: started.state, ...query })) url.searchParams.set(key, value);
  return {
    request: new Request(url, { headers: { cookie: `${OAUTH_COOKIE}=${started.oauthCookie}` } }),
    url,
  };
}

function expectStateDoesNotContain(store: GatewayStateStore, values: string[]): void {
  const paths = [store.path, `${store.path}-wal`, `${store.path}-shm`];
  for (const path of paths) {
    if (!existsSync(path)) continue;
    const bytes = readFileSync(path);
    for (const value of values) expect(bytes.includes(Buffer.from(value, "utf8"))).toBe(false);
  }
}

function responseMaterial(response: Response): string {
  return [response.headers.get("location"), ...responseCookies(response)].join("\n");
}

describe("Google callback provider fixture", () => {
  test("exchanges code with exact PKCE form, consumes state once, and retains no provider credential", async () => {
    const fixture = protocolFixture();
    const code = "provider-code-never-persist";
    const accessToken = "access-token-never-persist";
    const refreshToken = "refresh-token-never-persist";
    const errorSpy = spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = spyOn(console, "warn").mockImplementation(() => {});
    try {
      const started = await startLogin(fixture);
      const signedIdToken = await idToken(started.nonce);
      fixture.tokenResponse.create = () => new Response(JSON.stringify({
        id_token: signedIdToken,
        access_token: accessToken,
        refresh_token: refreshToken,
        token_type: "Bearer",
      }), { status: 200, headers: { "content-type": "application/json" } });
      const callback = callbackRequest(started, {
        code,
        iss: GOOGLE_IDENTITY_ISSUER,
        scope: "openid email",
      });
      const response = await fixture.protocol.callback(callback.request, callback.url);

      expect(response.status).toBe(303);
      expect(response.headers.get("location")).toBe("/auth/google/complete");
      expect(response.headers.get("cache-control")).toBe("no-store");
      expect(response.headers.get("referrer-policy")).toBe("no-referrer");
      expect(fixture.tokenRequests).toHaveLength(1);
      expect(fixture.tokenRequests[0]?.url).toBe(TOKEN_ENDPOINT);
      expect(fixture.tokenRequests[0]?.init?.redirect).toBe("manual");
      const form = new URLSearchParams(String(fixture.tokenRequests[0]?.init?.body));
      const verifier = form.get("code_verifier");
      if (verifier === null) throw new Error("OAuth token request omitted its PKCE verifier");
      expect(Object.fromEntries(form)).toEqual({
        code,
        client_id: CLIENT_ID,
        client_secret: CLIENT_SECRET,
        redirect_uri: GOOGLE_CALLBACK_URL,
        grant_type: "authorization_code",
        code_verifier: verifier,
      });
      expect(verifier).toMatch(/^[A-Za-z0-9_-]{43}$/u);
      expect(createHash("sha256").update(verifier, "ascii").digest("base64url"))
        .toBe(started.codeChallenge);
      expect(fixture.submissions).toEqual([{
        kind: "google-login",
        submission: {
          emailNormalized: "owner@example.test",
          identityIssuer: GOOGLE_IDENTITY_ISSUER,
          identitySubject: "google-provider-subject",
          verifiedAtMs: NOW_MS,
          idempotencyKey: expect.stringMatching(/^[0-9a-f]{64}$/u),
        },
      }]);

      const receipt = findCookie(response, RESULT_COOKIE);
      expect(receipt).not.toBeNull();
      if (receipt === null) throw new Error("OAuth callback omitted its result receipt");
      expect(fixture.store.getResult(receipt)?.state).toBe("pending");
      const exposed = responseMaterial(response);
      for (const secret of [code, signedIdToken, accessToken, refreshToken, CLIENT_SECRET]) {
        expect(exposed).not.toContain(secret);
      }
      expectStateDoesNotContain(fixture.store, [code, signedIdToken, accessToken, refreshToken, CLIENT_SECRET]);
      const logged = [...errorSpy.mock.calls, ...warnSpy.mock.calls].flat().join(" ");
      for (const secret of [code, signedIdToken, accessToken, refreshToken, CLIENT_SECRET]) {
        expect(logged).not.toContain(secret);
      }

      const replay = await fixture.protocol.callback(callback.request, callback.url);
      expect(replay.status).toBe(303);
      expect(fixture.tokenRequests).toHaveLength(1);
      expect(fixture.submissions).toHaveLength(1);
      expect(findCookie(replay, RESULT_COOKIE)).toBeNull();
    } finally {
      errorSpy.mockRestore();
      warnSpy.mockRestore();
      fixture.store.close();
    }
  });

  test("turns a provider error into one opaque failed result and rejects state replay", async () => {
    const fixture = protocolFixture();
    const providerDescription = "provider-description-never-persist";
    try {
      const started = await startLogin(fixture);
      const callback = callbackRequest(started, {
        error: "access_denied",
        error_description: providerDescription,
        iss: "accounts.google.com",
      });
      const response = await fixture.protocol.callback(callback.request, callback.url);
      expect(response.status).toBe(303);
      expect(fixture.tokenRequests).toHaveLength(0);
      expect(fixture.submissions).toHaveLength(0);
      const receipt = findCookie(response, RESULT_COOKIE);
      expect(receipt).not.toBeNull();
      if (receipt === null) throw new Error("provider failure omitted its result receipt");
      expect(fixture.store.getResult(receipt)?.state).toBe("failed");
      expect(responseMaterial(response)).not.toContain(providerDescription);
      expectStateDoesNotContain(fixture.store, [providerDescription, "access_denied"]);

      const replay = await fixture.protocol.callback(callback.request, callback.url);
      expect(replay.status).toBe(303);
      expect(findCookie(replay, RESULT_COOKIE)).toBeNull();
      expect(fixture.tokenRequests).toHaveLength(0);
      expect(fixture.submissions).toHaveLength(0);
    } finally {
      fixture.store.close();
    }
  });

  test("a mismatched state does not exchange a code or consume the valid state", async () => {
    const fixture = protocolFixture();
    const code = "state-bound-provider-code";
    try {
      const started = await startLogin(fixture);
      const signedIdToken = await idToken(started.nonce);
      fixture.tokenResponse.create = () => new Response(JSON.stringify({ id_token: signedIdToken }), { status: 200 });
      const wrong = callbackRequest({ ...started, state: "w".repeat(43) }, { code });
      expect((await fixture.protocol.callback(wrong.request, wrong.url)).status).toBe(303);
      expect(fixture.tokenRequests).toHaveLength(0);

      const valid = callbackRequest(started, { code });
      expect((await fixture.protocol.callback(valid.request, valid.url)).status).toBe(303);
      expect(fixture.tokenRequests).toHaveLength(1);
      expect(fixture.submissions).toHaveLength(1);
      expect((await fixture.protocol.callback(valid.request, valid.url)).status).toBe(303);
      expect(fixture.tokenRequests).toHaveLength(1);
    } finally {
      fixture.store.close();
    }
  });

  test.each([
    ["64 KiB token response", () => new Response(JSON.stringify({ padding: "response-overflow".repeat(5_000) }), { status: 200 })],
    ["16 KiB ID token", () => new Response(JSON.stringify({ id_token: `${"a".repeat(16 * 1_024)}.b.c` }), { status: 200 })],
  ])("fails closed on an oversized %s", async (_label, createResponse) => {
    const fixture = protocolFixture();
    try {
      const started = await startLogin(fixture);
      fixture.tokenResponse.create = createResponse;
      const callback = callbackRequest(started, { code: "bounded-response-code" });
      const response = await fixture.protocol.callback(callback.request, callback.url);
      expect(response.status).toBe(303);
      expect(fixture.tokenRequests).toHaveLength(1);
      expect(fixture.submissions).toHaveLength(0);
      const receipt = findCookie(response, RESULT_COOKIE);
      expect(receipt).not.toBeNull();
      if (receipt === null) throw new Error("oversized provider response omitted its result receipt");
      expect(fixture.store.getResult(receipt)?.state).toBe("failed");
      expect(responseMaterial(response)).not.toContain("response-overflow");
      expectStateDoesNotContain(fixture.store, ["bounded-response-code", "response-overflow"]);
    } finally {
      fixture.store.close();
    }
  });
});
