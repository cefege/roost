// TOFU coord fingerprint pin. Stores the first-seen coord fingerprint
// in IndexedDB. Subsequent loads verify against it. R4.3 auth deliverable.

import { signal } from "@roost/shared/diag";

const DB_NAME = "roost-auth";
const STORE_NAME = "trust";
const COORD_FP_KEY = "coord-fingerprint";

async function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME);
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

// Returns the pinned fingerprint, or null if first-boot.
async function getPinnedCoordFingerprint(): Promise<string | null> {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readonly");
    const req = tx.objectStore(STORE_NAME).get(COORD_FP_KEY);
    req.onsuccess = () => resolve((req.result as string) ?? null);
    req.onerror = () => reject(req.error);
  });
}

// TOFU: pin if not already pinned. Returns true if accepted, false if mismatch.
export async function trustCoordFingerprint(fp: string): Promise<boolean> {
  const db = await openDb();
  const existing = await getPinnedCoordFingerprint();

  if (existing === null) {
    // First boot — trust on first use.
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const req = tx.objectStore(STORE_NAME).put(fp, COORD_FP_KEY);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });
    return true;
  }

  // Pin mismatch = coord key rotated (or MITM). Surfaces as a re-pair
  // prompt; signal it so the daily digest catches coord-identity churn.
  if (existing !== fp) {
    signal("auth.pin_mismatch", { pinned8: existing.slice(0, 8), seen8: fp.slice(0, 8) });
  }
  return existing === fp;
}

// Clear pinned fingerprint (e.g. on explicit re-pair).
async function clearCoordTrust(): Promise<void> {
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite");
    const req = tx.objectStore(STORE_NAME).delete(COORD_FP_KEY);
    req.onsuccess = () => resolve();
    req.onerror = () => reject(req.error);
  });
}
