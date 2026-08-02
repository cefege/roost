import { beforeAll, describe, expect, test } from "bun:test";
import { makeCfAccessVerifier } from "../src/middleware/cf-access.ts";
import {
  ACCESS_AUD,
  ACCESS_TEAM_DOMAIN,
  generateAccessSigningKey,
  jwksResponse,
  signAccessToken,
  validAccessClaims,
  type AccessSigningKey,
} from "./helpers/cf-access-fixture.ts";

let keyA: AccessSigningKey;
let keyB: AccessSigningKey;

beforeAll(async () => {
  [keyA, keyB] = await Promise.all([
    generateAccessSigningKey("key-a"),
    generateAccessSigningKey("key-b"),
  ]);
});

function requestWithToken(token: string, source: "header" | "cookie" = "header"): Request {
  return new Request("https://roost.example.test/", {
    headers: source === "header"
      ? { "cf-access-jwt-assertion": token }
      : { cookie: `unrelated=1; CF_Authorization=${token}; later=2` },
  });
}
function mockFetch(
  impl: (input: string | URL | Request, init?: RequestInit) => Promise<Response>,
): typeof fetch {
  return Object.assign(impl, {
    preconnect(_url: string | URL): void {},
  });
}


describe("Cloudflare Access application-token verification", () => {
  test("accepts a valid assertion header and cookie fallback", async () => {
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => 1_700_000_000_000,
      fetch: mockFetch(async () => { fetches++;
      return jwksResponse(keyA); }),
    });
    const token = await signAccessToken(keyA, validAccessClaims(1_700_000_000));

    await expect(verifier.verify(requestWithToken(token))).resolves.toEqual({
      sub: "access-user-1",
      email: "user@example.com",
      exp: 1_700_000_300,
    });
    await expect(verifier.verify(requestWithToken(token, "cookie"))).resolves.toMatchObject({
      sub: "access-user-1",
    });
    expect(fetches).toBe(1);
  });

  test("uses common_name when email is absent", async () => {
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => 1_700_000_000_000,
      fetch: mockFetch(async () => jwksResponse(keyA)),
    });
    const claims = validAccessClaims(1_700_000_000);
    delete claims.email;
    claims.common_name = "common@example.com";
    const token = await signAccessToken(keyA, claims);
    await expect(verifier.verify(requestWithToken(token))).resolves.toMatchObject({
      email: "common@example.com",
    });
  });

  test("rejects missing and oversized tokens before JWKS lookup", async () => {
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      fetch: mockFetch(async () => { fetches++;
      return jwksResponse(keyA); }),
    });
    await expect(verifier.verify(new Request("https://roost.example.test/")))
      .rejects.toThrow("no access token");
    await expect(verifier.verify(requestWithToken("x".repeat(16 * 1024 + 1))))
      .rejects.toThrow("access token too long");
    expect(fetches).toBe(0);
  });

  test("rejects an explicit wrong algorithm before key lookup", async () => {
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      fetch: mockFetch(async () => { fetches++;
      return jwksResponse(keyA); }),
    });
    const token = await signAccessToken(
      keyA,
      validAccessClaims(Math.floor(Date.now() / 1000)),
      { alg: "HS256", kid: keyA.kid },
    );
    await expect(verifier.verify(requestWithToken(token)))
      .rejects.toThrow("wrong access token alg");
    expect(fetches).toBe(0);
  });

  test("rejects unknown keys and invalid signatures", async () => {
    let nowMs = 1_700_000_000_000;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowMs,
      fetch: mockFetch(async () => jwksResponse(keyA)),
    });
    const claims = validAccessClaims(Math.floor(nowMs / 1000));
    const validA = await signAccessToken(keyA, claims);
    await verifier.verify(requestWithToken(validA));

    const unknown = await signAccessToken(keyB, claims);
    await expect(verifier.verify(requestWithToken(unknown)))
      .rejects.toThrow(`unknown access key ${keyB.kid}`);

    const badSignature = await signAccessToken(keyB, claims, { alg: "RS256", kid: keyA.kid });
    await expect(verifier.verify(requestWithToken(badSignature)))
      .rejects.toThrow("access token signature invalid");
    nowMs++;
  });

  test("rejects every invalid claim independently", async () => {
    const nowSecs = 1_700_000_000;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowSecs * 1000,
      fetch: mockFetch(async () => jwksResponse(keyA)),
    });
    const cases: Array<[string, (claims: Record<string, unknown>) => void, string]> = [
      ["wrong issuer", (claims) => { claims.iss = "https://attacker.example"; }, "wrong access token issuer"],
      ["non-array audience", (claims) => { claims.aud = ACCESS_AUD; }, "wrong access token audience"],
      ["missing configured audience", (claims) => { claims.aud = ["other"]; }, "wrong access token audience"],
      ["expired exp", (claims) => { claims.exp = nowSecs; }, "access token expired"],
      ["non-numeric exp", (claims) => { claims.exp = "later"; }, "invalid access token exp"],
      ["missing iat", (claims) => { delete claims.iat; }, "invalid access token iat"],
      ["null iat", (claims) => { claims.iat = null; }, "invalid access token iat"],
      ["string iat", (claims) => { claims.iat = String(nowSecs); }, "invalid access token iat"],
      ["future iat", (claims) => { claims.iat = nowSecs + 31; }, "access token iat in future"],
      ["null nbf", (claims) => { claims.nbf = null; }, "invalid access token nbf"],
      ["string nbf", (claims) => { claims.nbf = String(nowSecs); }, "invalid access token nbf"],
      ["future nbf", (claims) => { claims.nbf = nowSecs + 31; }, "access token nbf in future"],
      ["organization token", (claims) => { claims.type = "org"; }, "wrong access token type"],
      ["empty subject", (claims) => { claims.sub = ""; }, "missing access token subject"],
    ];

    for (const [label, mutate, message] of cases) {
      const claims = validAccessClaims(nowSecs);
      mutate(claims);
      const token = await signAccessToken(keyA, claims);
      await expect(verifier.verify(requestWithToken(token)), label).rejects.toThrow(message);
    }
  });
});

describe("Cloudflare Access JWKS lifecycle", () => {
  test("rotates successfully from key A to key B", async () => {
    let nowMs = 0;
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowMs,
      fetch: mockFetch(async () => { fetches++;
      return fetches === 1 ? jwksResponse(keyA) : jwksResponse(keyB); }),
    });
    const tokenA = await signAccessToken(keyA, validAccessClaims(0));
    await verifier.verify(requestWithToken(tokenA));

    nowMs = 60_000;
    const tokenB = await signAccessToken(keyB, validAccessClaims(60));
    await expect(verifier.verify(requestWithToken(tokenB))).resolves.toMatchObject({ sub: "access-user-1" });
    expect(fetches).toBe(2);
  });

  test("shares one unknown-kid refresh across concurrent callers", async () => {
    let nowMs = 0;
    let fetches = 0;
    let release!: (response: Response) => void;
    const pendingResponse = new Promise<Response>((resolve) => { release = resolve; });
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowMs,
      fetch: mockFetch(async () => { fetches++;
      return fetches === 1 ? jwksResponse(keyA) : pendingResponse; }),
    });
    await verifier.verify(requestWithToken(await signAccessToken(keyA, validAccessClaims(0))));

    nowMs = 60_000;
    const tokenB = await signAccessToken(keyB, validAccessClaims(60));
    const attempts = [1, 2, 3].map(() => verifier.verify(requestWithToken(tokenB)));
    await Promise.resolve();
    release(jwksResponse(keyB));
    await expect(Promise.all(attempts)).resolves.toHaveLength(3);
    expect(fetches).toBe(2);
  });

  test("failed refresh preserves a live map but fails closed after TTL", async () => {
    let nowMs = 0;
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowMs,
      fetch: mockFetch(async () => { fetches++;
      return fetches === 1 ? jwksResponse(keyA) : new Response("down", { status: 503 }); }),
    });
    const tokenA = await signAccessToken(keyA, validAccessClaims(0));
    await verifier.verify(requestWithToken(tokenA));

    nowMs = 60_000;
    const tokenB = await signAccessToken(keyB, validAccessClaims(60));
    await expect(verifier.verify(requestWithToken(tokenB))).rejects.toThrow("access JWKS fetch failed");
    await expect(verifier.verify(requestWithToken(tokenA))).resolves.toMatchObject({ sub: "access-user-1" });

    nowMs = 600_001;
    const freshTokenA = await signAccessToken(keyA, validAccessClaims(600));
    await expect(verifier.verify(requestWithToken(freshTokenA))).rejects.toThrow("access JWKS fetch failed");
  });

  test("retries an unknown key only after the sixty-second floor", async () => {
    let nowMs = 0;
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowMs,
      fetch: mockFetch(async () => { fetches++;
      if (fetches === 1) return jwksResponse(keyA);
      if (fetches === 2) return new Response("down", { status: 503 });
      return jwksResponse(keyB); }),
    });
    await verifier.verify(requestWithToken(await signAccessToken(keyA, validAccessClaims(0))));
    const tokenB = await signAccessToken(keyB, validAccessClaims(60));

    nowMs = 60_000;
    await expect(verifier.verify(requestWithToken(tokenB))).rejects.toThrow("access JWKS fetch failed");
    nowMs = 119_999;
    await expect(verifier.verify(requestWithToken(tokenB))).rejects.toThrow(`unknown access key ${keyB.kid}`);
    expect(fetches).toBe(2);
    nowMs = 120_000;
    await expect(verifier.verify(requestWithToken(tokenB))).resolves.toMatchObject({ sub: "access-user-1" });
    expect(fetches).toBe(3);
  });

  test("a successful refresh removes keys absent from the new set", async () => {
    let nowMs = 0;
    let fetches = 0;
    const verifier = makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
      now: () => nowMs,
      fetch: mockFetch(async () => { fetches++;
      return fetches === 1 ? jwksResponse(keyA) : jwksResponse(keyB); }),
    });
    const tokenA = await signAccessToken(keyA, validAccessClaims(0));
    await verifier.verify(requestWithToken(tokenA));
    nowMs = 60_000;
    await verifier.verify(requestWithToken(await signAccessToken(keyB, validAccessClaims(60))));
    await expect(verifier.verify(requestWithToken(tokenA)))
      .rejects.toThrow(`unknown access key ${keyA.kid}`);
  });
});
