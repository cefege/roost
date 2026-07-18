// format.test.ts — transfer-display helpers. Pure functions; boundary coverage.

import { expect, test, describe } from "bun:test";
import { formatSpeed, formatEta } from "../src/lib/format.ts";

describe("formatSpeed", () => {
  test("zero / negative / non-finite → em dash", () => {
    expect(formatSpeed(0)).toBe("—");
    expect(formatSpeed(-5)).toBe("—");
    expect(formatSpeed(NaN)).toBe("—");
    expect(formatSpeed(Infinity)).toBe("—");
  });
  test("scales with the byte unit and appends /s", () => {
    expect(formatSpeed(512)).toBe("512 B/s");
    expect(formatSpeed(4 * 1024 * 1024)).toBe("4.0 MB/s");
  });
});

describe("formatEta", () => {
  test("unknown (negative / non-finite) → empty string", () => {
    expect(formatEta(-1)).toBe("");
    expect(formatEta(NaN)).toBe("");
  });
  test("sub-second → <1s", () => {
    expect(formatEta(0.4)).toBe("<1s");
  });
  test("seconds", () => {
    expect(formatEta(45)).toBe("45s");
  });
  test("minutes, with and without a seconds remainder", () => {
    expect(formatEta(130)).toBe("2m 10s");
    expect(formatEta(120)).toBe("2m");
  });
  test("hours, with and without a minutes remainder", () => {
    expect(formatEta(3900)).toBe("1h 5m");
    expect(formatEta(3600)).toBe("1h");
  });
});
