export const ACCESS_TEAM_DOMAIN = "example.cloudflareaccess.com";
export const ACCESS_AUD = "a".repeat(64);
export interface AccessPublicJwk extends JsonWebKey {
  alg: string;
  kid: string;
  use: string;
}


export interface AccessSigningKey {
  kid: string;
  privateKey: CryptoKey;
  jwk: AccessPublicJwk;
}

export async function generateAccessSigningKey(kid: string): Promise<AccessSigningKey> {
  const pair = await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  );
  const jwk = await crypto.subtle.exportKey("jwk", pair.publicKey);
  return {
    kid,
    privateKey: pair.privateKey,
    jwk: { ...jwk, kid, alg: "RS256", use: "sig" },
  };
}

export function validAccessClaims(nowSecs: number): Record<string, unknown> {
  return {
    iss: `https://${ACCESS_TEAM_DOMAIN}`,
    aud: [ACCESS_AUD],
    exp: nowSecs + 300,
    iat: nowSecs,
    type: "app",
    sub: "access-user-1",
    email: "user@example.com",
  };
}

export async function signAccessToken(
  key: AccessSigningKey,
  claims: Record<string, unknown>,
  header: Record<string, unknown> = { alg: "RS256", kid: key.kid },
): Promise<string> {
  const headerB64 = Buffer.from(JSON.stringify(header)).toString("base64url");
  const payloadB64 = Buffer.from(JSON.stringify(claims)).toString("base64url");
  const message = new TextEncoder().encode(`${headerB64}.${payloadB64}`);
  const signature = await crypto.subtle.sign(
    { name: "RSASSA-PKCS1-v1_5" },
    key.privateKey,
    message,
  );
  return `${headerB64}.${payloadB64}.${Buffer.from(signature).toString("base64url")}`;
}

export function jwksResponse(...keys: AccessSigningKey[]): Response {
  return Response.json({ keys: keys.map((key) => key.jwk) });
}

export function staticJwksFetch(...keys: AccessSigningKey[]): typeof fetch {
  return Object.assign(
    async () => jwksResponse(...keys),
    { preconnect(_url: string | URL): void {} },
  );
}
