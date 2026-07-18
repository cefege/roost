// keyboardOnDesktop — client-only UI pref: render the on-screen nav/key pad
// (TerminalNavButtons) on desktop (medium/expanded) viewports, not just
// compact. Default ON. Persisted to localStorage (per device/browser) and
// reactive, so CellTerminal's <Show> toggles live with no reload. Toggled from
// Settings → Terminal (TerminalPane); read by the nav-pad gate in
// CellTerminal.tsx alongside isCompact(). Sibling pref for the mic:
// lib/micOnDesktop.ts.

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.keyboardOnDesktop";

function load(): boolean {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v === null ? true : v === "1"; // default ON; respect explicit "0"
  } catch {
    return true;
  }
}

const [keyboardOnDesktop, setKeyboardOnDesktopSignal] = createSignal<boolean>(load());

/** Reactive accessor — read inside JSX to toggle the desktop nav pad live. */
export { keyboardOnDesktop };

/** Persist + flip the pref. Applies immediately (reactive signal). */
export function setKeyboardOnDesktop(on: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, on ? "1" : "0");
  } catch {
    // private-mode / storage-disabled: keep the in-memory signal only.
  }
  setKeyboardOnDesktopSignal(on);
}
