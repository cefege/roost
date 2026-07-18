import { createSignal } from "solid-js";

// True while the user drags a pane divider (PaneDivider) or the desktop sidebar
// resizer (AppShell). CellTerminal gates its ResizeObserver-driven PTY resize
// claims on this: a drag would otherwise round-trip a full grid rebuild per
// paused frame (the "laggy resize" bug). Instead it suppresses claims during the
// drag and fires ONE claim for the final size on release.
const [isResizeDragging, setResizeDragging] = createSignal(false);
export { isResizeDragging };
export function beginResizeDrag(): void { setResizeDragging(true); }
export function endResizeDrag(): void { setResizeDragging(false); }

// Bumped once per arrange-preset commit (TerminalDeck doArrange). CellTerminal
// watches it and fires ONE band-bypassing settle claim per visible pane —
// arrange rect deltas are often within the ±CLAIM_BAND hold-anchor, so the
// passive ResizeObserver claim gets suppressed and the PTY keeps the stale
// grid. Deliberately NOT isResizeDragging: that flag means "user is dragging"
// and toggles the deck's data-resizing CSS (glow drop).
const [arrangeEpoch, setArrangeEpoch] = createSignal(0);
export { arrangeEpoch };
export function pulseArrange(): void { setArrangeEpoch((n) => n + 1); }
