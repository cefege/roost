// ui-cc pure core — UiCommand → Layout mapping with ZERO side-effect imports.
// Split from lib/uiCommandDispatch.ts so tests (and any future consumer) can
// load the mapping without dragging in paneLayoutStore/rootStore/connect:
// paneLayoutStore registers a pagehide flush at import time, and bun's shared
// module cache means whichever test file loads it first wins — a static
// handleUiCommand import from a test poisoned paneLayoutStore.test.ts's
// window-stub capture (full-suite order coupling). Everything here is pure:
// paneLayout.ts tree ops + paneLayoutPresets.ts, no DOM, no store, no client.

import type { UiCommand, UiCommandFrame } from "@roost/shared/proto/sync_pb";
import {
  selectTab, focusPane, moveTab, splitLeaf, findLeafOfTab,
  type Layout, type PaneDir,
} from "../store/paneLayout.ts";
import { arrangeLayout, type ArrangeKind } from "../store/paneLayoutPresets.ts";

const ARRANGE_KINDS: Record<string, true> = { "even": true, "rows": true, "tiled": true, "main-vertical": true, "balance": true };

/** Tab targeting: empty targetTabId = broadcast (every tab accepts); set =
 *  only the addressed tab executes. The shell passes its own getTabId(). */
export function frameAccepted(frame: UiCommandFrame, ownTabId: string): boolean {
  return !frame.targetTabId || frame.targetTabId === ownTabId;
}

/** Apply one layout-shaped UiCommand to a Layout. Returns the next Layout, or
 *  null when the command can't apply (session/anchor not in the tree, bad dir
 *  or preset) — the shell drops null with a warn. navigate/closeTab/spotlight
 *  are NOT layout transforms and live in the shell. `liveIds` feeds arrange's
 *  rebuild presets (one pane per live session). */
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
    default:
      return null;
  }
}
