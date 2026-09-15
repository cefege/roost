// Edge-swipe gesture synthesis for compact workbench smoke coverage.
// It is called by the workbench shell spec through the interaction helper re-export.
// The helper dispatches browser-native touch events against the document body.
// Keeping it isolated lets the larger sidebar interaction fixture stay below the lint cap.

import type { Page } from "@playwright/test";

export async function swipeFromEdge(page: Page, startX: number, endX: number, y: number): Promise<void> {
  await page.evaluate(({ startX: initialX, endX: finalX, y: clientY }) => {
    const target = document.body;
    const touch = (clientX: number) => new Touch({
      identifier: 1,
      target,
      clientX,
      clientY,
      screenX: clientX,
      screenY: clientY,
      pageX: clientX,
      pageY: clientY,
    });
    target.dispatchEvent(new TouchEvent("touchstart", {
      bubbles: true,
      cancelable: true,
      changedTouches: [touch(initialX)],
      touches: [touch(initialX)],
    }));
    target.dispatchEvent(new TouchEvent("touchmove", {
      bubbles: true,
      cancelable: true,
      changedTouches: [touch(finalX)],
      touches: [touch(finalX)],
    }));
    target.dispatchEvent(new TouchEvent("touchend", {
      bubbles: true,
      cancelable: true,
      changedTouches: [touch(finalX)],
      touches: [],
    }));
  }, { startX, endX, y });
}
