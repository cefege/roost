// EdDSA JWT verify + sign using Bun's WebCrypto (crypto.subtle).
// Matches legacy lib/jwt.ts shape: 60s pubkey cache + DB fallback.
// kid = lowercase hex SHA-256 of raw 32-byte ed25519 pubkey.
// aud = "roost-coordinator" for inbound JWTs; "worker-direct" for minted ones.
// R0.2, R1.1 security baseline preserved.

import type { KyselyDB } from "./db/connection.ts";
import { log } from "@roost/shared/log";

const CACHE_TTL_MS = 60_000;
const AUDIENCE = "roost-coordinator";

// ─── raw pubkey → CryptoKey ────────────────────────────────────────────

const SPKI_PREFIX = new Uint8Array([
  0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
]);

function rawToSpki(raw: Uint8Array): Uint8Array {
  const out = new Uint8Array(SPKI_PREFIX.length + 32);
  out.set(SPKI_PREFIX);
  out.set(raw.subarray(0, 32), SPKI_PREFIX.length);
  return out;
}

async function importEd25519Pubkey(raw: Uint8Array): Promise<CryptoKey> {
  const spki = rawToSpki(raw);
  return crypto.subtle.importKey(
    "spki",
    spki.buffer.slice(spki.byteOffset, spki.byteOffset + spki.byteLength) as ArrayBuffer,
    { name: "Ed25519" },
    true,
    ["verify"],
  );
}

export async function importEd25519PrivkeyPkcs8(pkcs8Der: Uint8Array): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "pkcs8",
    pkcs8Der.buffer.slice(pkcs8Der.byteOffset, pkcs8Der.byteOffset + pkcs8Der.byteLength) as ArrayBuffer,
    { name: "Ed25519" },
    true,
    ["sign"],
  );
}

// ─── base64url helpers ─────────────────────────────────────────────────

function b64urlEncode(buf: Uint8Array): string {
  return Buffer.from(buf).toString("base64url");
}

function b64urlDecode(s: string): Uint8Array {
  return new Uint8Array(Buffer.from(s, "base64url"));
}

// ─── pubkey cache ──────────────────────────────────────────────────────

interface CacheEntry {
  key: CryptoKey;
  label: string;
  insertedAt: number;
  generation: number;
}

export interface JwtCache {
  entries: Map<string, CacheEntry>;
  generations: Map<string, number>;
}

export function newJwtCache(): JwtCache {
  return { entries: new Map(), generations: new Map() };
}

export function jwtKeyGeneration(cache: JwtCache, fingerprint: string): number {
  return cache.generations.get(fingerprint) ?? 0;
}

export function invalidateJwtKey(cache: JwtCache, fingerprint: string): void {
  cache.generations.set(fingerprint, jwtKeyGeneration(cache, fingerprint) + 1);
  cache.entries.delete(fingerprint);
}

/** Drop a cached authorized-key snapshot without changing its revocation
 * generation. Authorization/upsert callers use this after commit so a
 * verifier that already loaded an authorized row remains valid. */
export function refreshJwtKey(cache: JwtCache, fingerprint: string): void {
  cache.entries.delete(fingerprint);
}

async function lookupKey(
  db: KyselyDB,
  cache: JwtCache,
  kid: string,
  importKey: (raw: Uint8Array) => Promise<CryptoKey> = importEd25519Pubkey,
): Promise<{ key: CryptoKey; label: string; generation: number }> {
  const generation = jwtKeyGeneration(cache, kid);
  const now = Date.now();
  const hit = cache.entries.get(kid);
  if (hit && now - hit.insertedAt < CACHE_TTL_MS && hit.generation === generation) {
    return { key: hit.key, label: hit.label, generation };
  }
  const row = await db
    .selectFrom("authorized_keys")
    .select(["public_key", "label"])
    .where("fingerprint", "=", kid)
    .executeTakeFirst();
  if (jwtKeyGeneration(cache, kid) !== generation) {
    throw Object.assign(new Error(`unknown kid ${kid}`), { status: 401 });
  }
  if (!row) throw Object.assign(new Error(`unknown kid ${kid}`), { status: 401 });

  const raw = row.public_key instanceof Uint8Array ? row.public_key : new Uint8Array(row.public_key);
  if (raw.length !== 32) throw Object.assign(new Error("stored pubkey wrong length"), { status: 401 });

  const key = await importKey(raw);
  if (jwtKeyGeneration(cache, kid) !== generation) {
    throw Object.assign(new Error(`unknown kid ${kid}`), { status: 401 });
  }
  cache.entries.set(kid, { key, label: row.label, insertedAt: now, generation });
  return { key, label: row.label, generation };
}

// ─── verify ────────────────────────────────────────────────────────────

export interface Caller {
  fingerprint: string;
  label: string;
  scopes?: string[];
  keyGeneration: number;
}

export interface VerifyOpts {
  db: KyselyDB;
  cache: JwtCache;
  jwtMaxAgeSecs: number;
  importKey?: (raw: Uint8Array) => Promise<CryptoKey>;
  verifySignature?: (key: CryptoKey, signature: Uint8Array, message: Uint8Array) => Promise<boolean>;
}

function b64urlToUtf8(s: string): string {
  return Buffer.from(s, "base64url").toString("utf8");
}

export async function verifyJwt(token: string, opts: VerifyOpts): Promise<Caller> {
  const parts = token.split(".");
  if (parts.length !== 3) throw Object.assign(new Error("bad jwt format"), { status: 401 });

  const [headerB64, payloadB64, sigB64] = parts as [string, string, string];

  let header: { kid?: string; alg?: string };
  try {
    header = JSON.parse(b64urlToUtf8(headerB64));
  } catch {
    throw Object.assign(new Error("bad jwt header"), { status: 401 });
  }

  if (header.alg !== "EdDSA") throw Object.assign(new Error("wrong alg"), { status: 401 });
  const kid = header.kid;
  if (!kid) throw Object.assign(new Error("missing kid"), { status: 401 });

  const { key, label, generation } = await lookupKey(
    opts.db,
    opts.cache,
    kid,
    opts.importKey,
  );

  const msg = new TextEncoder().encode(`${headerB64}.${payloadB64}`);
  const sig = b64urlDecode(sigB64);

  const ok = opts.verifySignature
    ? await opts.verifySignature(key, sig, msg)
    : await crypto.subtle.verify(
        { name: "Ed25519" },
        key,
        sig.buffer.slice(sig.byteOffset, sig.byteOffset + sig.byteLength) as ArrayBuffer,
        msg.buffer.slice(msg.byteOffset, msg.byteOffset + msg.byteLength) as ArrayBuffer,
      );
  if (jwtKeyGeneration(opts.cache, kid) !== generation) {
    throw Object.assign(new Error(`unknown kid ${kid}`), { status: 401 });
  }
  if (!ok) throw Object.assign(new Error("signature invalid"), { status: 401 });

  let payload: { iat?: number; aud?: string | string[]; sub?: string };
  try {
    payload = JSON.parse(b64urlToUtf8(payloadB64));
  } catch {
    throw Object.assign(new Error("bad jwt payload"), { status: 401 });
  }

  // aud check
  const aud = Array.isArray(payload.aud) ? payload.aud : [payload.aud];
  if (!aud.includes(AUDIENCE)) {
    throw Object.assign(new Error(`wrong aud: ${payload.aud}`), { status: 401 });
  }

  // iat freshness
  const nowSecs = Math.floor(Date.now() / 1000);
  if (typeof payload.iat !== "number") throw Object.assign(new Error("missing iat"), { status: 401 });
  if (payload.iat + opts.jwtMaxAgeSecs < nowSecs) {
    throw Object.assign(new Error(`token too old`), { status: 401 });
  }
  if (payload.iat > nowSecs + 30) {
    throw Object.assign(new Error("token from the future"), { status: 401 });
  }

  if (jwtKeyGeneration(opts.cache, kid) !== generation) {
    throw Object.assign(new Error(`unknown kid ${kid}`), { status: 401 });
  }
  log.debug("jwt", "verified", { trace_id: undefined, fp: kid, label });
  return { fingerprint: kid, label, keyGeneration: generation };
}

// ─── sign (for coord-minted worker-direct JWTs) ────────────────────────

export interface SignClaims {
  aud: string;
  sub: string;
  iat: number;
  exp?: number;
  [k: string]: unknown;
}

export async function signJwt(claims: SignClaims, key: CryptoKey, kid: string): Promise<string> {
  const header = b64urlEncode(new TextEncoder().encode(JSON.stringify({ alg: "EdDSA", typ: "JWT", kid })));
  const payload = b64urlEncode(new TextEncoder().encode(JSON.stringify(claims)));
  const msg = new TextEncoder().encode(`${header}.${payload}`);
  const sigBuf = await crypto.subtle.sign(
    { name: "Ed25519" },
    key,
    msg.buffer.slice(msg.byteOffset, msg.byteOffset + msg.byteLength) as ArrayBuffer,
  );
  const sig = b64urlEncode(new Uint8Array(sigBuf));
  return `${header}.${payload}.${sig}`;
}

// ─── fingerprint helper ────────────────────────────────────────────────

export async function fingerprintOf(raw: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength) as ArrayBuffer,
  );
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
