// Gmail-style undo-close for terminal sessions. When the user clicks
// the ✕ on a sidebar row, we DON'T fire sessionsKill immediately —
// we stash the session id in a pending set + start a 5s timer. The
// SessionRow filters itself out of the visible tree while pending
// (instant disappear), and a stack of snackbars offers Undo. After a
// timer fires, that session's sessionsKill RPC is sent.
//
// Each close is INDEPENDENT: its own timer, its own snackbar card, its
// own Undo. Closing a second tab does not touch the first's countdown.
// (Superseded the earlier single-batched-banner model — Author 2026-07-01:
// "multiple tabs with their own timer".) Undo restores that one row in
// place (its store entry was never mutated; only the local pending-set
// hid it). Let a timer expire → that RPC fires, store removes the row
// via the closed event (existing path).

import { createSignal } from "solid-js";

const UNDO_WINDOW_MS = 5000;

// The published view object is stable per entry so Solid's <For> keeps
// each snackbar's DOM node (and its running countdown animation) across
// publishes instead of remounting the whole stack on every new close.
// terminalName/folder/server = the three labels the snackbar shows: the
// closed terminal's name, its folder/workspace, and the server it ran on.
type PendingView = { sessionId: string; terminalName: string; folder: string; server: string };

type Entry = {
  view: PendingView;
  timer: ReturnType<typeof setTimeout>;
  killNow: () => void;       // fires the actual sessionsKill RPC after the window
  onUndo?: () => void;       // optional restore side-effect (pane path re-tiles)
};

const [_pending, _setPending] = createSignal<ReadonlyArray<PendingView>>([], { equals: false });
const entries = new Map<string, Entry>();

export const pendingCloses = _pending;

function publish(): void {
  _setPending(Array.from(entries.values()).map((e) => e.view));
}

export function isPendingClose(sessionId: string): boolean {
  // Read the signal first so Solid memos that wrap this call re-run
  // when the queue changes. Without the read, entries.has() goes
  // straight to the Map and the filter stays stale — sidebar rows
  // wouldn't disappear on close.
  for (const p of _pending()) if (p.sessionId === sessionId) return true;
  return false;
}

/** Schedule a soft-close: hide immediately, fire kill after this
 *  session's own undo window unless undoOne() is called first. Other
 *  pending closes are untouched. `labels` are snapshotted now (the
 *  session may be gone by the time the card renders). `onUndo` runs on
 *  restore — the pane path uses it to re-commit the pre-close tiling. */
export function scheduleClose(
  sessionId: string,
  labels: Omit<PendingView, "sessionId">,
  killNow: () => void,
  onUndo?: () => void,
): void {
  // Replace any prior entry for this same id (re-click on hidden row):
  // clear its timer + reuse its view object so the card doesn't remount.
  const prior = entries.get(sessionId);
  if (prior) clearTimeout(prior.timer);
  const view: PendingView = prior ? prior.view : { sessionId, ...labels };
  Object.assign(view, labels);
  const timer = setTimeout(() => {
    const e = entries.get(sessionId);
    entries.delete(sessionId);
    publish();
    if (e) e.killNow();
  }, UNDO_WINDOW_MS);
  entries.set(sessionId, { view, timer, killNow, onUndo });
  publish();
}

/** Restore one pending close (its Undo button). */
export function undoOne(sessionId: string): void {
  const e = entries.get(sessionId);
  if (!e) return;
  clearTimeout(e.timer);
  entries.delete(sessionId);
  publish();          // un-hide first (re-includes the row in liveIds)
  e.onUndo?.();        // then restore any side-effect (pane tiling) as the last write
}

/** Restore everything currently in the undo queue. */
export function undoAll(): void {
  const restores: Array<() => void> = [];
  for (const e of entries.values()) {
    clearTimeout(e.timer);
    if (e.onUndo) restores.push(e.onUndo);
  }
  entries.clear();
  publish();
  for (const r of restores) r();
}
/** Drop all delayed close commands at a dashboard boundary. Restoring a prior
 * layout or firing its kill callback would both target a stale resource. */
export function resetPendingCloses(): void {
  for (const entry of entries.values()) clearTimeout(entry.timer);
  entries.clear();
  publish();
}


export { UNDO_WINDOW_MS };
