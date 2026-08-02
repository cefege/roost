// Chrome/Android-style elastic overscroll for a touch scroll container: pulling
// past an edge rubber-bands the content (translateY with resistance) and lights
// an edge glow, then springs back on release. Engages ONLY at a scroll boundary
// while pulling further, so it never hijacks normal scroll or trackpad momentum.
// The transform lands on the scroll element's first child (the content); the
// glow is a `data-overscroll="top|bottom"` hook for CSS. Reduced motion or a
// missing child → no-op. Returns a cleanup fn. Mirrors the pointer-gesture
// convention of drawerDrag.ts / edgeSwipeDrawer.ts.

import { prefersReducedMotion } from "./prefersReducedMotion.ts";
import { animateSpring, SPRING_GENTLE } from "./spring.ts";

const MAX_PULL = 80;    // px cap on the rubber-band stretch
const RESISTANCE = 0.35; // fraction of raw finger travel applied past the edge

export function attachElasticOverscroll(scrollEl: HTMLElement): () => void {
  // No rubber-band at all under reduced motion — the pull itself is the motion,
  // and with nothing stretched there is no release to spring back. animateSpring
  // gates the release independently, so a future relaxation here cannot animate.
  if (prefersReducedMotion()) return () => {};
  // The content element (transform target) is resolved lazily, NOT at attach
  // time: this runs from a Solid `ref`, which fires before the aside's children
  // (SidebarRoot → .df-all-view) are appended, so firstElementChild is null
  // here. Resolve it when a gesture actually begins.
  let content: HTMLElement | null = null;
  const resolveContent = (): HTMLElement | null =>
    (content ??= scrollEl.firstElementChild as HTMLElement | null);

  let startY = 0;
  let pulling = false; // armed: a boundary pull is in progress
  let offset = 0;
  let cancelSpring: (() => void) | undefined;

  const apply = (next: number): void => {
    const el = resolveContent();
    if (!el) return;
    offset = next;
    el.style.transform = offset ? `translateY(${offset}px)` : "";
    if (offset > 0.5) scrollEl.setAttribute("data-overscroll", "top");
    else if (offset < -0.5) scrollEl.setAttribute("data-overscroll", "bottom");
    else scrollEl.removeAttribute("data-overscroll");
  };

  const onTouchStart = (e: TouchEvent): void => {
    if (e.touches.length !== 1) return;
    cancelSpring?.();
    startY = e.touches[0].clientY;
    pulling = false;
  };

  const onTouchMove = (e: TouchEvent): void => {
    if (e.touches.length !== 1) return;
    const y = e.touches[0].clientY;
    const atTop = scrollEl.scrollTop <= 0;
    const atBottom = scrollEl.scrollTop + scrollEl.clientHeight >= scrollEl.scrollHeight - 1;
    if (!pulling) {
      // Arm only when pulling further past the edge the finger is already at,
      // and rebase startY to HERE so the rubber-band grows from 0 — not from the
      // pre-boundary travel native scroll already consumed.
      const dyRaw = y - startY;
      if ((dyRaw > 0 && atTop) || (dyRaw < 0 && atBottom)) { pulling = true; startY = y; }
      else return;
    }
    // Rubber-band with fixed resistance; cap the stretch.
    const raw = (y - startY) * RESISTANCE;
    const clamped = Math.max(-MAX_PULL, Math.min(MAX_PULL, raw));
    e.preventDefault(); // hold the native scroll; we own the edge now
    apply(clamped);
  };

  const onTouchEnd = (): void => {
    pulling = false;
    if (offset === 0) return;
    cancelSpring?.();
    cancelSpring = animateSpring(
      { position: offset, velocity: 0 }, 0, SPRING_GENTLE,
      (pos) => apply(Math.abs(pos) < 0.5 ? 0 : pos),
    );
  };

  scrollEl.addEventListener("touchstart", onTouchStart, { passive: true });
  scrollEl.addEventListener("touchmove", onTouchMove, { passive: false });
  scrollEl.addEventListener("touchend", onTouchEnd, { passive: true });
  scrollEl.addEventListener("touchcancel", onTouchEnd, { passive: true });

  return () => {
    cancelSpring?.();
    scrollEl.removeEventListener("touchstart", onTouchStart);
    scrollEl.removeEventListener("touchmove", onTouchMove);
    scrollEl.removeEventListener("touchend", onTouchEnd);
    scrollEl.removeEventListener("touchcancel", onTouchEnd);
    apply(0);
  };
}
