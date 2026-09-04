// Centralizes terminal deck geometry equality and slot style calculations.
// The reactive deck model uses these rules to avoid remounting unchanged panes
// and to park hidden renderers at their truthful reveal size. Rendering consumes
// the resulting style without owning any layout policy.

import type { PaneView, Rect } from "../store/paneLayout.ts";

export const TERMINAL_STRIP_HEIGHT = 40;
export const MOBILE_TERMINAL_STRIP_HEIGHT = 48;

export interface TerminalSessionSlot {
  rect: Rect;
  paneId: string;
  focused: boolean;
  spotlit?: boolean;
}

export function sameTerminalSessionSlot(
  left: TerminalSessionSlot | null,
  right: TerminalSessionSlot | null,
): boolean {
  if (left === right) return true;
  if (!left || !right) return false;
  return left.paneId === right.paneId
    && left.focused === right.focused
    && !!left.spotlit === !!right.spotlit
    && left.rect.x === right.rect.x
    && left.rect.y === right.rect.y
    && left.rect.w === right.rect.w
    && left.rect.h === right.rect.h;
}

export function sameTerminalPaneView(left: PaneView, right: PaneView): boolean {
  return left.paneId === right.paneId
    && left.selectedTab === right.selectedTab
    && left.tabIds.length === right.tabIds.length
    && left.tabIds.every((id, idx) => id === right.tabIds[idx]);
}

export function sameTerminalParkSizes(
  left: ReadonlyMap<string, { w: number; h: number }>,
  right: ReadonlyMap<string, { w: number; h: number }>,
): boolean {
  if (left.size !== right.size) return false;
  for (const [id, size] of left) {
    const candidate = right.get(id);
    if (!candidate || candidate.w !== size.w || candidate.h !== size.h) return false;
  }
  return true;
}

export function terminalSessionStyle(
  slot: TerminalSessionSlot | null,
  park: { w: number; h: number } | undefined,
  deckSize: { w: number; h: number },
  stripHeight: number,
): Record<string, string> {
  if (!slot) {
    // A parked renderer stays laid out at its future viewport size so its scroll
    // maximum cannot move while canonical frames continue to arrive.
    const fallbackWidth = park?.w ?? (deckSize.w > 0 ? deckSize.w : 800);
    const fallbackHeight = park?.h
      ?? (deckSize.h > 0 ? Math.max(0, deckSize.h - stripHeight) : 600);
    return {
      position: "absolute",
      left: "-99999px",
      top: "0",
      width: `${fallbackWidth}px`,
      height: `${fallbackHeight}px`,
      visibility: "hidden",
      "pointer-events": "none",
    };
  }
  const rect = slot.rect;
  if (slot.spotlit) {
    return {
      position: "absolute",
      left: `${rect.x}px`,
      top: `${rect.y}px`,
      width: `${rect.w}px`,
      height: `${rect.h}px`,
      visibility: "inherit",
      "z-index": "9",
      overflow: "hidden",
      "border-radius": "12px",
    };
  }
  return {
    position: "absolute",
    left: `${rect.x}px`,
    top: `${rect.y + stripHeight}px`,
    width: `${rect.w}px`,
    height: `${Math.max(0, rect.h - stripHeight)}px`,
    visibility: "inherit",
    "z-index": slot.focused ? "2" : "1",
  };
}
