// Commit-to-switch decision + momentum settle timing for the mobile tab-bar
// swipe (wired in TerminalDeck, gesture measured in MobileDeckBar). Pure so the
// thresholds are unit-tested. Mirrors edgeSwipeDrawer.ts. dir: 1 = swipe
// left→next (leftward, dx<0), -1 = swipe right→prev (rightward, dx>0).

export const SWITCH_DIST_FRAC = 0.4;       // deliberate drag: travel >= 40% width
export const SWITCH_FLING_VEL = 0.6;       // px/ms, directional flick
export const SWITCH_FLING_MIN_FRAC = 0.12; // a flick must have moved >= 12% width
export const SETTLE_MIN_MS = 180;
export const SETTLE_MAX_MS = 340;

// Commit only when the release is IN the armed direction AND either a real
// distance drag OR a directional flick past a small travel floor. The travel
// floor + direction check are what stop a weak swipe (tiny drag, incidental
// end-of-touch velocity spike, or a backward flick) from switching.
export function shouldCommitSwitch(
  dx: number,
  velocity: number,
  dir: 1 | -1,
  width: number,
): boolean {
  const releaseDir: 1 | -1 = dx < 0 ? 1 : -1;
  if (releaseDir !== dir) return false;
  const travel = Math.abs(dx);
  const distOk = travel >= width * SWITCH_DIST_FRAC;
  const flingOk =
    -dir * velocity >= SWITCH_FLING_VEL && travel >= width * SWITCH_FLING_MIN_FRAC;
  return distOk || flingOk;
}

// Settle continues the release motion instead of a fixed snap: a fast flick
// finishes briskly, a slow release settles gently, both clamped so it never
// snaps instantly (too fast) nor drags (laggy). remaining = px the slot still
// travels; releaseVel = px/ms at release (sign ignored).
export function settleDurationMs(remaining: number, releaseVel: number): number {
  const v = Math.abs(releaseVel);
  const raw = v > 0 ? remaining / v : SETTLE_MAX_MS;
  return Math.max(SETTLE_MIN_MS, Math.min(SETTLE_MAX_MS, raw));
}

// What a swipe at an end of the tab list becomes. hasNeighbor: is there a real
// adjacent tab in the armed direction? With a neighbor it's a plain tab slide.
// At an end the affordance depends on direction: forward (dir 1, finger-left)
// spawns a new terminal, backward (dir -1, finger-right) opens the workspace
// drawer.
export type SwipeMode = "slide" | "new-terminal" | "workspace";
export function endMode(dir: 1 | -1, hasNeighbor: boolean): SwipeMode {
  if (hasNeighbor) return "slide";
  return dir === 1 ? "new-terminal" : "workspace";
}

// Corner-FAB fill for the new-terminal pull: 0 at rest → 1 once the finger has
// travelled the commit distance (SWITCH_DIST_FRAC of width), the point at which
// the affordance reads "armed" and a release commits by distance. Clamped [0,1];
// a fling can still commit below 1 (shouldCommitSwitch), the FAB just won't have
// shown armed — same as a flung tab-switch that commits under full travel.
export function newFabProgress(offset: number, width: number): number {
  if (width <= 0) return 0;
  return Math.min(1, Math.abs(offset) / (width * SWITCH_DIST_FRAC));
}
