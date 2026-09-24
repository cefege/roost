// Owns the touch terminal-key sheet's shared open signal and focus handoff.
// TerminalNavButtons registers its per-sheet modifier release; padActions and
// other controller adapters use the same close/toggle paths.

import { createSignal } from "solid-js";

const PAD_OPEN_KEY = "roostNavPadOpen";

const readPadOpen = (): boolean => {
  try {
    return localStorage.getItem(PAD_OPEN_KEY) === "1";
  } catch {
    return false;
  }
};

const [navPadOpen, setNavPadOpen] = createSignal(readPadOpen());

const persistNavPadOpen = (open: boolean): void => {
  setNavPadOpen(open);
  try {
    localStorage.setItem(PAD_OPEN_KEY, open ? "1" : "0");
  } catch {
    // Ignore unavailable storage.
  }
};

// The mounted sheet owns the active Ctrl/Alt latches. Identity-guarded release
// lets a replacement sheet register before the previous instance cleans up.
let disarmModifiers: (() => void) | null = null;

export function registerTerminalNavPadDisarm(disarm: () => void): () => void {
  disarmModifiers = disarm;
  return () => {
    if (disarmModifiers === disarm) disarmModifiers = null;
  };
}

/** The reactive read of key-pad visibility, so the controller adapter can tell
 *  "open the pad" from "press the focused key" without owning the signal. */
export function terminalNavPadOpen(): boolean {
  return navPadOpen();
}

/** The ONE close path, so every caller disarms: the sheet is the only surface
 *  that can clear a latched Ctrl. */
export function closeTerminalNavPad(): void {
  if (!navPadOpen()) return;
  disarmModifiers?.();
  persistNavPadOpen(false);
}

/** The ONE key-pad toggle: the sheet's own button, and the controller adapter's
 *  `keypad` / `activate` actions. Closing always disarms the latches. */
export function toggleTerminalNavPad(): void {
  if (navPadOpen()) {
    closeTerminalNavPad();
    return;
  }
  persistNavPadOpen(true);
}

const FIRST_KEY_FOCUS_ATTEMPTS = 4;

/** Focus the pad's first key, for a device with no pointer: opening the pad is
 * useless to a controller until focus is inside it. The sheet is a Portal
 * mounted from a signal write, so the grid is absent for at least one turn —
 * retry by frame, bounded, and stop as soon as the key actually took focus. */
export function focusTerminalNavPadFirstKey(): () => void {
  let cancelled = false;
  let animationFrame: number | null = null;
  let attempts = 0;
  const focusWhenMounted = () => {
    if (cancelled) return;
    const target = document.querySelector<HTMLElement>(".term-nav__grid button:not(:disabled)");
    if (target) {
      target.focus();
      if (document.activeElement === target) return;
    }
    attempts++;
    if (attempts < FIRST_KEY_FOCUS_ATTEMPTS) animationFrame = requestAnimationFrame(focusWhenMounted);
  };
  queueMicrotask(focusWhenMounted);
  return () => {
    cancelled = true;
    if (animationFrame !== null) cancelAnimationFrame(animationFrame);
  };
}
