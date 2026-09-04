// This module owns the IndexedDB schema and atomic transactions for browser keys.
// web-key.ts calls it while loading, rotating, resetting, or revoking an identity.
// Keeping staged-key promotion here prevents partial writes during key rotation.
// It depends only on IndexedDB and structured-clone support for CryptoKeyPair.

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
}

export interface WebKeyRotationStage {
  operationId: string;
  keyPair: CryptoKeyPair;
}

export const WEB_KEY_DB_NAME = "roost-auth";

export async function openWebKeyDatabase(): Promise<IDBDatabase> {
  const { promise, resolve, reject } = promiseWithResolvers<IDBDatabase>();
  const request = indexedDB.open(WEB_KEY_DB_NAME, DB_VERSION);
  request.onupgradeneeded = () => {
    const database = request.result;
    if (!database.objectStoreNames.contains(STORE_NAME)) database.createObjectStore(STORE_NAME);
    if (database.objectStoreNames.contains(LEGACY_TRUST_STORE_NAME)) {
      database.deleteObjectStore(LEGACY_TRUST_STORE_NAME);
    }
  };
  request.onsuccess = () => resolve(request.result);
  request.onerror = () => reject(request.error);
  return promise;
}

export function readCurrentWebKey(database: IDBDatabase): Promise<CryptoKeyPair | null> {
  return readRecord<CryptoKeyPair>(database, KEY_ID);
}

export function readWebKeyRotationStage(
  database: IDBDatabase,
): Promise<WebKeyRotationStage | null> {
  return readRecord<WebKeyRotationStage>(database, ROTATION_ID);
}

export function addCurrentWebKey(
  database: IDBDatabase,
  keyPair: CryptoKeyPair,
): Promise<void> {
  return addRecord(database, KEY_ID, keyPair);
}

export function addWebKeyRotationStage(
  database: IDBDatabase,
  stage: WebKeyRotationStage,
): Promise<void> {
  return addRecord(database, ROTATION_ID, stage);
}

export function deleteMatchingWebKeyRotationStage(
  database: IDBDatabase,
  operationId: string,
): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const transaction = database.transaction(STORE_NAME, "readwrite");
  const store = transaction.objectStore(STORE_NAME);
  const read = store.get(ROTATION_ID);
  read.onsuccess = () => {
    const stage: WebKeyRotationStage | undefined = read.result;
    if (stage?.operationId === operationId) store.delete(ROTATION_ID);
  };
  transaction.oncomplete = () => resolve();
  transaction.onerror = () => reject(transaction.error);
  transaction.onabort = () => reject(transaction.error);
  return promise;
}

export function promoteWebKeyRotationStage(
  database: IDBDatabase,
  stage: WebKeyRotationStage,
): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const transaction = database.transaction(STORE_NAME, "readwrite");
  const store = transaction.objectStore(STORE_NAME);
  const read = store.get(ROTATION_ID);
  read.onsuccess = () => {
    const current: WebKeyRotationStage | undefined = read.result;
    if (current?.operationId !== stage.operationId) {
      transaction.abort();
      return;
    }
    store.put(stage.keyPair, KEY_ID);
    store.delete(ROTATION_ID);
  };
  transaction.oncomplete = () => resolve();
  transaction.onerror = () => reject(transaction.error);
  transaction.onabort = () => reject(transaction.error ?? new Error("rotation stage changed"));
  return promise;
}

export function deleteCurrentWebKey(database: IDBDatabase): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const transaction = database.transaction(STORE_NAME, "readwrite");
  transaction.objectStore(STORE_NAME).delete(KEY_ID);
  transaction.oncomplete = () => resolve();
  transaction.onerror = () => reject(transaction.error);
  transaction.onabort = () => reject(transaction.error);
  return promise;
}

export function deleteAllWebKeyMaterial(database: IDBDatabase): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const transaction = database.transaction(STORE_NAME, "readwrite");
  const store = transaction.objectStore(STORE_NAME);
  store.delete(KEY_ID);
  store.delete(ROTATION_ID);
  transaction.oncomplete = () => resolve();
  transaction.onerror = () => reject(transaction.error);
  transaction.onabort = () => reject(transaction.error);
  return promise;
}

const STORE_NAME = "keys";
const LEGACY_TRUST_STORE_NAME = "trust";
const DB_VERSION = 2;
const KEY_ID = "ed25519";
const ROTATION_ID = "ed25519-rotation-v1";

function promiseWithResolvers<T>(): Deferred<T> {
  if (typeof Promise.withResolvers === "function") return Promise.withResolvers<T>();
  let resolve!: Deferred<T>["resolve"];
  let reject!: Deferred<T>["reject"];
  // Compatibility path for Safari releases predating Promise.withResolvers.
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function readRecord<T>(database: IDBDatabase, key: string): Promise<T | null> {
  const { promise, resolve, reject } = promiseWithResolvers<T | null>();
  const request = database.transaction(STORE_NAME, "readonly").objectStore(STORE_NAME).get(key);
  request.onsuccess = () => resolve(request.result ?? null);
  request.onerror = () => reject(request.error);
  return promise;
}

function addRecord(database: IDBDatabase, key: string, value: unknown): Promise<void> {
  const { promise, resolve, reject } = promiseWithResolvers<void>();
  const transaction = database.transaction(STORE_NAME, "readwrite");
  transaction.objectStore(STORE_NAME).add(value, key);
  transaction.oncomplete = () => resolve();
  transaction.onerror = () => reject(transaction.error);
  transaction.onabort = () => reject(transaction.error);
  return promise;
}
