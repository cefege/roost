// One reactive document-visibility signal. Replaces the ~10 ad-hoc
// `document.hidden` / `visibilityState` polls scattered across the app.
// `pageVisible()` is true while the browser tab is foregrounded — gate work that
// must not run in a backgrounded tab (e.g. marking on-screen terminals "seen",
// so a backgrounded tab doesn't silently clear the attention band).
//
// setForceVisible (automation pin, exposed as `__smoke.forceVisible`): pins
// app-level visibility to foreground regardless of document.visibilityState,
// so a backgrounded verification tab keeps its viewport claims, sync stream,
// and timers instead of withdrawing/stalling (Author 2026-07-11 "push to
// front via API" — verification runs kept losing to tab-visibility races).
// SCOPE: the pin governs SPA behavior only. The painted path is synchronous
// (cellRenderer.apply runs in the WS cell handler), so frames that ARRIVE
// paint immediately — but Chrome can still starve a long-backgrounded tab's
// HTTP/2 server-stream at the transport layer (sync.ts:62-69). Keep hidden
// probe windows short; the health poll + re-dial (unpinned from hidden by
// this module) recover a starved stream. Every consumer MUST read visibility
// through this module (pageVisible() reactive / isPageVisible() plain) — a
// raw `document.hidden` read bypasses the pin.

import { createSignal } from "solid-js";

let _forceVisible = false;

function current(): boolean {
  return _forceVisible || typeof document === "undefined" || document.visibilityState === "visible";
}

const [visible, setVisible] = createSignal(current());

if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => setVisible(current()));
}

/** Reactive: is the browser tab currently foregrounded? */
export function pageVisible(): boolean {
  return visible();
}

/** Non-reactive read for timer callbacks / DOM event handlers — the
 *  override-aware replacement for raw `document.hidden` /
 *  `document.visibilityState` checks. */
export function isPageVisible(): boolean {
  return current();
}

/** Automation pin. Toggling dispatches a synthetic visibilitychange so
 *  listener-based consumers (claim re-claim in CellTerminal, stream re-dial
 *  in sync-bootstrap, health poll in sync-health) re-evaluate through the
 *  pin immediately — turning it ON while hidden behaves like a refocus. */
export function setForceVisible(on: boolean): void {
  if (_forceVisible === on) return;
  _forceVisible = on;
  setVisible(current());
  if (typeof document !== "undefined") document.dispatchEvent(new Event("visibilitychange"));
}
