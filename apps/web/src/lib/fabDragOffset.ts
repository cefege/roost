// fabDragOffset — per-browser vertical lift for the bottom-right terminal FAB
// cluster (mic / attach / agent-launch / keyboard-nav). Press-drag ANY FAB up
// to move the whole stack off the terminal text; a quick tap still fires the
// button. Applied via the ONE global CSS var --roost-fab-dy that every FAB's
// bottom: calc() in voice-input.css reads, so all per-session CellTerminal
// clusters shift together and the offset is inherently per-browser.
// No Solid signal: nothing renders off it — pure CSS var + localStorage.
// Wired via onPointerDown={onFabPointerDown} on the 4 primary FAB buttons.
// Reuses dragArmed (dragThreshold.ts) + the PaneDivider.tsx window-listener,
// no-setPointerCapture recipe (FABs live in a per-session deck, capture would
// die on node recreation — feedback_for_recreates_node_kills_pointer_capture).

import { dragArmed } from "./dragThreshold.ts";

const KEY = "roost.fabOffsetY.v1";
const VAR = "--roost-fab-dy";
const CLUSTER_H = 190; // approx stack height incl. keys FAB + its sheet; keep on-screen

function clampY(y: number): number {
  const max = Math.max(0, window.innerHeight - CLUSTER_H);
  return Math.min(max, Math.max(0, y));
}

function setVar(y: number): void {
  document.documentElement.style.setProperty(VAR, `${y}px`);
}

function load(): number {
  try {
    const n = Number.parseInt(localStorage.getItem(KEY) ?? "", 10);
    return Number.isFinite(n) ? clampY(n) : 0;
  } catch {
    return 0; // private mode / storage disabled
  }
}

function persist(y: number): void {
  try {
    localStorage.setItem(KEY, String(y));
  } catch {
    // private mode / quota — offset stays for this session, just not durable
  }
}

let current = load();
setVar(current); // apply before the first FAB paints (module imported at boot)

// Re-clamp on viewport shrink (rotate / resize) so the cluster can't strand
// itself above the visible area.
window.addEventListener("resize", () => {
  const next = clampY(current);
  if (next !== current) {
    current = next;
    setVar(current);
    persist(current);
  }
});

export function onFabPointerDown(e: PointerEvent): void {
  if (e.pointerType === "mouse" && e.button !== 0) return;
  const startY = e.clientY;
  const startX = e.clientX;
  const base = current;
  // Snapshot once at pointerdown — innerHeight only changes on rotate/resize,
  // and the resize listener re-clamps on those. No per-pointermove layout read.
  const maxY = Math.max(0, window.innerHeight - CLUSTER_H);
  let armed = false;

  // rAF-coalesce the var write: one compositor transform per frame, not one
  // per pointermove. Mirrors PaneDivider.tsx — the smooth drag pattern. On
  // touch, pointermove fires at high frequency and uncoalesced writes queue up
  // behind main-thread cell rendering → the cluster visually trails the thumb.
  let last = current;
  let rafId = 0;
  const flush = (): void => {
    rafId = 0;
    current = last;
    setVar(current);
  };

  const onMove = (ev: PointerEvent): void => {
    if (!armed && !dragArmed({ x: startX, y: startY }, ev.clientX, ev.clientY)) return;
    if (!armed) {
      armed = true;
      // Promote FAB layers to compositor surfaces for the drag only (mirrors
      // the sidebar [data-swiping="1"] gating — permanent will-change kept
      // every FAB on its own layer across all mounted sessions and janked the
      // drag). Set BEFORE scheduling the first rAF so promotion is in place
      // when the first translate write lands.
      document.documentElement.setAttribute("data-fab-dragging", "true");
    }
    // drag UP (clientY decreases) => larger upward offset
    last = Math.min(maxY, Math.max(0, base + (startY - ev.clientY)));
    if (!rafId) rafId = requestAnimationFrame(flush);
  };

  const onUp = (): void => {
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    if (!armed) return; // was a tap — let the FAB's own onClick fire; no var write
    document.documentElement.removeAttribute("data-fab-dragging");
    // Commit the most recent target (a frame may have been pending at release)
    // so the released position is final and durable.
    current = last;
    setVar(current);
    persist(current);
    // Swallow the trailing click so the dragged FAB doesn't also toggle mic /
    // open the file picker after a reposition.
    window.addEventListener(
      "click",
      (ce) => {
        ce.stopPropagation();
        ce.preventDefault();
      },
      { capture: true, once: true },
    );
  };

  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}
