// Mobile tab-bar swipe commit decision + momentum settle math (lib/deckSwipe.ts).

import { describe, test, expect } from "bun:test";
import { shouldCommitSwitch, settleDurationMs, endMode, newFabProgress, peekCard, newFabScale, PEEK_SCALE_MIN, PEEK_RADIUS_PX, shouldDismissCard, cardSwipeAlpha } from "../src/lib/deckSwipe.ts";

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
