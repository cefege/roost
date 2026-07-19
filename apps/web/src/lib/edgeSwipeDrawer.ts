// Pure decision helpers for the mobile left-edge swipe-to-open drawer gesture
// (wired in AppShell). Kept side-effect-free so the geometry is unit-tested and
// AppShell stays declarative. Constants mirror the existing in-app swipe sites:
// axis-lock gate/ratio from MobileDeckBar, commit thresholds from TerminalDeck.

// A touch must START within this many px of the left edge to be a candidate.
export const EDGE_PX = 24;
// Axis-lock travel gate (matches MobileDeckBar).
export const ARM_PX = 10;
// Horizontal wins only if |dx| > |dy| * AXIS_RATIO (matches MobileDeckBar).
export const AXIS_RATIO = 1.5;

// Which axis the gesture locks to, or "none" while still under the arm gate.
export function lockAxis(dx: number, dy: number): "none" | "x" | "y" {
  if (Math.abs(dx) < ARM_PX && Math.abs(dy) < ARM_PX) return "none";
  return Math.abs(dx) > Math.abs(dy) * AXIS_RATIO ? "x" : "y";
}

// The drawer's live translateX in px from its base off-screen position. Base
// closed = -width. Rightward drag advances toward 0; range [-width, 0].
// Leftward (dx<0) clamps to -width (stays closed).
export function openOffsetPx(dx: number, width: number): number {
  return -width + Math.max(0, Math.min(dx, width));
}

// Commit-to-open decision on release: past 30% width OR a rightward flick.
// Rightward-positive dx/velocity only — negative never opens.
export function shouldOpen(dx: number, velocity: number, width: number): boolean {
  return dx >= width * 0.3 || velocity >= 0.8;
}

// The drawer's live translateX in px while dragging it CLOSED. Base open = 0.
// Leftward drag (dx<0) advances toward -width (off the LEFT edge — the close
// direction chosen for the workspace drawer). Rightward (dx>0) clamps to 0 (stays open).
export function closeOffsetPx(dx: number, width: number): number {
  return Math.min(0, Math.max(dx, -width));
}

// Commit-to-close on release: past 30% width left OR a leftward flick.
// Leftward-negative thresholds (mirror of shouldOpen's rightward-positive).
export function shouldClose(dx: number, velocity: number, width: number): boolean {
  return dx <= -width * 0.3 || velocity <= -0.8;
}
