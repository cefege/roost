// Drag-arming gate for pointer drags. A drag arms once the pointer travels
// >= threshold px from its start IN ANY DIRECTION (Euclidean). PaneStrip's tab
// drag previously gated on HORIZONTAL delta only, so a straight-down split-drag
// left the 40px tab button before any x-delta accumulated and never armed —
// "drag-to-tile doesn't trigger" (feedback_for_recreates_node_kills_pointer_capture
// symptom class). Callers: PaneStrip.tsx onTabPointerDown.

export const DRAG_THRESHOLD_PX = 8;

export function dragArmed(
  start: { x: number; y: number },
  clientX: number,
  clientY: number,
  threshold = DRAG_THRESHOLD_PX,
): boolean {
  return Math.hypot(clientX - start.x, clientY - start.y) >= threshold;
}
