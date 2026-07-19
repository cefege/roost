// Damped-spring solver math (lib/spring.ts).

import { describe, test, expect } from "bun:test";
import {
  springStep,
  isSpringAtRest,
  criticalDamping,
  SPRING_SNAP,
  SPRING_REST_POSITION,
  SPRING_REST_VELOCITY,
  type SpringState,
} from "../src/lib/spring.ts";

describe("criticalDamping", () => {
  test("2*sqrt(k*m) — unit mass", () => {
    expect(criticalDamping(100, 1)).toBeCloseTo(20);
  });
  test("scales with mass", () => {
    expect(criticalDamping(100, 4)).toBeCloseTo(40);
  });
  test("mass defaults to 1", () => {
    expect(criticalDamping(400)).toBeCloseTo(40);
  });
});

describe("springStep", () => {
  test("zero dt is a no-op", () => {
    const s: SpringState = { position: 10, velocity: 5 };
    expect(springStep(s, 0, SPRING_SNAP, 0)).toEqual(s);
  });
  test("negative dt is clamped to no-op", () => {
    const s: SpringState = { position: 10, velocity: 5 };
    expect(springStep(s, 0, SPRING_SNAP, -16)).toEqual(s);
  });
  test("pulls position toward target", () => {
    const next = springStep({ position: 100, velocity: 0 }, 0, SPRING_SNAP, 16);
    expect(next.position).toBeLessThan(100); // moved toward 0
    expect(next.velocity).toBeLessThan(0);   // accelerating toward target
  });
  test("already at target with no velocity stays put", () => {
    const next = springStep({ position: 0, velocity: 0 }, 0, SPRING_SNAP, 16);
    expect(next.position).toBeCloseTo(0);
    expect(next.velocity).toBeCloseTo(0);
  });
});

describe("isSpringAtRest", () => {
  test("close + slow → rest", () => {
    expect(isSpringAtRest({ position: 0.05, velocity: 0.5 }, 0)).toBe(true);
  });
  test("far → not rest", () => {
    expect(isSpringAtRest({ position: 5, velocity: 0 }, 0)).toBe(false);
  });
  test("fast → not rest even if close", () => {
    expect(isSpringAtRest({ position: 0, velocity: 100 }, 0)).toBe(false);
  });
  test("boundary just inside → rest", () => {
    const almost = SPRING_REST_POSITION / 2;
    const slow = SPRING_REST_VELOCITY / 2;
    expect(isSpringAtRest({ position: almost, velocity: slow }, 0)).toBe(true);
  });
});

describe("convergence", () => {
  test("SPRING_SNAP settles to target within ~1s", () => {
    let s: SpringState = { position: 200, velocity: 0 };
    let frames = 0;
    while (!isSpringAtRest(s, 0) && frames < 120) {
      s = springStep(s, 0, SPRING_SNAP, 16);
      frames++;
    }
    expect(isSpringAtRest(s, 0)).toBe(true);
    expect(frames).toBeLessThan(120); // under ~2s of 16ms frames
  });
});
