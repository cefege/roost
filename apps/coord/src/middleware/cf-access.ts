const TOKEN_MAX_BYTES = 16 * 1024;
const JWKS_TTL_MS = 10 * 60_000;
const UNKNOWN_KID_REFRESH_FLOOR_MS = 60_000;
const JWKS_TIMEOUT_MS = 5_000;

export interface CfAccessIdentity {
  sub: string;
  email: string;
  exp: number;
}

export interface CfAccessVerifier {
  verify(req: Request): Promise<CfAccessIdentity>;
}

export interface CfAccessVerifierOptions {
  fetch?: typeof globalThis.fetch;
  now?: () => number;
}

interface AccessHeader {
  alg?: unknown;
  kid?: unknown;
}
interface AccessJwk extends JsonWebKey {
  alg: string;
  kid: string;
  use: string;
}


interface AccessPayload {
  iss?: unknown;
  aud?: unknown;
  exp?: unknown;
  iat?: unknown;
  nbf?: unknown;
  type?: unknown;
  sub?: unknown;
  email?: unknown;
  common_name?: unknown;
}

function tokenFromRequest(req: Request): string {
  const assertion = req.headers.get("cf-access-jwt-assertion");
  if (assertion) return assertion;

  const cookie = req.headers.get("cookie");
  if (cookie) {
    for (const part of cookie.split(";")) {
      const trimmed = part.trim();
      const equals = trimmed.indexOf("=");
      if (equals > 0 && trimmed.slice(0, equals) === "CF_Authorization") {
        const value = trimmed.slice(equals + 1);
        if (value) return value;
      }
    }
  }
  throw new Error("no access token");
}

function parseJsonPart<T>(encoded: string, label: string): T {
  try {
    return JSON.parse(Buffer.from(encoded, "base64url").toString("utf8")) as T;
  } catch {
    throw new Error(`bad access token ${label}`);
  }
}

function signatureBytes(encoded: string): Uint8Array {
  try {
    return new Uint8Array(Buffer.from(encoded, "base64url"));
  } catch {
    throw new Error("bad access token signature");
  }
}

export function makeCfAccessVerifier(
  teamDomain: string,
  aud: string,
  opts: CfAccessVerifierOptions = {},
): CfAccessVerifier {
  const fetchImpl = opts.fetch ?? globalThis.fetch;
  const now = opts.now ?? Date.now;
  const issuer = `https://${teamDomain}`;

  let keys = new Map<string, CryptoKey>();
  let lastSuccessfulRefreshMs: number | null = null;
  let lastRefreshAttemptMs: number | null = null;
  let refreshInFlight: Promise<void> | null = null;

  async function refreshKeys(): Promise<void> {
    if (refreshInFlight) return refreshInFlight;

    lastRefreshAttemptMs = now();
    const pending = (async () => {
      const response = await fetchImpl(`https://${teamDomain}/cdn-cgi/access/certs`, {
        method: "GET",
        signal: AbortSignal.timeout(JWKS_TIMEOUT_MS),
      });
      if (!response.ok) throw new Error(`access JWKS fetch failed: ${response.status}`);

      const document = await response.json() as { keys?: unknown };
      if (!Array.isArray(document.keys)) throw new Error("access JWKS missing keys");

      const imported = await Promise.all(document.keys.map(async (candidate) => {
        if (!candidate || typeof candidate !== "object") return null;
        const key = candidate as AccessJwk;
        if (
          typeof key.kid !== "string"
          || key.kty !== "RSA"
          || key.alg !== "RS256"
          || key.use !== "sig"
        ) return null;
        const cryptoKey = await crypto.subtle.importKey(
          "jwk",
          key,
          { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
          false,
          ["verify"],
        );
        return [key.kid, cryptoKey] as const;
      }));

      const next = new Map<string, CryptoKey>();
      for (const entry of imported) {
        if (entry) next.set(entry[0], entry[1]);
      }
      keys = next;
      lastSuccessfulRefreshMs = now();
    })();

    refreshInFlight = pending.finally(() => {
      refreshInFlight = null;
    });
    return refreshInFlight;
  }

  async function keyFor(kid: string): Promise<CryptoKey> {
    const at = now();
    const live = lastSuccessfulRefreshMs !== null
      && at - lastSuccessfulRefreshMs < JWKS_TTL_MS;
    const known = keys.get(kid);
    if (known && live) return known;

    if (refreshInFlight) {
      try {
        await refreshInFlight;
      } catch (error) {
        if (known && live) return known;
        throw error;
      }
    } else {
      const unknownRefreshFloored = !known
        && lastRefreshAttemptMs !== null
        && at - lastRefreshAttemptMs < UNKNOWN_KID_REFRESH_FLOOR_MS;
      if (unknownRefreshFloored) throw new Error(`unknown access key ${kid}`);
      try {
        await refreshKeys();
      } catch (error) {
        if (known && live) return known;
        throw error;
      }
    }

    const refreshed = keys.get(kid);
    if (!refreshed) throw new Error(`unknown access key ${kid}`);
    return refreshed;
  }

  async function verify(req: Request): Promise<CfAccessIdentity> {
    const token = tokenFromRequest(req);
    if (Buffer.byteLength(token) > TOKEN_MAX_BYTES) throw new Error("access token too long");

    const parts = token.split(".");
    if (parts.length !== 3) throw new Error("bad access token format");
    const [headerB64, payloadB64, signatureB64] = parts as [string, string, string];
    const header = parseJsonPart<AccessHeader>(headerB64, "header");
    if (header.alg !== "RS256") throw new Error("wrong access token alg");
    if (typeof header.kid !== "string" || !header.kid) throw new Error("missing access token kid");

    const key = await keyFor(header.kid);
    const message = new TextEncoder().encode(`${headerB64}.${payloadB64}`);
    const signature = signatureBytes(signatureB64);
    const valid = await crypto.subtle.verify(
      { name: "RSASSA-PKCS1-v1_5" },
      key,
      signature.buffer.slice(
        signature.byteOffset,
        signature.byteOffset + signature.byteLength,
      ) as ArrayBuffer,
      message.buffer.slice(
        message.byteOffset,
        message.byteOffset + message.byteLength,
      ) as ArrayBuffer,
    );
    if (!valid) throw new Error("access token signature invalid");

    const payload = parseJsonPart<AccessPayload>(payloadB64, "payload");
    const nowSecs = Math.floor(now() / 1000);
    if (payload.iss !== issuer) throw new Error("wrong access token issuer");
    if (!Array.isArray(payload.aud) || !payload.aud.includes(aud)) {
      throw new Error("wrong access token audience");
    }
    if (typeof payload.exp !== "number" || !Number.isFinite(payload.exp)) {
      throw new Error("invalid access token exp");
    }
    if (payload.exp <= nowSecs) throw new Error("access token expired");
    if (typeof payload.iat !== "number" || !Number.isFinite(payload.iat)) {
      throw new Error("invalid access token iat");
    }
    if (payload.iat > nowSecs + 30) throw new Error("access token iat in future");
    if (payload.nbf !== undefined) {
      if (typeof payload.nbf !== "number" || !Number.isFinite(payload.nbf)) {
        throw new Error("invalid access token nbf");
      }
      if (payload.nbf > nowSecs + 30) throw new Error("access token nbf in future");
    }
    if (payload.type !== "app") throw new Error("wrong access token type");
    if (typeof payload.sub !== "string" || !payload.sub) {
      throw new Error("missing access token subject");
    }

    return {
      sub: payload.sub,
      email: typeof payload.email === "string"
        ? payload.email
        : (typeof payload.common_name === "string" ? payload.common_name : ""),
      exp: payload.exp,
    };
  }

  return { verify };
}
