// NotificationDock lift coverage — the dock must clear whichever bottom chrome
// is mounted, and that decision is the one thing a layout regression lands on.
// Exercises notificationDockLift from src/lib/notificationDockLift.ts.

import { describe, expect, test } from "bun:test";
import { notificationDockLift } from "../src/lib/notificationDockLift.ts";

describe("notificationDockLift", () => {
  test("clears the live viewport composer by its measured height", () => {
    expect(notificationDockLift({ composerActive: true, composerHeightPx: 96, compact: true }))
      .toBe("calc(var(--term-chat-dock-offset) + 96px + var(--md-space-2))");
  });

  test("rests on the compact dock offset when no composer is mounted", () => {
    expect(notificationDockLift({ composerActive: false, composerHeightPx: 0, compact: true }))
      .toBe("var(--term-chat-dock-offset)");
  });

  test("clears the status bar and soft keyboard on pointer layouts", () => {
    const lift = notificationDockLift({ composerActive: false, composerHeightPx: 0, compact: false });
    expect(lift).toContain("var(--workbench-statusbar-height)");
    expect(lift).toContain("max(var(--kb-offset), 0px)");
  });
});
