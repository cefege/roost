// Persisted toggle: forward pointer + touch gestures to the running TUI as
// SGR-1006 mouse-tracking (claude fullscreen click / drag / scroll) instead
// of letting the browser handle them. OFF (default) = native browser
// selection + DOM scroll; ON = claude gets the mouse, so a finger-swipe
// scrolls claude and a click hits its UI. Toggled from Settings → Terminal.
// Module-level signal so every deck-mounted pane shares one state; localStorage
// makes the choice sticky across reloads.
//
// Consumed by CellTerminal's pointer/touch handlers (only forward when this
// is on AND the session is in alt-screen — a plain shell keeps native scroll).

import { createSignal } from "solid-js";

const KEY = "roostMouseForward";
const read = (): boolean => {
  try { return localStorage.getItem(KEY) === "1"; } catch { return false; }
};

const [mouseForwardEnabled, _set] = createSignal(read());
export { mouseForwardEnabled };

export const toggleMouseForward = (): void => {
  _set((on) => {
    const next = !on;
    try { localStorage.setItem(KEY, next ? "1" : "0"); } catch { /* ignore */ }
    return next;
  });
};
