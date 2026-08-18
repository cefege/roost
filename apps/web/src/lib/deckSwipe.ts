// Commit-to-switch decision + momentum settle timing for the mobile tab-bar
// swipe (wired in TerminalDeck, gesture measured in MobileDeckBar). Pure so the
// thresholds are unit-tested. Mirrors edgeSwipeDrawer.ts. dir: 1 = next, -1 =
// prev (deck feeds the real finger dx, so dir 1 = finger-left, dir -1 = finger-right).
// The swipe-slot presentation (Swipe state shape + every style the deck paints
// from it) lives at the bottom of this file.

import type { Rect } from "../store/paneLayout.ts";

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

// ── Mobile swipe-to-switch presentation ───────────────────────────────────
// The gesture's state shape plus every style the deck paints from it. Moved
// out of TerminalDeck — which keeps arm/track/end and the DOM listeners — so
// the mapping stays pure: the deck threads its live swipe state, deck width,
// and tab-strip height in.
type SwipePhase = "track" | "settle";
export type Swipe = {
  phase: SwipePhase;
  currentId: string;          // URL-active session at swipe start
  neighborId: string | null;  // null → at an end (new-terminal / workspace affordance)
  dir: 1 | -1;                 // 1 = finger-left → next; -1 = finger-right → prev
  offset: number;              // live finger dx (px), clamped to [-w, w]
  mode: SwipeMode;             // slide (real neighbor) | new-terminal | workspace
  settleTarget?: "commit" | "cancel"; // set only in endSwipe → keys the settle geometry
  settleMs?: number;           // per-settle duration (momentum); undefined during track
};

// Emphasized-decelerate easing for the swipe settle (shared by slot transform +
// end-affordance placeholder). Mirrors the M3 token used across the mobile deck.
const SWIPE_DECEL = "var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))";
// Tab-switch slide settle — Chromium ToolbarSwipeLayout animates the offset with
// Android's DecelerateInterpolator(1.0) = 1-(1-t)² (easeOutQuad). Gentler than the
// M3 emphasized-decelerate SWIPE_DECEL, which front-loads ~70% of travel into the
// first ~15% of time and hid the swipe direction. Slide only; affordances keep SWIPE_DECEL.
const SWIPE_SLIDE_EASE = "cubic-bezier(0.25, 0.46, 0.45, 0.94)";
export const NEW_BLOOM_MS = 300;       // container-transform reveal (M3 emphasized medium2)

// ── Swipe transform per terminal slot ─────────────────────────────────
// Composed AFTER termStyle (which never sets transform) so the slot's base
// geometry is intact. During track: no transition (finger-follow). During
// settle: constant-speed slide (settleDurationMs = 500ms per screen-width) with a
// decelerate ease-out so a full traverse never blinks past and its direction reads.
export function swipeOffsetsPx(sw: Swipe, w: number): { current: number; neighbor: number } {
  if (sw.phase === "settle")
    return sw.settleTarget === "commit"
      ? { current: -sw.dir * w, neighbor: 0 }
      : { current: 0, neighbor: sw.dir * w };
  return { current: sw.offset, neighbor: sw.offset + sw.dir * w };
}
export function swipeStyleFor(sw: Swipe | null, id: string, w: number): Record<string, string> {
  if (!sw || (id !== sw.currentId && id !== sw.neighborId)) return {};
  const transition = sw.phase === "settle" ? `transform ${sw.settleMs ?? 200}ms ${SWIPE_SLIDE_EASE}` : "none";
  const o = swipeOffsetsPx(sw, w);
  if (id === sw.currentId) {
    if (sw.mode === "new-terminal") {
      const p = sw.phase === "settle" ? (sw.settleTarget === "commit" ? 1 : 0) : newFabProgress(sw.offset, w);
      const { scale, shiftFrac, radius } = peekCard(p);
      const ms = sw.settleMs ?? 200;
      return {
        transform: `translateX(${shiftFrac * w}px) scale(${scale})`,
        "transform-origin": "center center",
        "border-radius": `${radius}px`,
        overflow: "hidden",
        "box-shadow": p > 0 ? `0 ${Math.round(8 * p)}px ${Math.round(30 * p)}px color-mix(in srgb, var(--md-shadow) ${Math.round(50 * p)}%, transparent)` : "none",
        transition: sw.phase === "settle"
          ? `transform ${ms}ms ${SWIPE_DECEL}, border-radius ${ms}ms ${SWIPE_DECEL}, box-shadow ${ms}ms ${SWIPE_DECEL}`
          : "none",
      };
    }
    if (sw.mode === "workspace") return {};
    return { transform: `translateX(${o.current}px)`, transition };
  }
  if (!sw.neighborId) return {}; // new-terminal/workspace: no session neighbor slot (placeholder owns that side)
  return { transform: `translateX(${o.neighbor}px)`, transition };
}

// The neighbor terminal's own full bar rides in as a distinct card during a real
// neighbor slide (Chrome-mobile whole-tab slide). null for new-terminal / workspace
// end affordances and off-compact. slide mode always has a real neighbor.
export function barNeighborId(sw: Swipe | null, compact: boolean): string | null {
  if (!sw || !compact || sw.mode !== "slide") return null;
  return sw.neighborId;
}

// Behind-surface that the shrinking terminal reveals (predictive-back peek).
// rect = the single compact pane's rect; stripH = the deck bar height above it.
export function newPeekStyle(sw: Swipe | null, rect: Rect | undefined, w: number, stripH: number): Record<string, string> {
  if (!sw || sw.mode !== "new-terminal") return { display: "none" };
  if (!rect || w <= 0) return { display: "none" };
  const p = newFabProgress(sw.offset, w);
  const committing = sw.phase === "settle" && sw.settleTarget === "commit";
  const settling = sw.phase === "settle";
  return {
    position: "absolute", left: "0px", top: `${rect.y + stripH}px`,
    width: `${rect.w}px`, height: `${Math.max(0, rect.h - stripH)}px`, "z-index": "1",
    opacity: committing ? "1" : settling ? "0" : `${Math.min(1, p)}`,
    transition: settling ? `opacity ${sw.settleMs ?? NEW_BLOOM_MS}ms ${SWIPE_DECEL}` : "none",
  };
}
// Circular + FAB that grows under the finger and container-transforms to full deck on commit.
export function newFabStyle(sw: Swipe | null, rect: Rect | undefined, w: number, stripH: number): Record<string, string> {
  if (!sw || sw.mode !== "new-terminal") return { display: "none" };
  if (!rect || w <= 0) return { display: "none" };
  const areaTop = rect.y + stripH;
  const areaH = Math.max(0, rect.h - stripH);
  const committing = sw.phase === "settle" && sw.settleTarget === "commit";
  const settling = sw.phase === "settle";
  const p = newFabProgress(sw.offset, w);
  const FAB = 56;
  if (committing) {
    return {
      position: "absolute", left: "0px", top: `${areaTop}px`,
      width: `${rect.w}px`, height: `${areaH}px`, "border-radius": "0px",
      transform: "scale(1)", opacity: "1", "z-index": "6",
      transition: `left ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, top ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, width ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, height ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, border-radius ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}`,
    };
  }
  const ms = sw.settleMs ?? 200;
  return {
    position: "absolute", left: `${rect.w - 20 - FAB}px`, top: `${areaTop + areaH / 2 - FAB / 2}px`,
    width: `${FAB}px`, height: `${FAB}px`, "border-radius": "50%",
    transform: `scale(${settling ? NEW_FAB_MIN_SCALE : newFabScale(p)})`, "transform-origin": "center center",
    opacity: settling ? "0" : `${Math.min(1, p * 1.4)}`, "z-index": "6",
    transition: settling ? `transform ${ms}ms ${SWIPE_DECEL}, opacity ${ms}ms ${SWIPE_DECEL}` : "none",
  };
}
