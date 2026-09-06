// Owns the terminal deck decisions that may persist route or rendered-pane focus.
// The model/operations supply resolved layouts and their mutation boundaries.
// Focused-empty compact projection stays view-only to preserve desktop state.

import {
  findLeaf,
  findLeafOfTab,
  selectTab,
  type Layout,
} from "../store/paneLayout.ts";

function projectsCompactFromEmptyFocus(
  current: Layout | null,
  compact: boolean,
): boolean {
  if (!current || !compact) return false;
  return findLeaf(current.root, current.focusedPaneId)?.tabs.length === 0;
}

export function syncDeckRouteSelection(
  current: Layout,
  activeSessionId: string,
  compact: boolean,
  commit: (next: Layout) => void,
): void {
  if (projectsCompactFromEmptyFocus(current, compact)) return;
  const leaf = findLeafOfTab(current.root, activeSessionId);
  if (!leaf) return;
  if (current.focusedPaneId === leaf.paneId && leaf.selectedTab === activeSessionId) return;
  commit(selectTab(current, activeSessionId));
}

export function syncDeckPaneFocus(
  current: Layout | null,
  renderedPaneId: string,
  compact: boolean,
  persistFocus: () => void,
): void {
  if (!current) return;
  if (projectsCompactFromEmptyFocus(current, compact)) return;
  if (compact && current.focusedPaneId !== renderedPaneId) return;
  persistFocus();
}

export function syncDeckSessionSelection(
  current: Layout | null,
  compact: boolean,
  persistSelection: () => void,
  navigateOnly: () => void,
): void {
  if (projectsCompactFromEmptyFocus(current, compact)) {
    navigateOnly();
    return;
  }
  persistSelection();
}
