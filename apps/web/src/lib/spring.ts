// Damped-spring solver for drag-follow + settle motion (tab reorder, overscroll
// rubber-band, smooth scroll). Chrome/Chromium use physics springs for tab drag
// and overscroll; this is the reusable equivalent. The math is pure so the
// thresholds/step are unit-tested, mirroring deckSwipe.ts / edgeSwipeDrawer.ts.
// Physics runs in SECONDS; position in px, velocity in px/s.

export interface SpringConfig {
  stiffness: number; // k — pull toward target
  damping: number;   // c — resistance; c = 2*sqrt(k*m) is critical (no overshoot)
  mass: number;      // m
}
export interface SpringState {
  position: number; // px
  velocity: number; // px/s
}

// Rest thresholds: within this of target AND slow enough → settled, stop the loop.
export const SPRING_REST_POSITION = 0.1; // px
export const SPRING_REST_VELOCITY = 1;   // px/s

// Critical damping coefficient for a given stiffness/mass — the fastest settle
// with no overshoot. damping above → overdamped (sluggish), below → bouncy.
export function criticalDamping(stiffness: number, mass = 1): number {
  return 2 * Math.sqrt(stiffness * mass);
}

// Semi-implicit (symplectic) Euler step toward `target`. dtMs is the frame delta
// in ms (clamp caller-side to avoid instability on tab-switch stalls). Stable at
// rAF-scale dt. Returns the next state; caller loops until isSpringAtRest.
export function springStep(
  state: SpringState,
  target: number,
  cfg: SpringConfig,
  dtMs: number,
): SpringState {
  const dt = Math.max(0, dtMs) / 1000;
  if (dt === 0) return { position: state.position, velocity: state.velocity };
  const x = state.position - target; // displacement from target
  const accel = (-cfg.stiffness * x - cfg.damping * state.velocity) / cfg.mass;
  const velocity = state.velocity + accel * dt;
  const position = state.position + velocity * dt;
  return { position, velocity };
}

// Settled when close to target and nearly stopped.
export function isSpringAtRest(state: SpringState, target: number): boolean {
  return (
    Math.abs(state.position - target) < SPRING_REST_POSITION &&
    Math.abs(state.velocity) < SPRING_REST_VELOCITY
  );
}

// Chrome-ish presets. SNAP: crisp tab-reorder/settle (slightly underdamped for
// a hint of life). GENTLE: overscroll rubber-band release. STIFF: smooth-scroll.
export const SPRING_SNAP: SpringConfig = { stiffness: 700, damping: 45, mass: 1 };
export const SPRING_GENTLE: SpringConfig = { stiffness: 300, damping: 30, mass: 1 };
export const SPRING_STIFF: SpringConfig = { stiffness: 900, damping: 60, mass: 1 };

// rAF driver over springStep. onFrame receives each position; resolves once at
// rest (snapped exactly to target). Returns a cancel fn. Impure (rAF/perf) so it
// lives beside — but outside — the tested pure core. Callers under reduced-motion
// should skip this and jump to target directly (see lib/prefersReducedMotion.ts).
export function animateSpring(
  from: SpringState,
  target: number,
  cfg: SpringConfig,
  onFrame: (position: number) => void,
  onDone?: () => void,
): () => void {
  let state = from;
  let raf = 0;
  let last = performance.now();
  let cancelled = false;
  const tick = (now: number): void => {
    if (cancelled) return;
    const dtMs = Math.min(now - last, 64); // clamp to ~4 frames after a stall
    last = now;
    state = springStep(state, target, cfg, dtMs);
    if (isSpringAtRest(state, target)) {
      onFrame(target);
      onDone?.();
      return;
    }
    onFrame(state.position);
    raf = requestAnimationFrame(tick);
  };
  raf = requestAnimationFrame(tick);
  return () => {
    cancelled = true;
    cancelAnimationFrame(raf);
  };
}
