// micOnDesktop — client-only UI pref: render the voice mic button on desktop
// (medium/expanded) viewports, not just compact. Default ON. Persisted to
// localStorage (per device/browser) and reactive, so CellTerminal's <Show>
// toggles live with no reload. Toggled from Settings → Voice
// (TranscriptionPane); read by the mic gate in CellTerminal.tsx alongside
// isCompact(). Sibling pref for the nav pad: lib/keyboardOnDesktop.ts.

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.micOnDesktop";

function load(): boolean {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v === null ? true : v === "1"; // default ON; respect explicit "0"
  } catch {
    return true;
  }
}

const [micOnDesktop, setMicOnDesktopSignal] = createSignal<boolean>(load());

/** Reactive accessor — read inside JSX to toggle the desktop mic live. */
export { micOnDesktop };

/** Persist + flip the pref. Applies immediately (reactive signal). */
export function setMicOnDesktop(on: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, on ? "1" : "0");
  } catch {
    // private-mode / storage-disabled: keep the in-memory signal only.
  }
  setMicOnDesktopSignal(on);
}
