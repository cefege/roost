// JS accessor for the OS "reduce motion" setting. The global CSS guard
// (theme-vars.css) neutralizes CSS animation/transition durations, but
// JS-driven motion — View Transitions, WAAPI, the spring rAF driver — bypasses
// CSS entirely and MUST check this before animating (jump to the end state
// instead). One-shot read; SSR / no-matchMedia → false.

export function prefersReducedMotion(): boolean {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}
