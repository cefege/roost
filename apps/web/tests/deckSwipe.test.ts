// Mobile tab-bar swipe commit decision + momentum settle math (lib/deckSwipe.ts).

import { describe, test, expect } from "bun:test";
import { shouldCommitSwitch, settleDurationMs, endMode, newFabProgress } from "../src/lib/deckSwipe.ts";

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
  test("fast flick clamps to min", () => {
    expect(settleDurationMs(300, 5)).toBe(180);
  });
  test("slow release clamps to max", () => {
    expect(settleDurationMs(300, 0.1)).toBe(340);
  });
  test("zero velocity → max", () => {
    expect(settleDurationMs(50, 0)).toBe(340);
  });
  test("mid case proportional", () => {
    expect(settleDurationMs(240, 1)).toBe(240);
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
