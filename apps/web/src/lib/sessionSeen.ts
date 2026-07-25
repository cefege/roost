// Per-session "last seen" timestamps — the data behind the "Needs you"
// for FINISHED agents (not just blocked ones). A session is marked seen while
// you're viewing it (FolderList effect); a finished agent whose last
// message is newer than its last-seen stamp = output you haven't looked at →
// it surfaces in the broadened needs-attention band.
//
// Persisted to localStorage so "unseen" survives a reload. Monotonic: a stamp
// only ever moves forward.
//
// Internals (perf sweep): a createStore keyed by session id — lastSeenAt(id)
// subscribes ONLY to its own key, so one session's stamp moving no longer
// notifies every other row's subscriber (the old `{ equals: false }` signal
// broadcast to ALL of them, even on the monotonic no-op branch). Persistence
// is a trailing debounce that stringifies the map ONCE per burst instead of
// synchronously per write; a pending flush survives tab close via pagehide.

import { untrack } from "solid-js";
import { createStore, unwrap } from "solid-js/store";

const KEY = "roost.sidebar.seen";

function read(): Record<string, number> {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return v && typeof v === "object" ? v as Record<string, number> : {};
  } catch { return {}; }
}

const [_seen, _setSeen] = createStore<Record<string, number>>(read());

/** Reactive: ms timestamp you last viewed this session (0 = never). */
export function lastSeenAt(sessionId: string): number {
  return _seen[sessionId] ?? 0;
}

const SEED_KEY = "roost.sidebar.seen.seeded";

/** First-run seed: stamp every currently-open session as seen NOW, once ever,
 *  so enabling the attention bands doesn't dump every pre-existing finished
 *  agent into "Needs you" on first load. Only activity AFTER the seed surfaces.
 *  Idempotent across reloads via a localStorage marker. */
export function seedSeenOnce(sessionIds: string[]): void {
  try { if (localStorage.getItem(SEED_KEY) === "1") return; } catch { return; }
  const now = Date.now();
  for (const id of sessionIds) markSeen(id, now);
  try { localStorage.setItem(SEED_KEY, "1"); } catch { /* quota / privacy */ }
}

// ── debounced persist ────────────────────────────────────────────────────────
const PERSIST_DEBOUNCE_MS = 500;
let _persistTimer = 0;
let _persistPending = false;
let _flushHooked = false;

function _flushPersist(): void {
  if (_persistTimer) { clearTimeout(_persistTimer); _persistTimer = 0; }
  if (!_persistPending) return;
  _persistPending = false;
  try { localStorage.setItem(KEY, JSON.stringify(unwrap(_seen))); } catch { /* quota / privacy */ }
}

function _schedulePersist(): void {
  _persistPending = true;
  // Flush a pending persist on tab close so stamps survive. pagehide (not
  // unload) — it fires on bfcache navigations and on mobile Safari. Hooked
  // lazily on first schedule (not at import) so the module stays free of
  // import-time side effects.
  if (!_flushHooked && typeof window !== "undefined") {
    _flushHooked = true;
    window.addEventListener("pagehide", _flushPersist);
  }
  if (_persistTimer) return; // trailing debounce: one stringify per burst
  _persistTimer = setTimeout(() => {
    _persistTimer = 0;
    _flushPersist();
  }, PERSIST_DEBOUNCE_MS) as unknown as number;
}

/** Test seam: drop the pending debounce and the pagehide latch. bun test
 *  shares ONE module registry across files, so a suite that merely calls
 *  markSeen (folderGroupsPriority) leaves _persistTimer armed and _flushHooked
 *  latched against its own window. The next suite's _schedulePersist() then
 *  returns early at the timer guard and never re-registers pagehide, so its
 *  fake-timer advance flushes nothing. Production has one window and one
 *  lifetime; only tests need to rewind this. */
export function __resetPersistForTests(): void {
  if (_persistTimer) clearTimeout(_persistTimer);
  _persistTimer = 0;
  _persistPending = false;
  _flushHooked = false;
}

/** Mark a session seen up to `ts` (default now). Monotonic; no-op if older —
 *  the no-op branch fires NO reactive notification at all. untrack on the
 *  guard read so effect-hosted callers (FolderList/TerminalDeck) don't
 *  self-subscribe to the key they're writing. */
export function markSeen(sessionId: string, ts: number = Date.now()): void {
  if (untrack(() => _seen[sessionId] ?? 0) >= ts) return;
  _setSeen(sessionId, ts);
  _schedulePersist();
}
