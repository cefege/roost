// Commit-to-switch decision + momentum settle timing for the mobile tab-bar
// swipe (wired in TerminalDeck, gesture measured in MobileDeckBar). Pure so the
// thresholds are unit-tested. Mirrors edgeSwipeDrawer.ts. dir: 1 = next, -1 =
// prev (deck feeds the real finger dx, so dir 1 = finger-left, dir -1 = finger-right).

export const SWITCH_DIST_FRAC = 0.4;       // deliberate drag: travel >= 40% width
export const SWITCH_FLING_VEL = 0.6;       // px/ms, directional flick
export const SWITCH_FLING_MIN_FRAC = 0.12; // a flick must have moved >= 12% width
export const ANIMATION_SPEED_SCREEN_MS = 500; // Chromium ToolbarSwipeLayout: ms per full screen-width of travel

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

// Chrome tab-grid swipe-to-dismiss (TabGridItemTouchHelperCallback). dp≈px on web.
export const CARD_DISMISS_PX = 144;   // Chrome swipe_to_dismiss_threshold (144dp)
export const CARD_FLING_VEL = 0.5;    // px/ms — directional flick escape velocity
export const CARD_FLING_MIN_PX = 24;  // flick floor so a jittery release doesn't dismiss

// Dismiss on a full-travel drag OR a directional flick past the floor. Both
// directions dismiss (Chrome START|END). vx must point the same way as dx.
export function shouldDismissCard(dx: number, vx: number): boolean {
  const travel = Math.abs(dx);
  if (travel >= CARD_DISMISS_PX) return true;
  return Math.abs(vx) >= CARD_FLING_VEL && travel >= CARD_FLING_MIN_PX && Math.sign(vx) === Math.sign(dx);
}

// Chrome onChildDraw: alpha = max(0.2, 1 - 0.8*|dX|/threshold). 1 at rest, 0.2 at threshold.
export function cardSwipeAlpha(dx: number): number {
  return Math.max(0.2, 1 - (0.8 * Math.abs(dx)) / CARD_DISMISS_PX);
}

// Constant-speed settle (Chromium ToolbarSwipeLayout): the slot always finishes
// at 500ms per screen-width regardless of release velocity, so a full traverse is
// never faster than ~500ms and its direction is always readable. remaining = px the
// slot still travels; width = viewport px.
export function settleDurationMs(remaining: number, width: number): number {
  if (width <= 0) return 0;
  return Math.round(ANIMATION_SPEED_SCREEN_MS * Math.abs(remaining) / width);
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

// Predictive-back peel + growing-FAB visuals for the new-terminal pull.
// p = newFabProgress(offset,width) ∈ [0,1]. Pure so the mapping is unit-tested;
// TerminalDeck composes these into inline styles.
export const PEEK_SCALE_MIN = 0.9;   // current terminal shrinks to 90% at armed
export const PEEK_SHIFT_FRAC = 0.05; // …and slides left up to 5% of width
export const PEEK_RADIUS_PX = 28;    // …corners round to 28px (M3 large)
export const NEW_FAB_MIN_SCALE = 0.5;

export function peekCard(p: number): { scale: number; shiftFrac: number; radius: number } {
  const c = Math.max(0, Math.min(1, p));
  return { scale: 1 - c * (1 - PEEK_SCALE_MIN), shiftFrac: -c * PEEK_SHIFT_FRAC || 0, radius: c * PEEK_RADIUS_PX };
}
export function newFabScale(p: number): number {
  const c = Math.max(0, Math.min(1, p));
  return NEW_FAB_MIN_SCALE + (1 - NEW_FAB_MIN_SCALE) * c;
}
