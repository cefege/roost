// Persisted escape hatch for mouse + touch forwarding. Forwarding itself is
// gated on what the foreground application asked for (CellGridFrame.mouseTracking,
// DECSET 1000/1002 read off the core), so this toggle is not the detector — it is
// the user's override for the case the mode cannot express: keeping native
// browser selection and scroll inside an app that DOES request the mouse (mouse-
// selecting text out of htop). Module-level signal so every deck-mounted pane
// shares one state; localStorage makes the choice sticky across reloads.
//
// Default ON, because the gate is precise: an app that never requested tracking
// never receives events either way, so an opt-in only cost mouse-aware TUIs
// their mouse. Toggled from Settings → Terminal and the mobile nav-mouse key;
// consumed by CellTerminal's pointer/touch handlers, which forward only when this
// is on AND the frame reports a nonzero tracking mode.

import { createSignal } from "solid-js";
import type { MouseTracking } from "@roost/shared/cell";

const KEY = "roostMouseForward";
const read = (): boolean => {
  try { return localStorage.getItem(KEY) !== "0"; } catch { return true; }
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

/** Pointer and touch gestures belong to the terminal application, not the
 *  browser: the frame reports a tracking mode AND this device allows
 *  forwarding. One predicate so the gesture handlers and the pane's
 *  touch-action can never disagree about who owns a drag. */
export const mouseGesturesForwarded = (tracking: MouseTracking): boolean =>
  mouseForwardEnabled() && tracking !== 0;
