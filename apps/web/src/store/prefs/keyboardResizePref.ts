// keyboardResizePref — how the soft keyboard should affect the terminal.
//
// Default = PUSH (signal false): the shell stays full height and AppShell
// translateY's the content up by --kb-offset, so the grid size NEVER changes.
// The terminal does zero recomputation while the keyboard slides in — the top
// just scrolls off above the keyboard. No per-frame grid re-claim, no
// scrollback recompute. This is the calm default.
//
// Opt-in = RESIZE (signal true, set the Settings switch): shrink the shell to
// 100svh − --kb-offset to see more rows above the keyboard, at the cost of the
// terminal's ResizeObserver re-fitting the grid every frame as --kb-offset
// ramps. Safe to offer as an option in cell mode — the client never reflows
// history, so a height change can't corrupt scrollback the way the old byte
// renderer could.
//
// Persisted per device; reactive so AppShell's shell/main styles switch live.
// Toggled from Settings; read by AppShell (shellStyle + mainStyle).

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.keyboardResize";

function load(): boolean {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v === null ? false : v === "1"; // default OFF = push mode
  } catch {
    return false;
  }
}

const [keyboardResize, _set] = createSignal<boolean>(load());

/** Reactive accessor — read inside JSX so the layout switches live. */
export { keyboardResize };

export function setKeyboardResize(on: boolean): void {
  try { localStorage.setItem(STORAGE_KEY, on ? "1" : "0"); } catch { /* ignore */ }
  _set(on);
}
