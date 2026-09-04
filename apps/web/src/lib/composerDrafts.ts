// composerDrafts — unsent terminal-composer text retained per session on this
// device (localStorage, never server-side).
//
// Owner of this state: this module. getComposerDraft / saveComposerDraft are the
// persistence API; subscribeComposerDraft keeps the brief responsive handoff
// between a pane composer and a viewport composer coherent. Nothing prunes on
// session death: navigation and parked-pane lifecycle must retain drafts, so the
// LRU-by-write cap below is the only eviction.
const DRAFTS_KEY = "roost.composerDrafts.v1";

// Bounds the blob. Evicted oldest-write-first (saveComposerDraft re-inserts).
const MAX_DRAFTS = 24;

function readDrafts(): Map<string, string> {
  try {
    const raw = localStorage.getItem(DRAFTS_KEY);
    const obj = raw ? JSON.parse(raw) : null;
    if (!obj || typeof obj !== "object") return new Map();
    return new Map(Object.entries(obj as Record<string, string>));
  } catch { return new Map(); }
}

const drafts = readDrafts();
const subscribers = new Map<string, Set<(text: string) => void>>();

/** The retained draft for a session, or "" if none. */
export function getComposerDraft(sessionId: string): string {
  return drafts.get(sessionId) ?? "";
}

/** Follow writes made by another live composer instance for the same session. */
export function subscribeComposerDraft(
  sessionId: string,
  subscriber: (text: string) => void,
): () => void {
  let sessionSubscribers = subscribers.get(sessionId);
  if (!sessionSubscribers) {
    sessionSubscribers = new Set();
    subscribers.set(sessionId, sessionSubscribers);
  }
  sessionSubscribers.add(subscriber);
  return () => {
    sessionSubscribers!.delete(subscriber);
    if (sessionSubscribers!.size === 0) subscribers.delete(sessionId);
  };
}

/** Write-through the session's draft. "" removes the entry. */
export function saveComposerDraft(sessionId: string, text: string): void {
  // The mount-time effect re-writes its own initial value; identical writes and
  // every keystroke that doesn't change the stored text cost nothing.
  if (drafts.get(sessionId) === (text || undefined)) return;
  // Delete-then-set moves a rewritten entry to the newest Map position, which is
  // what makes the cap LRU-by-write rather than insertion-order-forever.
  drafts.delete(sessionId);
  if (text !== "") drafts.set(sessionId, text);
  while (drafts.size > MAX_DRAFTS) drafts.delete(drafts.keys().next().value as string);
  for (const subscriber of subscribers.get(sessionId) ?? []) subscriber(text);
  // In-memory first: a quota failure still keeps the draft for the tab's life.
  // Never trimmed or truncated — leading/trailing spaces are real PTY input.
  try {
    localStorage.setItem(DRAFTS_KEY, JSON.stringify(Object.fromEntries(drafts)));
  } catch { /* quota / privacy mode */ }
}

/** Remove every account-scoped draft from memory and durable browser storage. */
export function clearComposerDraftsForLogout(): void {
  drafts.clear();
  for (const sessionSubscribers of subscribers.values()) {
    for (const subscriber of [...sessionSubscribers]) subscriber("");
  }
  try { localStorage.removeItem(DRAFTS_KEY); } catch { /* quota / privacy mode */ }
}
