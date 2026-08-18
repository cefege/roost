// Mobile tab-bar swipe commit decision + momentum settle math, plus the slot
// presentation the deck paints from it (lib/deckSwipe.ts).

import { describe, test, expect } from "bun:test";
import { shouldCommitSwitch, settleDurationMs, endMode, newFabProgress, peekCard, newFabScale, PEEK_SCALE_MIN, PEEK_RADIUS_PX, shouldDismissCard, cardSwipeAlpha, swipeOffsetsPx, swipeStyleFor, barNeighborId, newPeekStyle, newFabStyle, NEW_BLOOM_MS, type Swipe } from "../src/lib/deckSwipe.ts";

// A tracking swipe with a real neighbor, half a 400px screen of travel to the
// left (dir 1 = next). Overridable so each case names only what it changes.
function track(over: Partial<Swipe> = {}): Swipe {
  return { phase: "track", currentId: "cur", neighborId: "nxt", dir: 1, offset: -200, mode: "slide", ...over };
}

describe("shouldCommitSwitch", () => {
  test("past-distance drag commits", () => {
    expect(shouldCommitSwitch(-170, 0, 1, 400)).toBe(true); // 170 >= 160 (40%)
  });
  test("just-under distance stays", () => {
    expect(shouldCommitSwitch(-159, 0, 1, 400)).toBe(false);
  });
  test("directional flick past floor commits", () => {
    expect(shouldCommitSwitch(-60, -0.7, 1, 400)).toBe(true); // 60 >= 48 (12%), 0.7 >= 0.6
  });
  test("flick under the travel floor stays (weak-swipe fix)", () => {
    expect(shouldCommitSwitch(-40, -2, 1, 400)).toBe(false); // 40 < 48
  });
  test("backward flick stays (direction fix)", () => {
    expect(shouldCommitSwitch(-60, 0.7, 1, 400)).toBe(false); // velocity opposite armed dir
  });
  test("reversed release direction never commits", () => {
    expect(shouldCommitSwitch(200, 0, 1, 400)).toBe(false);
  });
  test("prev-direction (dir -1) directional flick commits", () => {
    expect(shouldCommitSwitch(60, 0.7, -1, 400)).toBe(true);
  });
  test("prev-direction backward flick stays", () => {
    expect(shouldCommitSwitch(60, -0.7, -1, 400)).toBe(false);
  });
});

describe("settleDurationMs", () => {
  test("full-screen traverse → 500ms", () => {
    expect(settleDurationMs(400, 400)).toBe(500);
  });
  test("half-screen remaining → 250ms", () => {
    expect(settleDurationMs(200, 400)).toBe(250);
  });
  test("quarter-screen remaining → 125ms", () => {
    expect(settleDurationMs(100, 400)).toBe(125);
  });
  test("zero remaining → 0ms", () => {
    expect(settleDurationMs(0, 400)).toBe(0);
  });
  test("zero-width guard → 0", () => {
    expect(settleDurationMs(100, 0)).toBe(0);
  });
});

describe("endMode", () => {
  test("neighbor forward → slide", () => {
    expect(endMode(1, true)).toBe("slide");
  });
  test("neighbor backward → slide", () => {
    expect(endMode(-1, true)).toBe("slide");
  });
  test("end forward → new-terminal", () => {
    expect(endMode(1, false)).toBe("new-terminal");
  });
  test("end backward → workspace", () => {
    expect(endMode(-1, false)).toBe("workspace");
  });
});

describe("newFabProgress", () => {
  test("at rest → 0", () => {
    expect(newFabProgress(0, 400)).toBe(0);
  });
  test("at commit distance → 1", () => {
    expect(newFabProgress(-160, 400)).toBe(1);
  });
  test("half commit distance → 0.5", () => {
    expect(newFabProgress(-80, 400)).toBe(0.5);
  });
  test("past commit distance clamps to 1", () => {
    expect(newFabProgress(-320, 400)).toBe(1);
  });
  test("sign-independent (backward magnitude)", () => {
    expect(newFabProgress(80, 400)).toBe(0.5);
  });
  test("zero-width guard → 0", () => {
    expect(newFabProgress(-100, 0)).toBe(0);
  });
});

describe("peekCard", () => {
  test("at rest → identity", () => { expect(peekCard(0)).toEqual({ scale: 1, shiftFrac: 0, radius: 0 }); });
  test("armed → shrunk/shifted/rounded", () => { expect(peekCard(1)).toEqual({ scale: PEEK_SCALE_MIN, shiftFrac: -0.05, radius: PEEK_RADIUS_PX }); });
  test("clamps past 1", () => { expect(peekCard(2)).toEqual(peekCard(1)); });
});
describe("newFabScale", () => {
  test("rest → 0.5", () => { expect(newFabScale(0)).toBe(0.5); });
  test("armed → 1", () => { expect(newFabScale(1)).toBe(1); });
  test("clamps", () => { expect(newFabScale(-1)).toBe(0.5); expect(newFabScale(3)).toBe(1); });
});

describe("shouldDismissCard", () => {
  test("just-under 144px travel stays", () => {
    expect(shouldDismissCard(143, 0)).toBe(false);
  });
  test("at 144px travel dismisses", () => {
    expect(shouldDismissCard(144, 0)).toBe(true);
  });
  test("fast flick under threshold dismisses", () => {
    expect(shouldDismissCard(30, 0.6)).toBe(true);
  });
  test("flick opposite the drag stays", () => {
    expect(shouldDismissCard(30, -0.6)).toBe(false);
  });
  test("fast flick below the travel floor stays", () => {
    expect(shouldDismissCard(10, 0.9)).toBe(false);
  });
});

describe("cardSwipeAlpha", () => {
  test("at rest → fully opaque", () => {
    expect(cardSwipeAlpha(0)).toBe(1);
  });
  test("at threshold → ~0.2", () => {
    expect(cardSwipeAlpha(144)).toBeCloseTo(0.2, 10);
  });
  test("past threshold clamps to 0.2", () => {
    expect(cardSwipeAlpha(1000)).toBe(0.2);
  });
});

describe("swipeOffsetsPx", () => {
  test("track: current follows the finger, neighbor trails one width behind", () => {
    expect(swipeOffsetsPx(track(), 400)).toEqual({ current: -200, neighbor: 200 });
  });
  test("settle commit: current lands a full width off in the swipe dir, neighbor at rest", () => {
    expect(swipeOffsetsPx(track({ phase: "settle", settleTarget: "commit" }), 400))
      .toEqual({ current: -400, neighbor: 0 });
  });
  test("settle cancel: current springs back, neighbor returns off-edge", () => {
    expect(swipeOffsetsPx(track({ phase: "settle", settleTarget: "cancel" }), 400))
      .toEqual({ current: 0, neighbor: 400 });
  });
  test("backward swipe mirrors both offsets", () => {
    expect(swipeOffsetsPx(track({ dir: -1, offset: 200 }), 400)).toEqual({ current: 200, neighbor: -200 });
  });
});

describe("swipeStyleFor", () => {
  test("no swipe → no transform at all (slot keeps its termStyle geometry)", () => {
    expect(swipeStyleFor(null, "cur", 400)).toEqual({});
  });
  test("an uninvolved slot is untouched", () => {
    expect(swipeStyleFor(track(), "other", 400)).toEqual({});
  });
  test("slide: finger-follow with no transition while tracking", () => {
    expect(swipeStyleFor(track(), "cur", 400)).toEqual({ transform: "translateX(-200px)", transition: "none" });
    expect(swipeStyleFor(track(), "nxt", 400)).toEqual({ transform: "translateX(200px)", transition: "none" });
  });
  test("slide settle: per-settle duration drives the transition", () => {
    const s = swipeStyleFor(track({ phase: "settle", settleTarget: "commit", settleMs: 250 }), "cur", 400);
    expect(s.transform).toBe("translateX(-400px)");
    expect(s.transition).toContain("250ms");
  });
  test("workspace mode leaves the terminal alone (the drawer moves instead)", () => {
    expect(swipeStyleFor(track({ mode: "workspace", neighborId: null }), "cur", 400)).toEqual({});
  });
  test("new-terminal mode peels the current card instead of sliding it", () => {
    // offset -160 over width 400 → newFabProgress 1 (400*0.4 = 160), i.e. armed:
    // scale PEEK_SCALE_MIN, shift -5% of width, radius PEEK_RADIUS_PX.
    const s = swipeStyleFor(track({ mode: "new-terminal", neighborId: null, offset: -160 }), "cur", 400);
    expect(s.transform).toBe(`translateX(-20px) scale(${PEEK_SCALE_MIN})`);
    expect(s["border-radius"]).toBe(`${PEEK_RADIUS_PX}px`);
    expect(s.overflow).toBe("hidden");
    expect(s.transition).toBe("none");
  });
  test("new-terminal at rest casts no shadow", () => {
    const s = swipeStyleFor(track({ mode: "new-terminal", neighborId: null, offset: 0 }), "cur", 400);
    expect(s["box-shadow"]).toBe("none");
  });
  test("an end-affordance swipe has no neighbor slot to move", () => {
    expect(swipeStyleFor(track({ mode: "new-terminal", neighborId: null }), "nxt", 400)).toEqual({});
  });
});

describe("barNeighborId", () => {
  test("slide on compact → the neighbor's own bar rides in", () => {
    expect(barNeighborId(track(), true)).toBe("nxt");
  });
  test("off-compact → no second bar", () => {
    expect(barNeighborId(track(), false)).toBe(null);
  });
  test("end affordances have no neighbor bar", () => {
    expect(barNeighborId(track({ mode: "new-terminal", neighborId: null }), true)).toBe(null);
    expect(barNeighborId(track({ mode: "workspace", neighborId: null }), true)).toBe(null);
  });
  test("no swipe → null", () => {
    expect(barNeighborId(null, true)).toBe(null);
  });
});

describe("newPeekStyle / newFabStyle", () => {
  const rect = { x: 0, y: 0, w: 400, h: 800 };
  const pull = (over: Partial<Swipe> = {}) => track({ mode: "new-terminal", neighborId: null, ...over });

  test("inert outside a new-terminal pull", () => {
    expect(newPeekStyle(null, rect, 400, 48)).toEqual({ display: "none" });
    expect(newFabStyle(track(), rect, 400, 48)).toEqual({ display: "none" });
  });
  test("unmeasured deck stays hidden rather than painting at 0×0", () => {
    expect(newPeekStyle(pull(), undefined, 400, 48)).toEqual({ display: "none" });
    expect(newFabStyle(pull(), rect, 0, 48)).toEqual({ display: "none" });
  });
  test("peek fills the terminal area below the deck bar and fades in with progress", () => {
    const s = newPeekStyle(pull({ offset: -80 }), rect, 400, 48);
    expect(s.top).toBe("48px");
    expect(s.height).toBe("752px");
    expect(s.opacity).toBe("0.5"); // newFabProgress(-80, 400) = 0.5
    expect(s.transition).toBe("none");
  });
  test("committing peek is fully opaque; cancelling settles to transparent", () => {
    expect(newPeekStyle(pull({ phase: "settle", settleTarget: "commit" }), rect, 400, 48).opacity).toBe("1");
    const cancel = newPeekStyle(pull({ phase: "settle", settleTarget: "cancel" }), rect, 400, 48);
    expect(cancel.opacity).toBe("0");
    expect(cancel.transition).toContain(`${NEW_BLOOM_MS}ms`);
  });
  test("FAB rests 20px inside the right edge, vertically centred in the area", () => {
    const s = newFabStyle(pull({ offset: -80 }), rect, 400, 48);
    expect(s.left).toBe("324px");        // 400 - 20 - 56
    expect(s.top).toBe("396px");         // 48 + 752/2 - 56/2
    expect(s.width).toBe("56px");
    expect(s["border-radius"]).toBe("50%");
    expect(s.transform).toBe(`scale(${newFabScale(0.5)})`);
  });
  test("commit container-transforms the FAB into the full terminal area", () => {
    const s = newFabStyle(pull({ phase: "settle", settleTarget: "commit" }), rect, 400, 48);
    expect(s.left).toBe("0px");
    expect(s.top).toBe("48px");
    expect(s.width).toBe("400px");
    expect(s.height).toBe("752px");
    expect(s["border-radius"]).toBe("0px");
    expect(s.transform).toBe("scale(1)");
  });
});
