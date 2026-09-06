// Pins the terminal deck's focused-empty compact persistence boundary.
// Route sync, rendered-pane focus, and session selection stay view-only there.
// Callback counters prove compact use cannot overwrite imported desktop focus.

import { describe, expect, test } from "bun:test";
import type { Layout } from "../src/store/paneLayout.ts";
import {
  syncDeckPaneFocus,
  syncDeckRouteSelection,
  syncDeckSessionSelection,
} from "../src/lib/deckRouteSelection.ts";

function focusedEmptyLayout(): Layout {
  return {
    root: {
      kind: "split",
      id: "split",
      dir: "row",
      ratio: 0.5,
      a: { kind: "leaf", paneId: "empty", tabs: [], selectedTab: "" },
      b: { kind: "leaf", paneId: "live", tabs: ["s1"], selectedTab: "s1" },
    },
    focusedPaneId: "empty",
  };
}

describe("focused-empty compact deck selection", () => {
  test("keeps route, rendered focus, and tab selection view-only", () => {
    const layout = focusedEmptyLayout();
    let routeCommits = 0;
    syncDeckRouteSelection(layout, "s1", true, () => { routeCommits++; });
    let paneFocusCommits = 0;
    syncDeckPaneFocus(layout, "live", true, () => { paneFocusCommits++; });
    let selectionCommits = 0;
    let navigations = 0;
    syncDeckSessionSelection(
      layout,
      true,
      () => { selectionCommits++; },
      () => { navigations++; },
    );

    expect([routeCommits, paneFocusCommits, selectionCommits, navigations])
      .toEqual([0, 0, 0, 1]);
    expect(layout.focusedPaneId).toBe("empty");
  });
});
