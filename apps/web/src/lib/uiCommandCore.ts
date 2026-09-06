// Pure mapping from the eight legacy UiCommands to browser-local layouts.
// Keeping store, transport, and DOM imports out makes this safe to test without
// triggering paneLayoutStore's page-lifecycle side effects. Acknowledged layout
// apply is explicitly refused here and owned by uiLayoutApply instead.

import type { UiCommand, UiCommandFrame } from "@roost/shared/proto/sync_pb";
import {
  selectTab, focusPane, moveTab, splitLeaf, findLeafOfTab,
  type Layout, type PaneDir,
} from "../store/paneLayout.ts";
import { arrangeLayout, type ArrangeKind } from "../store/paneLayoutPresets.ts";

const ARRANGE_KINDS: Record<string, true> = { "even": true, "rows": true, "tiled": true, "main-vertical": true, "balance": true };

/** Legacy targeting: an empty targetTabId broadcasts to every tab. The
 * acknowledged apply path never calls this predicate. */
export function frameAccepted(frame: UiCommandFrame, ownTabId: string): boolean {
  return !frame.targetTabId || frame.targetTabId === ownTabId;
}

/** Apply one legacy layout-shaped UiCommand to a Layout. Returns null for bad
 * references/arguments and for shell-owned navigate, closeTab, and spotlight.
 * applyLayout is also refused: only the exact-target acknowledged adapter may
 * execute it. `liveIds` feeds arrange's one-pane-per-live-session presets. */
export function applyUiCommandToLayout(layout: Layout, cmd: UiCommand, liveIds: string[]): Layout | null {
  const c = cmd.command;
  switch (c.case) {
    case "placeSplit": {
      const { sessionId, anchorSessionId, dir, insertFirst } = c.value;
      const anchor = findLeafOfTab(layout.root, anchorSessionId);
      if (!sessionId || !anchor || (dir !== "row" && dir !== "col")) return null;
      return splitLeaf(layout, anchor.paneId, dir as PaneDir, sessionId, insertFirst);
    }
    case "selectTab":
      return findLeafOfTab(layout.root, c.value.sessionId)
        ? selectTab(layout, c.value.sessionId)
        : null;
    case "focusPane": {
      // Command addresses the pane by a session it CONTAINS (proto contract).
      const leaf = findLeafOfTab(layout.root, c.value.sessionId);
      return leaf ? focusPane(layout, leaf.paneId) : null;
    }
    case "moveTab": {
      const { sessionId, destSessionId } = c.value;
      const dest = findLeafOfTab(layout.root, destSessionId);
      if (!dest || !findLeafOfTab(layout.root, sessionId)) return null;
      return moveTab(layout, sessionId, dest.paneId);
    }
    case "arrange":
      return ARRANGE_KINDS[c.value.preset]
        ? arrangeLayout(c.value.preset as ArrangeKind, layout, liveIds)
        : null;
    case "applyLayout":
      return null;
    default:
      return null;
  }
}
