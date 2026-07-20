// FLIP move-animation for a grid whose children carry data-flip-key. Call after
// each commit; pass the prior rects, get the new rects back. Survivors present in
// both maps translate from old→new slot over durationMs. Respects reduced-motion.
export function flipGrid(
  container: HTMLElement,
  prev: Map<string, DOMRect>,
  durationMs = 250,
  easing = "var(--md-sys-motion-easing-emphasized, cubic-bezier(0.2,0,0,1))",
): Map<string, DOMRect> {
  const next = new Map<string, DOMRect>();
  const nodes = container.querySelectorAll<HTMLElement>("[data-flip-key]");
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  for (const el of nodes) {
    const key = el.dataset.flipKey!;
    const last = el.getBoundingClientRect();
    next.set(key, last);
    const first = prev.get(key);
    if (!first || reduce) continue;
    const dx = first.left - last.left;
    const dy = first.top - last.top;
    if (!dx && !dy) continue;
    el.style.transition = "none";
    el.style.transform = `translate(${dx}px, ${dy}px)`;
    // next frame: release to identity with a transition → slides to new slot
    requestAnimationFrame(() => requestAnimationFrame(() => {
      el.style.transition = `transform ${durationMs}ms ${easing}`;
      el.style.transform = "";
    }));
  }
  return next;
}
