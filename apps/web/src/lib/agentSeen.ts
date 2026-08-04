// Browser-profile acknowledgement state for coding-agent revisions. Revisions
// only move forward; storage events merge maxima so viewing a session in one
// tab clears its unseen marker in every tab in the profile.

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.agentSeen.v1";
const PERSIST_DEBOUNCE_MS = 250;

function parseStored(raw: string | null): Map<string, number> {
  const result = new Map<string, number>();
  if (!raw) return result;
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return result;
    for (const [sessionId, revision] of Object.entries(parsed)) {
      if (Number.isSafeInteger(revision) && (revision as number) >= 0) {
        result.set(sessionId, revision as number);
      }
    }
  } catch { /* malformed profile state starts empty */ }
  return result;
}

function readStorage(): Map<string, number> {
  try {
    return typeof localStorage === "undefined"
      ? new Map<string, number>()
      : parseStored(localStorage.getItem(STORAGE_KEY));
  } catch {
    return new Map<string, number>();
  }
}

const seen = readStorage();
const [version, setVersion] = createSignal(0);
let persistTimer: ReturnType<typeof setTimeout> | null = null;

function mergeStored(incoming: ReadonlyMap<string, number>): boolean {
  let changed = false;
  for (const [sessionId, revision] of incoming) {
    if (revision <= (seen.get(sessionId) ?? -1)) continue;
    seen.set(sessionId, revision);
    changed = true;
  }
  if (changed) setVersion((value) => value + 1);
  return changed;
}

export function seenAgentRevision(sessionId: string): number {
  version();
  return seen.get(sessionId) ?? 0;
}

export function markAgentSeen(sessionId: string, revision: number): boolean {
  if (!Number.isSafeInteger(revision) || revision < 0) return false;
  if (revision <= (seen.get(sessionId) ?? 0)) return false;
  seen.set(sessionId, revision);
  setVersion((value) => value + 1);
  schedulePersist();
  return true;
}

function schedulePersist(): void {
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    flushAgentSeen();
  }, PERSIST_DEBOUNCE_MS);
}

/** Merge before writing so independent sessions acknowledged by two tabs survive. */
export function flushAgentSeen(): void {
  if (persistTimer) {
    clearTimeout(persistTimer);
    persistTimer = null;
  }
  try {
    if (typeof localStorage === "undefined") return;
    mergeStored(parseStored(localStorage.getItem(STORAGE_KEY)));
    localStorage.setItem(STORAGE_KEY, JSON.stringify(Object.fromEntries(seen)));
  } catch { /* private mode / quota: keep the in-memory acknowledgement */ }
}

/** Install cross-tab merge and last-chance pagehide persistence. */
export function startAgentSeenPersistence(): () => void {
  if (typeof window === "undefined") return () => {};
  const onStorage = (event: StorageEvent) => {
    if (event.key !== STORAGE_KEY) return;
    mergeStored(parseStored(event.newValue));
  };
  const onPageHide = () => flushAgentSeen();
  window.addEventListener("storage", onStorage);
  window.addEventListener("pagehide", onPageHide);
  return () => {
    window.removeEventListener("storage", onStorage);
    window.removeEventListener("pagehide", onPageHide);
    flushAgentSeen();
  };
}

export function resetAgentSeenForTest(): void {
  if (persistTimer) clearTimeout(persistTimer);
  persistTimer = null;
  seen.clear();
  setVersion((value) => value + 1);
  try { localStorage.removeItem(STORAGE_KEY); } catch { /* unavailable */ }
}
