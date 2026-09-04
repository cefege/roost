import { describe, expect, test } from "bun:test";
import { generateKeyPairSync, randomBytes, type KeyObject } from "node:crypto";
import { SignJWT, createLocalJWKSet, type JSONWebKeySet } from "jose";
import { verifyGoogleIdToken } from "../src/saas-auth/google-id-token.ts";

const CLIENT_ID = "roost-client.apps.googleusercontent.com";
const NONCE = "fixture-nonce";
const NOW = Math.floor(Date.now() / 1_000);

function fixture() {
  const pair = generateKeyPairSync("rsa", { modulusLength: 2_048 });
  const publicJwk = pair.publicKey.export({ format: "jwk" });
  const jwks = createLocalJWKSet({ keys: [{ ...publicJwk, kid: "fixture", alg: "RS256", use: "sig" }] } as JSONWebKeySet);
  return { ...pair, jwks };
}

async function token(
  privateKey: KeyObject,
  overrides: Record<string, unknown> = {},
  header: { alg: string; kid?: string } = { alg: "RS256", kid: "fixture" },
): Promise<string> {
  const claims: Record<string, unknown> = {
    nonce: NONCE,
    email: "Owner@Example.Test",
    email_verified: true,
    azp: CLIENT_ID,
    ...overrides,
  };
  const jwt = new SignJWT(claims).setProtectedHeader(header).setIssuer(String(overrides.iss ?? "https://accounts.google.com"));
  if (!("aud" in overrides)) jwt.setAudience(CLIENT_ID);
  else jwt.setAudience(String(overrides.aud));
  if (!("sub" in overrides)) jwt.setSubject("google-subject-1");
  else if (overrides.sub !== undefined) jwt.setSubject(String(overrides.sub));
  jwt.setIssuedAt(NOW);
  if (!("exp" in overrides)) jwt.setExpirationTime(NOW + 300);
  else jwt.setExpirationTime(Number(overrides.exp));
  return jwt.sign(privateKey);
}

describe("Google ID token fixture", () => {
  test("accepts both Google issuer spellings and canonicalizes verified identity", async () => {
    const keys = fixture();
    for (const issuer of ["https://accounts.google.com", "accounts.google.com"]) {
      const idToken = await token(keys.privateKey, { iss: issuer });
      await expect(verifyGoogleIdToken(idToken, {
        clientId: CLIENT_ID,
        expectedNonce: NONCE,
        jwks: keys.jwks,
      })).resolves.toEqual({
        issuer: "https://accounts.google.com",
        subject: "google-subject-1",
        emailNormalized: "owner@example.test",
      });
    }
  });

  test("rejects wrong audience, azp, nonce, issuer, expiry, and unverified email", async () => {
    const keys = fixture();
    const cases: Array<[Record<string, unknown>, string]> = [
      [{ aud: "another-client" }, NONCE],
      [{ azp: "another-client" }, NONCE],
      [{ nonce: "another-nonce" }, NONCE],
      [{ iss: "https://issuer.invalid" }, NONCE],
      [{ exp: NOW - 1 }, NONCE],
      [{ email_verified: false }, NONCE],
    ];
    for (const [overrides, expectedNonce] of cases) {
      const idToken = await token(keys.privateKey, overrides);
      await expect(verifyGoogleIdToken(idToken, {
        clientId: CLIENT_ID,
        expectedNonce,
        jwks: keys.jwks,
      })).rejects.toBeDefined();
    }
  });

  test("rejects wrong signature, kid, and algorithm", async () => {
    const trusted = fixture();
    const attacker = fixture();
    await expect(verifyGoogleIdToken(await token(attacker.privateKey), {
      clientId: CLIENT_ID,
      expectedNonce: NONCE,
      jwks: trusted.jwks,
    })).rejects.toBeDefined();
    await expect(verifyGoogleIdToken(await token(trusted.privateKey, {}, { alg: "RS256", kid: "unknown" }), {
      clientId: CLIENT_ID,
      expectedNonce: NONCE,
      jwks: trusted.jwks,
    })).rejects.toBeDefined();
    const symmetric = randomBytes(32);
    const wrongAlgorithm = await new SignJWT({
      nonce: NONCE,
      email: "owner@example.test",
      email_verified: true,
    })
      .setProtectedHeader({ alg: "HS256" })
      .setIssuer("https://accounts.google.com")
      .setAudience(CLIENT_ID)
      .setSubject("google-subject-1")
      .setExpirationTime(NOW + 300)
      .sign(symmetric);
    await expect(verifyGoogleIdToken(wrongAlgorithm, {
      clientId: CLIENT_ID,
      expectedNonce: NONCE,
      jwks: trusted.jwks,
    })).rejects.toBeDefined();
  });
});
