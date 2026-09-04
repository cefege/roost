/**
 * Verifies Google ID tokens and returns the normalized identity trusted by signup flows.
 * The OAuth protocol supplies the expected audience, nonce, and remote key resolver.
 * Central claim checks prevent unverified email or issuer variants entering provisioning state.
 */

import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  createRemoteJWKSet,
  jwtVerify,
  type JWTVerifyGetKey,
} from "jose";

export const GOOGLE_IDENTITY_ISSUER = "https://accounts.google.com" as const;
const GOOGLE_TOKEN_ISSUERS = [GOOGLE_IDENTITY_ISSUER, "accounts.google.com"] as const;
const GOOGLE_SUBJECT_MAX_BYTES = 255;

const googleJwks = createRemoteJWKSet(
  new URL("https://www.googleapis.com/oauth2/v3/certs"),
  {
    timeoutDuration: 5_000,
    cooldownDuration: 30_000,
    cacheMaxAge: 600_000,
  },
);

export interface GoogleIdentityClaims {
  issuer: typeof GOOGLE_IDENTITY_ISSUER;
  subject: string;
  emailNormalized: string;
}

export interface VerifyGoogleIdTokenOptions {
  clientId: string;
  expectedNonce: string;
  /** Test seam for a deterministic local JWKS fixture. Production callers omit it. */
  jwks?: JWTVerifyGetKey;
}

export async function verifyGoogleIdToken(
  idToken: string,
  options: VerifyGoogleIdTokenOptions,
): Promise<GoogleIdentityClaims> {
  const clientId = options.clientId.trim();
  if (!clientId) throw new Error("Google OIDC client ID is required");
  if (!options.expectedNonce) throw new Error("Google OIDC nonce is required");

  const { payload } = await jwtVerify(idToken, options.jwks ?? googleJwks, {
    algorithms: ["RS256"],
    issuer: [...GOOGLE_TOKEN_ISSUERS],
    audience: clientId,
    requiredClaims: ["exp", "sub", "nonce", "email", "email_verified"],
  });

  if (payload.nonce !== options.expectedNonce) {
    throw new Error("Google ID token nonce mismatch");
  }
  const subject = payload.sub;
  if (
    typeof subject !== "string"
    || subject.length === 0
    || Buffer.byteLength(subject, "utf8") > GOOGLE_SUBJECT_MAX_BYTES
  ) {
    throw new Error("Google ID token subject is invalid");
  }
  if (payload.email_verified !== true || typeof payload.email !== "string") {
    throw new Error("Google ID token email is not verified");
  }
  const emailNormalized = normalizeAccountEmail(payload.email);
  if (!emailNormalized) throw new Error("Google ID token email is invalid");
  if (payload.azp !== undefined && payload.azp !== clientId) {
    throw new Error("Google ID token authorized party mismatch");
  }

  return {
    issuer: GOOGLE_IDENTITY_ISSUER,
    subject,
    emailNormalized,
  };
}
