// Left-edge swipe-to-open drawer decision math (lib/edgeSwipeDrawer.ts).

import { describe, test, expect } from "bun:test";
import { lockAxis, openOffsetPx, shouldOpen, closeOffsetPx, shouldClose } from "../src/lib/edgeSwipeDrawer.ts";

describe("lockAxis", () => {
  test("under the arm gate → none", () => {
    expect(lockAxis(2, 3)).toBe("none");
  });
  test("horizontal-dominant → x", () => {
    expect(lockAxis(20, 3)).toBe("x");
  });
  test("vertical-dominant → y", () => {
    expect(lockAxis(3, 20)).toBe("y");
  });
  test("equal travel fails the 1.5x ratio → y", () => {
    expect(lockAxis(20, 20)).toBe("y");
  });
});

describe("openOffsetPx", () => {
  test("no drag → fully off-screen", () => {
    expect(openOffsetPx(0, 400)).toBe(-400);
  });
  test("dragged one full width → open", () => {
    expect(openOffsetPx(400, 400)).toBe(0);
  });
  test("over-drag clamps to open", () => {
    expect(openOffsetPx(600, 400)).toBe(0);
  });
  test("leftward drag clamps to closed", () => {
    expect(openOffsetPx(-50, 400)).toBe(-400);
  });
});

describe("shouldOpen", () => {
  test("past 30% width opens", () => {
    expect(shouldOpen(120, 0, 400)).toBe(true);
  });
  test("just under 30% width stays closed", () => {
    expect(shouldOpen(119, 0, 400)).toBe(false);
  });
  test("a rightward flick opens", () => {
    expect(shouldOpen(10, 0.8, 400)).toBe(true);
  });
  test("just under flick velocity stays closed", () => {
    expect(shouldOpen(10, 0.79, 400)).toBe(false);
  });
  test("leftward never opens", () => {
    expect(shouldOpen(-200, 0, 400)).toBe(false);
  });
});

describe("closeOffsetPx", () => {
  test("open base → 0", () => {
    expect(closeOffsetPx(0, 400)).toBe(0);
  });
  test("dragged one full width → off right", () => {
    expect(closeOffsetPx(400, 400)).toBe(400);
  });
  test("over-drag clamps to width", () => {
    expect(closeOffsetPx(600, 400)).toBe(400);
  });
  test("leftward drag clamps to stay open", () => {
    expect(closeOffsetPx(-50, 400)).toBe(0);
  });
});

describe("shouldClose", () => {
  test("past 30% width closes", () => {
    expect(shouldClose(120, 0, 400)).toBe(true);
  });
  test("just under 30% width stays open", () => {
    expect(shouldClose(119, 0, 400)).toBe(false);
  });
  test("a rightward flick closes", () => {
    expect(shouldClose(10, 0.8, 400)).toBe(true);
  });
  test("just under flick velocity stays open", () => {
    expect(shouldClose(10, 0.79, 400)).toBe(false);
  });
  test("leftward never closes", () => {
    expect(shouldClose(-200, 0, 400)).toBe(false);
  });
});
