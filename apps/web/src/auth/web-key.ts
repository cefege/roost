// This stable entry owns the browser Ed25519 identity lifecycle and public auth API.
// Auth bootstrap and device settings call it to sign, rotate, or reset.
// It delegates atomic IndexedDB transactions so staged keys survive interrupted rotation.
// WebCrypto supplies non-extractable keys, while coordinator probes decide safe recovery.

import { diag, signal } from "@roost/shared/diag";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { WebKeyRotationStage } from "./web-key-storage.ts";
import {
  addCurrentWebKey,
  addWebKeyRotationStage,
  deleteCurrentWebKey,
  deleteMatchingWebKeyRotationStage,
  openWebKeyDatabase,
  promoteWebKeyRotationStage,
  readCurrentWebKey,
  readWebKeyRotationStage,
  WEB_KEY_DB_NAME,
} from "./web-key-storage.ts";

const KEY_LOCK = "roost-web-key-v1";
const KEY_MINTED_FLAG = "roostKeyMinted";
const JWT_LIFETIME_SECS = 300;
const JWT_CACHE_TTL_MS = 240_000;
const DEVICES_LIST_PATH = "/roost.v1.CoordinatorService/DevicesList";


type WebKeyProbeResult = "authorized" | "device-rejected" | "ambiguous";
type RecoveryResult = "none" | "promoted" | "discarded" | "ambiguous";

let _cachedKeyPair: CryptoKeyPair | null = null;
let _cachedKidHex: string | null = null;
let _cachedPubKeyB64: string | null = null;
let _cachedJwt: { token: string; iatMs: number } | null = null;
let _cacheGeneration = 0;

let _inFlight: Promise<CryptoKeyPair> | null = null;

function hadKeyBefore(): boolean {
  try { return localStorage.getItem(KEY_MINTED_FLAG) === "1"; } catch { return false; }
}


function markKeyMinted(): void {
  try { localStorage.setItem(KEY_MINTED_FLAG, "1"); } catch { /* private mode */ }
}


function clearCaches(): void {
  _cacheGeneration++;
  _cachedKeyPair = null;
  _cachedKidHex = null;
  _cachedPubKeyB64 = null;
  _cachedJwt = null;
  _inFlight = null;
}

const keyChangeChannel = (() => {
  if (typeof BroadcastChannel === "undefined") return null;
  try {
    return new BroadcastChannel(KEY_LOCK);
  } catch {
    return null;
  }
})();

export function _announceWebKeyChange(): void {
  clearCaches();
  try {
    // A BroadcastChannel never delivers to the object that posted the message.
    // Reusing the listener keeps initiating-tab credentials alive while peers
    // still discard stale keys.
    keyChangeChannel?.postMessage("changed");
  } catch { /* BroadcastChannel unavailable */ }
}

if (keyChangeChannel) {
  keyChangeChannel.onmessage = () => {
    clearCaches();
    if (typeof location !== "undefined" && typeof location.reload === "function") {
      location.reload();
    }
  };
}

function reloadAfterKeyChange(): void {
  markKeyMinted();
  _announceWebKeyChange();
  if (typeof location !== "undefined" && typeof location.reload === "function") location.reload();
}

async function withKeyLock<T>(destructive: boolean, action: () => Promise<T>): Promise<T> {
  if (typeof navigator !== "undefined" && navigator.locks) {
    return navigator.locks.request(KEY_LOCK, action);
  }
  if (destructive) throw new Error("Web Locks is required to rotate or reset this device key");
  return action();
}

async function generateKeyPair(): Promise<CryptoKeyPair> {
  return crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
}

async function fingerprintFor(pair: CryptoKeyPair): Promise<string> {
  return fingerprintOf(new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey)));
}

async function publicKeyB64For(pair: CryptoKeyPair): Promise<string> {
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return btoa(Array.from(raw, (byte) => String.fromCharCode(byte)).join(""));
}

export async function signCoordinatorJwtWithKeyPair(pair: CryptoKeyPair): Promise<string> {
  const kid = await fingerprintFor(pair);
  const now = Math.floor(Date.now() / 1000);
  const header = b64url(JSON.stringify({ alg: "EdDSA", typ: "JWT", kid }));
  const payload = b64url(JSON.stringify({
    sub: kid,
    aud: "roost-coordinator",
    iat: now,
    exp: now + JWT_LIFETIME_SECS,
  }));
  const message = new TextEncoder().encode(`${header}.${payload}`);
  const signature = await crypto.subtle.sign("Ed25519", pair.privateKey, message);
  return `${header}.${payload}.${b64url(signature)}`;
}

async function probePair(pair: CryptoKeyPair): Promise<WebKeyProbeResult> {
  try {
    const { makeCoordinatorClientForSigner } = await import("../connect.ts");
    const client = makeCoordinatorClientForSigner(() => signCoordinatorJwtWithKeyPair(pair));
    await client.devicesList({});
    return "authorized";
  } catch (error) {
    const { classifyAuthFailure } = await import("../connect.ts");
    return classifyAuthFailure(error, DEVICES_LIST_PATH) === "device"
      ? "device-rejected"
      : "ambiguous";
  }
}

async function recoverStageLocked(db: IDBDatabase): Promise<RecoveryResult> {
  const stage = await readWebKeyRotationStage(db);
  if (!stage) return "none";
  const staged = await probePair(stage.keyPair);
  if (staged === "authorized") {
    await promoteWebKeyRotationStage(db, stage);
    clearCaches();
    return "promoted";
  }
  if (staged !== "device-rejected") return "ambiguous";
  const current = await readCurrentWebKey(db);
  if (!current) return "ambiguous";
  const old = await probePair(current);
  if (old !== "authorized") return "ambiguous";
  await deleteMatchingWebKeyRotationStage(db, stage.operationId);
  return "discarded";
}

async function loadOrGenerateLocked(): Promise<CryptoKeyPair> {
  const db = await openWebKeyDatabase();
  const recovery = await recoverStageLocked(db);
  if (recovery === "promoted") {
    const promoted = await readCurrentWebKey(db);
    if (!promoted) throw new Error("Promoted device key is missing");
    reloadAfterKeyChange();
    return promoted;
  }
  const existing = await readCurrentWebKey(db);
  if (existing) return existing;

  if (hadKeyBefore()) signal("auth.key_evicted", { db: WEB_KEY_DB_NAME });
  else diag("auth.key_first_boot", {});
  const generated = await generateKeyPair();
  try {
    await addCurrentWebKey(db, generated);
    markKeyMinted();
    return generated;
  } catch (error) {
    if (!(error instanceof DOMException) || error.name !== "ConstraintError") throw error;
    const winner = await readCurrentWebKey(db);
    if (!winner) throw error;
    return winner;
  }
}

async function loadOrGenerate(): Promise<CryptoKeyPair> {
  if (_cachedKeyPair) return _cachedKeyPair;
  if (_inFlight) return _inFlight;
  const cacheGeneration = _cacheGeneration;
  const inFlight = withKeyLock(false, loadOrGenerateLocked);
  _inFlight = inFlight;
  try {
    const pair = await inFlight;
    if (cacheGeneration === _cacheGeneration) _cachedKeyPair = pair;
    return pair;
  } finally {
    if (_inFlight === inFlight) _inFlight = null;
  }
}

export async function getPublicKeyB64(): Promise<string> {
  if (_cachedPubKeyB64) return _cachedPubKeyB64;
  const cacheGeneration = _cacheGeneration;
  const pair = await loadOrGenerate();
  const publicKeyB64 = await publicKeyB64For(pair);
  if (cacheGeneration === _cacheGeneration) _cachedPubKeyB64 = publicKeyB64;
  return publicKeyB64;
}

export async function getCurrentWebKeyInfo(): Promise<{ fingerprint: string; extractable: boolean }> {
  const pair = await loadOrGenerate();
  return { fingerprint: await fingerprintFor(pair), extractable: pair.privateKey.extractable };
}

export async function signCoordinatorJwt(): Promise<string> {
  const now = Date.now();
  const age = _cachedJwt ? now - _cachedJwt.iatMs : Number.POSITIVE_INFINITY;
  if (_cachedJwt && age >= 0 && age < JWT_CACHE_TTL_MS) return _cachedJwt.token;
  const cacheGeneration = _cacheGeneration;
  const pair = await loadOrGenerate();
  const token = await signCoordinatorJwtWithKeyPair(pair);
  const kid = await fingerprintFor(pair);
  if (cacheGeneration === _cacheGeneration) {
    _cachedKidHex = kid;
    _cachedJwt = { token, iatMs: Date.now() };
  }
  return token;
}

export async function rotateCurrentWebKey(label: string): Promise<void> {
  await withKeyLock(true, async () => {
    const db = await openWebKeyDatabase();
    const recovery = await recoverStageLocked(db);
    if (recovery === "ambiguous") {
      throw new Error("A prior key rotation is still ambiguous; retry when the coordinator is reachable");
    }
    if (recovery === "promoted") {
      reloadAfterKeyChange();
      return;
    }
    const current = await readCurrentWebKey(db);
    if (!current) throw new Error("Current device key is missing");
    const stage: WebKeyRotationStage = {
      operationId: crypto.randomUUID(),
      keyPair: await generateKeyPair(),
    };
    await addWebKeyRotationStage(db, stage);
    const { makeCoordinatorClientForSigner } = await import("../connect.ts");
    const client = makeCoordinatorClientForSigner(() => signCoordinatorJwtWithKeyPair(current));
    await client.devicesRotateCurrent({
      sshPubkeyB64: await publicKeyB64For(stage.keyPair),
      label,
    });
    await promoteWebKeyRotationStage(db, stage);
    reloadAfterKeyChange();
  });
}

export async function isResetWebKeyEligible(): Promise<boolean> {
  return withKeyLock(false, async () => probePair(await loadOrGenerateLocked())
    .then((result) => result === "device-rejected"));
}

export async function resetWebKey(): Promise<void> {
  await withKeyLock(true, async () => {
    const db = await openWebKeyDatabase();
    const recovery = await recoverStageLocked(db);
    if (recovery === "ambiguous") {
      throw new Error("Key state is ambiguous; reset refused until the coordinator is reachable");
    }
    if (recovery === "promoted") {
      reloadAfterKeyChange();
      return;
    }
    const current = await readCurrentWebKey(db);
    if (!current) return;
    if (await probePair(current) !== "device-rejected") {
      throw new Error("Reset is allowed only after this device key is explicitly rejected");
    }
    await deleteCurrentWebKey(db);
    _announceWebKeyChange();
    if (typeof location !== "undefined" && typeof location.reload === "function") location.reload();
  });
}


function b64url(input: string | ArrayBuffer): string {
  const bytes = typeof input === "string"
    ? new TextEncoder().encode(input)
    : new Uint8Array(input);
  return btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join(""))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=/g, "");
}
