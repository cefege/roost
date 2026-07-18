// Shared imperative drive of the mobile workspace drawer (.roost-drawer).
// Both the left-edge swipe (AppShell) and the deck's backward "workspace" swipe
// (TerminalDeck) drive the SAME real drawer element live, so the user sees the
// actual sidebar follow their finger instead of a placeholder. Transforms are
// written imperatively (file-header convention across the swipe sites); on
// settle we hand transform back to the .roost-drawer[data-open] CSS.
import { openSidebar, closeSidebar } from "../store/uiStore.ts";

const DECEL =
  "transform var(--md-sys-motion-duration-medium2, 300ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))";
const ACCEL =
  "transform var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-accelerate, cubic-bezier(0.3, 0, 0.8, 0.15))";

let drawerEl: HTMLElement | null = null;
export function registerDrawer(el: HTMLElement | null): void { drawerEl = el; }

// Live finger-follow: off = translateX px. Base closed = -width (off left).
export function dragDrawer(off: number): void {
  if (!drawerEl) return;
  drawerEl.style.transition = "none";
  drawerEl.style.transform = `translateX(${off}px)`;
}

function handoff(el: HTMLElement): void {
  let cleared = false;
  const clear = () => {
    if (cleared) return;
    cleared = true;
    el.style.transition = "none";
    el.style.transform = "";   // hand back to data-open CSS
    void el.offsetWidth;       // reflow so the snap is not animated
    el.style.transition = "";  // restore CSS transition for next gesture
  };
  el.addEventListener("transitionend", clear, { once: true });
  setTimeout(clear, 350);      // fallback: a zero-distance settle fires no transitionend
}

// Settle the OPEN gesture: commit → slide fully on-screen + openSidebar();
// cancel → spring back off the LEFT edge. Enter decelerates, exit accelerates.
export function settleDrawerOpen(commit: boolean): void {
  if (commit) openSidebar();
  const el = drawerEl;
  if (!el) return;
  el.style.transition = commit ? DECEL : ACCEL;
  el.style.transform = commit ? "translateX(0)" : "translateX(-100%)";
  handoff(el);
}

// Settle the CLOSE gesture: commit → slide off the RIGHT edge + closeSidebar();
// cancel → settle back open. Mirrors AppShell's prior close branch exactly.
export function settleDrawerClose(commit: boolean): void {
  if (commit) closeSidebar();
  const el = drawerEl;
  if (!el) return;
  el.style.transition = commit ? ACCEL : DECEL;
  el.style.transform = commit ? "translateX(100%)" : "translateX(0)";
  handoff(el);
}
