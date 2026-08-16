// One reactive document-visibility signal. Replaces the ~10 ad-hoc
// `document.hidden` / `visibilityState` polls scattered across the app.
// `pageVisible()` is true while the browser tab is foregrounded — gate work that
// must not run in a backgrounded tab (e.g. marking on-screen terminals "seen",
// so a backgrounded tab doesn't silently clear the attention band).
//
// Automation visibility pins, exposed as `__smoke.forceVisible` and
// `__smoke.forceHidden`, override document.visibilityState in either direction.
// Verification can therefore keep a background tab live or deterministically
// exercise hidden-tab lifecycle without asking Chromium to schedule the page a
// particular way. Turning either pin off releases every override and returns to
// the real document state.
//
// SCOPE: the pin governs SPA behavior only. The painted path is synchronous
// (cellRenderer.apply runs in the WS cell handler), so frames that ARRIVE paint
// immediately. Every consumer MUST read visibility through this module
// (pageVisible() reactive / isPageVisible() plain); a raw document.hidden read
// bypasses the pin.

import { createSignal } from "solid-js";

let _visibilityOverride: boolean | null = null;

function current(): boolean {
  return _visibilityOverride
    ?? (typeof document === "undefined" || document.visibilityState === "visible");
}

const [visible, setVisible] = createSignal(current());

if (typeof document !== "undefined" && typeof document.addEventListener === "function") {
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

function setVisibilityOverride(next: boolean | null): void {
  if (_visibilityOverride === next) return;
  _visibilityOverride = next;
  setVisible(current());
  if (typeof document !== "undefined"
    && typeof document.dispatchEvent === "function"
    && typeof Event !== "undefined") document.dispatchEvent(new Event("visibilitychange"));
}

/** Pin foreground visibility; false releases every automation override. */
export function setForceVisible(on: boolean): void {
  setVisibilityOverride(on ? true : null);
}

/** Pin background visibility; false releases every automation override. */
export function setForceHidden(on: boolean): void {
  setVisibilityOverride(on ? false : null);
}
