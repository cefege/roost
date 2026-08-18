// Browser Ed25519 identity. The private key is non-extractable and persisted by
// IndexedDB structured clone. Rotation keeps old and staged keys until the
// coordinator's commit state is unambiguous.

import { diag, signal } from "@roost/shared/diag";
import { fingerprintOf } from "@roost/shared/fingerprint";

const DB_NAME = "roost-auth";
const STORE_NAME = "keys";
const KEY_ID = "ed25519";
const ROTATION_ID = "ed25519-rotation-v1";
const KEY_LOCK = "roost-web-key-v1";
const KEY_MINTED_FLAG = "roostKeyMinted";
const JWT_CACHE_TTL_MS = 240_000;
const DEVICES_LIST_PATH = "/roost.v1.CoordinatorService/DevicesList";

interface RotationStage {
  operationId: string;
  keyPair: CryptoKeyPair;
}

type ProbeResult = "authorized" | "device-rejected" | "ambiguous";
type RecoveryResult = "none" | "promoted" | "discarded" | "ambiguous";

let _cachedKeyPair: CryptoKeyPair | null = null;
let _cachedKidHex: string | null = null;
let _cachedPubKeyB64: string | null = null;
let _cachedJwt: { token: string; iatMs: number } | null = null;

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
}

function promiseWithResolvers<T>(): Deferred<T> {
  if (typeof Promise.withResolvers === "function") return Promise.withResolvers<T>();
  let resolve!: Deferred<T>["resolve"];
  let reject!: Deferred<T>["reject"];
  // Compatibility path for Safari releases predating Promise.withResolvers.
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
let _inFlight: Promise<CryptoKeyPair> | null = null;

function hadKeyBefore(): boolean {
  try { return localStorage.getItem(KEY_MINTED_FLAG) === "1"; } catch { return false; }
}

// Snapshot before any eager startup consumer can mint a key. The live flag
// answers "has key now"; bootstrap needs "was this browser already enrolled?"
export const persistedWebKeyAtStartup = hadKeyBefore();

function markKeyMinted(): void {
  try { localStorage.setItem(KEY_MINTED_FLAG, "1"); } catch { /* private mode */ }
}

function clearCaches(): void {
  _cachedKeyPair = null;
  _cachedKidHex = null;
  _cachedPubKeyB64 = null;
  _cachedJwt = null;
  _inFlight = null;
}

function announceKeyChange(): void {
  clearCaches();
  try {
    const channel = new BroadcastChannel(KEY_LOCK);
    channel.postMessage("changed");
    channel.close();
  } catch { /* BroadcastChannel unavailable */ }
}

if (typeof BroadcastChannel !== "undefined") {
  const channel = new BroadcastChannel(KEY_LOCK);
  channel.onmessage = () => {
    clearCaches();
    if (typeof location !== "undefined") location.reload();
  };
}

function reloadAfterKeyChange(): void {
  markKeyMinted();
  announceKeyChange();
  if (typeof location !== "undefined") location.reload();
}

async function withKeyLock<T>(destructive: boolean, action: () => Promise<T>): Promise<T> {
  if (typeof navigator !== "undefined" && navigator.locks) {
    return navigator.locks.request(KEY_LOCK, action);
  }
  if (destructive) throw new Error("Web Locks is required to rotate or reset this device key");
  return action();
}

async function openDb(): Promise<IDBDatabase> {
  const { promise, resolve, reject } = promiseWithResolvers<IDBDatabase>();
  const req = indexedDB.open(DB_NAME, 1);
  req.onupgradeneeded = () => req.result.createObjectStore(STORE_NAME);
  req.onsuccess = () => resolve(req.result);
  req.onerror = () => reject(req.error);
  return promise;
}

function getRecord<T>(db: IDBDatabase, key: string): Promise<T | null> {
  const { promise, resolve, reject } = promiseWithResolvers<T | null>();
  const req = db.transaction(STORE_NAME, "readonly").objectStore(STORE_NAME).get(key);
  req.onsuccess = () => resolve(req.result ?? null);
  req.onerror = () => reject(req.error);
  return promise;
}

function addRecord(db: IDBDatabase, key: string, value: unknown): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const tx = db.transaction(STORE_NAME, "readwrite");
  tx.objectStore(STORE_NAME).add(value, key);
  tx.oncomplete = () => resolve();
  tx.onerror = () => reject(tx.error);
  tx.onabort = () => reject(tx.error);
  return promise;
}

function deleteMatchingStage(db: IDBDatabase, operationId: string): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const tx = db.transaction(STORE_NAME, "readwrite");
  const store = tx.objectStore(STORE_NAME);
  const read = store.get(ROTATION_ID);
  read.onsuccess = () => {
    const stage: RotationStage | undefined = read.result;
    if (stage?.operationId === operationId) store.delete(ROTATION_ID);
  };
  tx.oncomplete = () => resolve();
  tx.onerror = () => reject(tx.error);
  tx.onabort = () => reject(tx.error);
  return promise;
}

function promoteStage(db: IDBDatabase, stage: RotationStage): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const tx = db.transaction(STORE_NAME, "readwrite");
  const store = tx.objectStore(STORE_NAME);
  const read = store.get(ROTATION_ID);
  read.onsuccess = () => {
    const current: RotationStage | undefined = read.result;
    if (current?.operationId !== stage.operationId) {
      tx.abort();
      return;
    }
    store.put(stage.keyPair, KEY_ID);
    store.delete(ROTATION_ID);
  };
  tx.oncomplete = () => resolve();
  tx.onerror = () => reject(tx.error);
  tx.onabort = () => reject(tx.error ?? new Error("rotation stage changed"));
  return promise;
}

function deleteCurrentKey(db: IDBDatabase): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const tx = db.transaction(STORE_NAME, "readwrite");
  tx.objectStore(STORE_NAME).delete(KEY_ID);
  tx.oncomplete = () => resolve();
  tx.onerror = () => reject(tx.error);
  tx.onabort = () => reject(tx.error);
  return promise;
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
  const payload = b64url(JSON.stringify({ sub: "web", aud: "roost-coordinator", iat: now }));
  const message = new TextEncoder().encode(`${header}.${payload}`);
  const signature = await crypto.subtle.sign("Ed25519", pair.privateKey, message);
  return `${header}.${payload}.${b64url(signature)}`;
}

async function probePair(pair: CryptoKeyPair): Promise<ProbeResult> {
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
  const stage = await getRecord<RotationStage>(db, ROTATION_ID);
  if (!stage) return "none";
  const staged = await probePair(stage.keyPair);
  if (staged === "authorized") {
    await promoteStage(db, stage);
    clearCaches();
    return "promoted";
  }
  if (staged !== "device-rejected") return "ambiguous";
  const current = await getRecord<CryptoKeyPair>(db, KEY_ID);
  if (!current) return "ambiguous";
  const old = await probePair(current);
  if (old !== "authorized") return "ambiguous";
  await deleteMatchingStage(db, stage.operationId);
  return "discarded";
}

async function loadOrGenerateLocked(): Promise<CryptoKeyPair> {
  const db = await openDb();
  const recovery = await recoverStageLocked(db);
  if (recovery === "promoted") {
    const promoted = await getRecord<CryptoKeyPair>(db, KEY_ID);
    if (!promoted) throw new Error("Promoted device key is missing");
    reloadAfterKeyChange();
    return promoted;
  }
  const existing = await getRecord<CryptoKeyPair>(db, KEY_ID);
  if (existing) return existing;

  if (hadKeyBefore()) signal("auth.key_evicted", { db: DB_NAME });
  else diag("auth.key_first_boot", {});
  const generated = await generateKeyPair();
  try {
    await addRecord(db, KEY_ID, generated);
    markKeyMinted();
    return generated;
  } catch (error) {
    if (!(error instanceof DOMException) || error.name !== "ConstraintError") throw error;
    const winner = await getRecord<CryptoKeyPair>(db, KEY_ID);
    if (!winner) throw error;
    return winner;
  }
}

async function loadOrGenerate(): Promise<CryptoKeyPair> {
  if (_cachedKeyPair) return _cachedKeyPair;
  if (_inFlight) return _inFlight;
  _inFlight = withKeyLock(false, loadOrGenerateLocked);
  try {
    _cachedKeyPair = await _inFlight;
    return _cachedKeyPair;
  } finally {
    _inFlight = null;
  }
}

export async function getPublicKeyB64(): Promise<string> {
  if (_cachedPubKeyB64) return _cachedPubKeyB64;
  const pair = await loadOrGenerate();
  _cachedPubKeyB64 = await publicKeyB64For(pair);
  return _cachedPubKeyB64;
}

export async function getCurrentWebKeyInfo(): Promise<{ fingerprint: string; extractable: boolean }> {
  const pair = await loadOrGenerate();
  return { fingerprint: await fingerprintFor(pair), extractable: pair.privateKey.extractable };
}

export async function signCoordinatorJwt(): Promise<string> {
  if (_cachedJwt && Date.now() - _cachedJwt.iatMs < JWT_CACHE_TTL_MS) return _cachedJwt.token;
  const pair = await loadOrGenerate();
  const token = await signCoordinatorJwtWithKeyPair(pair);
  _cachedKidHex = await fingerprintFor(pair);
  _cachedJwt = { token, iatMs: Date.now() };
  return token;
}

export async function rotateCurrentWebKey(label: string): Promise<void> {
  await withKeyLock(true, async () => {
    const db = await openDb();
    const recovery = await recoverStageLocked(db);
    if (recovery === "ambiguous") {
      throw new Error("A prior key rotation is still ambiguous; retry when the coordinator is reachable");
    }
    if (recovery === "promoted") {
      reloadAfterKeyChange();
      return;
    }
    const current = await getRecord<CryptoKeyPair>(db, KEY_ID);
    if (!current) throw new Error("Current device key is missing");
    const stage: RotationStage = { operationId: crypto.randomUUID(), keyPair: await generateKeyPair() };
    await addRecord(db, ROTATION_ID, stage);
    const { makeCoordinatorClientForSigner } = await import("../connect.ts");
    const client = makeCoordinatorClientForSigner(() => signCoordinatorJwtWithKeyPair(current));
    await client.devicesRotateCurrent({
      sshPubkeyB64: await publicKeyB64For(stage.keyPair),
      label,
    });
    await promoteStage(db, stage);
    reloadAfterKeyChange();
  });
}

export async function isResetWebKeyEligible(): Promise<boolean> {
  return withKeyLock(false, async () => probePair(await loadOrGenerateLocked())
    .then((result) => result === "device-rejected"));
}

export async function resetWebKey(): Promise<void> {
  await withKeyLock(true, async () => {
    const db = await openDb();
    const recovery = await recoverStageLocked(db);
    if (recovery === "ambiguous") {
      throw new Error("Key state is ambiguous; reset refused until the coordinator is reachable");
    }
    if (recovery === "promoted") {
      reloadAfterKeyChange();
      return;
    }
    const current = await getRecord<CryptoKeyPair>(db, KEY_ID);
    if (!current) return;
    if (await probePair(current) !== "device-rejected") {
      throw new Error("Reset is allowed only after this device key is explicitly rejected");
    }
    await deleteCurrentKey(db);
    announceKeyChange();
    if (typeof location !== "undefined") location.reload();
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
