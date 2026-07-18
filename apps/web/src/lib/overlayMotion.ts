// overlayMotion — M3 enter-motion for bespoke full-screen/large overlays that
// gate on a bare <Show> (CommandPalette, HelpOverlay, Deploy/Transfer console
// docks). They can't use md-dialog (custom search/list/
// console structure) so this drives the enter transition via the Web Animations
// API on the mounted node — no @keyframes CSS, no unmount-delay machinery, so
// the existing open/close (<Show>) logic is untouched. Enter-only by design;
// exit stays instant (bare <Show> unmount) to avoid refactoring close paths.
//
// Tokens mirror theme-vars.css: emphasized-decelerate easing @ medium2 (300ms)
// for panels, short4 (200ms) scrim fade. Honors prefers-reduced-motion.
//
// Callers: the overlay components above wire `ref={animateOverlayPanel}` on
// the panel element. Scrim visibility is base CSS (rgba backdrop at opacity
// 1) — NEVER animate scrim opacity; a frozen fade-in hides the whole overlay.

// (no Solid reactivity needed here — createOverlayPresence just wires refs)

const REDUCED = (): boolean =>
  typeof window !== "undefined" &&
  window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;

const EMPHASIZED_DECELERATE = "cubic-bezier(0.05, 0.7, 0.1, 1)";

// Keep-mounted-during-exit: lets a bare-<Show> overlay animate OUT before it
// unmounts (M3 exits accelerate). Usage in a component:
//   const { present, setPanelRef } = createOverlayPresence(myOpenSignal);
//   <Show when={present()}> ... <div ref={setPanelRef} ...> ...
// `present` stays true through the exit animation, then flips false → unmount.
// `setPanelRef` plays the enter on mount + holds the node for the exit. `kind`
// selects panel vs dock geometry (mirrors the enter animations above).
export function createOverlayPresence(
  open: () => boolean,
  kind: "panel" | "dock" = "panel",
): { present: () => boolean; setPanelRef: (el: HTMLElement) => void } {
  const enter = kind === "dock" ? animateOverlayDock : animateOverlayPanel;
  // Mount mirrors open() exactly (close is INSTANT — an exit animation left the
  // dark scrim lingering ~200ms after the panel shrank, reading as "a window
  // underneath that disappears a bit later"). So `present` IS `open`; the only
  // job left is playing the transform-only enter on mount via setPanelRef.
  return { present: open, setPanelRef: (el) => enter(el) };
}

// Panel slide-up + scale 0.97→1. medium2 (300ms). Transform only — opacity is
// never animated so the panel can't get stuck invisible.
export function animateOverlayPanel(el: HTMLElement): void {
  if (REDUCED() || typeof el.animate !== "function") return;
  el.animate(
    [
      { transform: "translateY(8px) scale(0.97)" },
      { transform: "translateY(0) scale(1)" },
    ],
    { duration: 300, easing: EMPHASIZED_DECELERATE, fill: "backwards" },
  );
}

// Docked console (bottom-right) slide-up. medium2 (300ms). Transform only.
export function animateOverlayDock(el: HTMLElement): void {
  if (REDUCED() || typeof el.animate !== "function") return;
  el.animate(
    [
      { transform: "translateY(12px) scale(0.98)" },
      { transform: "translateY(0) scale(1)" },
    ],
    { duration: 300, easing: EMPHASIZED_DECELERATE, fill: "backwards" },
  );
}
