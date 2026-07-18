// keyboardResizePref — when the soft keyboard opens, RESIZE the terminal to
// fit the space above it (the grid reflows to fewer rows, grows back on
// dismiss) instead of pushing the whole terminal up off-screen. Default ON.
// Toggle OFF reverts to the legacy push behavior (AppShell translateY by
// --kb-offset; grid size unchanged, top scrolls off above the keyboard).
//
// Safe to resize in cell mode — the client never reflows history, so a
// keyboard-driven height change can't corrupt scrollback the way the old byte
// renderer could (which is why the push behavior existed). Persisted per
// device; reactive so AppShell's shell/main styles switch live. Toggled from
// Settings; read by AppShell (shellStyle + mainStyle).

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.keyboardResize";

function load(): boolean {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v === null ? true : v === "1"; // default ON
  } catch {
    return true;
  }
}

const [keyboardResize, _set] = createSignal<boolean>(load());

/** Reactive accessor — read inside JSX so the layout switches live. */
export { keyboardResize };

export function setKeyboardResize(on: boolean): void {
  try { localStorage.setItem(STORAGE_KEY, on ? "1" : "0"); } catch { /* ignore */ }
  _set(on);
}
