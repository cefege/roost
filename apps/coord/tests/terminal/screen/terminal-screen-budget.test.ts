// Pins the terminal replica budget arithmetic: a declared byte budget must map to
// residency ceilings that always admit one worst-case session and never exceed the
// hub's hard maxima, and to a per-socket send buffer bounded on both ends.

import { describe, expect, test } from "bun:test";
import {
  syncBackpressureBytes,
  terminalScreenBudgetBytes,
  terminalScreenCaps,
} from "../../../src/terminal/screen/terminal-screen-budget.ts";
import {
  TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
  TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
} from "../../../src/terminal/screen/terminal-screen-hub.ts";

describe("terminalScreenCaps", () => {
  test("derives ceilings from the budget at host sizes that do not clamp", () => {
    // 1 GiB ceiling → 25% budget.
    expect(terminalScreenCaps(268_435_456)).toEqual({
      maxResidentRows: TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
      maxResidentSpans: 569_226,
    });
    // 512 MiB ceiling.
    expect(terminalScreenCaps(134_217_728)).toEqual({
      maxResidentRows: 38_130,
      maxResidentSpans: 284_613,
    });
    // Explicit 64 MiB operator budget.
    expect(terminalScreenCaps(67_108_864)).toEqual({
      maxResidentRows: 19_065,
      maxResidentSpans: 142_306,
    });
  });

  test("a budget too small for one worst-case session degrades to the floors, not to zero", () => {
    expect(terminalScreenCaps(524_288)).toEqual({
      maxResidentRows: 256,
      maxResidentSpans: 65_536,
    });
    expect(terminalScreenCaps(0)).toEqual({
      maxResidentRows: 256,
      maxResidentSpans: 65_536,
    });
  });

  test("a large host keeps today's hard maxima", () => {
    expect(terminalScreenCaps(8 * 1024 ** 3)).toEqual({
      maxResidentRows: TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
      maxResidentSpans: TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
    });
    // The span clamp binds from ~3.7 GiB of detected ceiling upward, so every
    // 4 GB-or-larger machine behaves exactly as it did before the budget existed.
    expect(terminalScreenCaps(terminalScreenBudgetBytes(undefined, 4 * 1024 ** 3)))
      .toEqual({
        maxResidentRows: TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
        maxResidentSpans: TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
      });
  });
});

describe("terminalScreenBudgetBytes", () => {
  test("an operator budget wins over the detected ceiling", () => {
    expect(terminalScreenBudgetBytes(67_108_864, 8 * 1024 ** 3)).toBe(67_108_864);
  });

  test("without a declared budget it claims a quarter of the ceiling", () => {
    expect(terminalScreenBudgetBytes(undefined, 1024 ** 3)).toBe(268_435_456);
    expect(terminalScreenBudgetBytes(undefined, 482_344_960)).toBe(120_586_240);
  });
});

describe("syncBackpressureBytes", () => {
  test("clamps the per-socket send buffer between two snapshot parts and today's 8 MiB", () => {
    expect(syncBackpressureBytes(16 * 1024 * 1024)).toBe(2 * 1024 * 1024);
    expect(syncBackpressureBytes(1024 ** 3)).toBe(8 * 1024 * 1024);
    // A 1 GiB host's replica budget still yields today's value unchanged.
    expect(syncBackpressureBytes(268_435_456)).toBe(8 * 1024 * 1024);
    expect(syncBackpressureBytes(33_554_432)).toBe(4 * 1024 * 1024);
  });
});
