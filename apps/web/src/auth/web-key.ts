// Ed25519 keypair via WebCrypto. Persisted in IndexedDB under "roost-web-key".
// Signs JWTs for coord auth: sub="web", aud="roost-coordinator".
// Called by trpc.ts authHeader(). R4.3 deliverable.

import { signal, diag } from "@roost/shared/diag";

const DB_NAME = "roost-auth";
const STORE_NAME = "keys";
const KEY_ID = "ed25519";
// Best-effort eviction breadcrumb. Set once a key is persisted; if it is
// still set on a later load but the IDB key is GONE, the key was evicted
// (iOS Safari ITP wipes script storage after ~7d idle → forced re-pair =
// the "lots of logins" pain). Caveat: ITP clears localStorage too, so a
// full eviction also drops this flag → we UNDER-report (the reliable
// signal is server-side `auth.relogin_401` in sync.ts). Stays quiet on
// genuine first boot; never false-positives. See [[project_todo_macos_tcc_blessing]].
const KEY_MINTED_FLAG = "roostKeyMinted";
function hadKeyBefore(): boolean {
  try { return localStorage.getItem(KEY_MINTED_FLAG) === "1"; } catch { return false; }
}
function markKeyMinted(): void {
  try { localStorage.setItem(KEY_MINTED_FLAG, "1"); } catch { /* private mode / sandboxed */ }
}

let _cachedKeyPair: CryptoKeyPair | null = null;
let _cachedKidHex: string | null = null;
let _cachedPubKeyB64: string | null = null;

async function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE_NAME);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

// Concurrent-call dedup. bootstrapSync() fires the coord-health poller
// AND _bootstrap() in the same tick; both await loadOrGenerate() before
// _cachedKeyPair is set. Without an in-flight Promise lock, each call
// IDB-read-misses and generate()s a fresh keypair. Whoever IDB-writes
// last wins the persisted slot, but the EARLIER caller has already
// resolved with a different keypair — its kid/pubkey mismatch the
// signing key, every JWT it mints fails coord verification (401).
let _inFlight: Promise<CryptoKeyPair> | null = null;

async function loadOrGenerate(): Promise<CryptoKeyPair> {
  if (_cachedKeyPair) return _cachedKeyPair;
  if (_inFlight) return _inFlight;
  _inFlight = (async () => {
    const db = await openDb();

    // Try load.
    const existing = await new Promise<CryptoKeyPair | null>((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readonly");
      const req = tx.objectStore(STORE_NAME).get(KEY_ID);
      req.onsuccess = () => resolve(req.result ?? null);
      req.onerror = () => reject(req.error);
    });

    if (existing) {
      _cachedKeyPair = existing;
      return existing;
    }

    // No key in IDB → mint fresh. If we'd minted before (breadcrumb still
    // set), the prior key was evicted and coord won't know this new kid →
    // re-pair. Signal it (best-effort) so `roost doctor` surfaces the churn.
    if (hadKeyBefore()) signal("auth.key_evicted", { db: DB_NAME });
    else diag("auth.key_first_boot", {});

    // Defensive: derived caches MUST track the keypair. Clear them before a
    // fresh mint so a stale kid/pubkey from any prior keypair can never sign
    // against the new private key (the stale-kid 401 class).
    _cachedKidHex = null;
    _cachedPubKeyB64 = null;
    _cachedJwt = null;

    // Generate fresh ed25519 keypair.
    const kp = await crypto.subtle.generateKey(
      { name: "Ed25519" },
      true,
      ["sign", "verify"],
    );

    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const req = tx.objectStore(STORE_NAME).put(kp, KEY_ID);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });

    markKeyMinted();
    _cachedKeyPair = kp;
    return kp;
  })();
  try {
    return await _inFlight;
  } finally {
    // Keep the cache; release the lock only after the cache is set so
    // any caller queued behind us reads the same result.
    _inFlight = null;
  }
}

export async function getPublicKeyB64(): Promise<string> {
  if (_cachedPubKeyB64) return _cachedPubKeyB64;
  const kp = await loadOrGenerate();
  const raw = await crypto.subtle.exportKey("raw", kp.publicKey);
  _cachedPubKeyB64 = btoa(String.fromCharCode(...new Uint8Array(raw)));
  return _cachedPubKeyB64;
}

// kid = lowercase hex SHA-256 of the raw 32-byte ed25519 pubkey, matching
// coord/src/jwt.ts::fingerprintOf. Stored in `authorized_keys.fingerprint`.
// Memoized — pubkey is stable for the lifetime of the IDB keypair.
async function getKidHex(): Promise<string> {
  if (_cachedKidHex) return _cachedKidHex;
  const kp = await loadOrGenerate();
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", kp.publicKey));
  const digest = await crypto.subtle.digest("SHA-256", raw);
  _cachedKidHex = Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return _cachedKidHex;
}

// Token cache: the payload is {sub:"web", aud, iat} only — no per-RPC claims —
// and coord accepts iat + jwtMaxAgeSecs (default 300, shared/config.ts) >= now.
// Reuse one token for 240 s (80% of the default max-age) instead of a fresh
// Ed25519 sign per RPC — input-channel signs one per KEYSTROKE otherwise. No
// invalidation edge: the keypair never rotates within a page life (IDB-cached,
// see loadOrGenerate).
const JWT_CACHE_TTL_MS = 240_000;
let _cachedJwt: { token: string; iatMs: number } | null = null;

// Mint a JWT: header.payload.signature (base64url, no library dependency).
export async function signCoordinatorJwt(): Promise<string> {
  if (_cachedJwt && Date.now() - _cachedJwt.iatMs < JWT_CACHE_TTL_MS) return _cachedJwt.token;
  const kp = await loadOrGenerate();
  const kid = await getKidHex();
  const now = Math.floor(Date.now() / 1000);

  const header = b64url(JSON.stringify({ alg: "EdDSA", typ: "JWT", kid }));
  const payload = b64url(
    JSON.stringify({ sub: "web", aud: "roost-coordinator", iat: now }),
  );

  const msg = new TextEncoder().encode(`${header}.${payload}`);
  const sig = await crypto.subtle.sign("Ed25519", kp.privateKey, msg);
  const sigB64 = b64url(sig);

  const token = `${header}.${payload}.${sigB64}`;
  _cachedJwt = { token, iatMs: now * 1000 };
  return token;
}

function b64url(input: string | ArrayBuffer): string {
  let bytes: Uint8Array;
  if (typeof input === "string") {
    bytes = new TextEncoder().encode(input);
  } else {
    bytes = new Uint8Array(input);
  }
  const bin = Array.from(bytes)
    .map((b) => String.fromCharCode(b))
    .join("");
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
}
