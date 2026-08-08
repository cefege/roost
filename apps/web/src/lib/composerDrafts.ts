// composerDrafts — the unsent text sitting in a session's mobile composer bar,
// retained per session on THIS device (localStorage, never server-side). The
// composer component is torn down whenever its pane loses focus (CellTerminal's
// <Show> on props.focused), so a component signal cannot hold a draft across a
// pane switch, a nav-away, or a reload — WhatsApp/Telegram behaviour needs this
// module. Collapsing the bar (✕ / tap-outside / Escape) keeps the draft; only
// SEND consumes it, by writing "".
//
// Owner of this state: this module. getComposerDraft / saveComposerDraft — grep
// these. Nothing prunes on session death: the LRU-by-write cap below is the only
// eviction, deliberately, because the CellTerminal cleanup that would be the
// obvious prune hook fires on nav-away — exactly when the draft must survive.
//
// Perf: one top-level key holding the whole map, parsed ONCE at import. Reads
// happen at composer mount, not per render, so no signal is needed.

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

/** The retained draft for a session, or "" if none. */
export function getComposerDraft(sessionId: string): string {
  return drafts.get(sessionId) ?? "";
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
  // In-memory first: a quota failure still keeps the draft for the tab's life.
  // Never trimmed or truncated — leading/trailing spaces are real PTY input.
  try {
    localStorage.setItem(DRAFTS_KEY, JSON.stringify(Object.fromEntries(drafts)));
  } catch { /* quota / privacy mode */ }
}
