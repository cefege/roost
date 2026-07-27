// Unit coverage for the shared sidebar urgency-ranking helpers.
// Mirrors src/lib/sessionRecencyKey.ts.

import { test, expect } from "bun:test";
import { stageRank } from "../src/lib/sessionRecencyKey.ts";

test("stageRank ranks OMP urgency needs-input > running > idle > done", () => {
  expect(stageRank("needs-input")).toBeGreaterThan(stageRank("running"));
  expect(stageRank("running")).toBeGreaterThan(stageRank("idle"));
  expect(stageRank("idle")).toBeGreaterThan(stageRank("done"));
  expect(stageRank("done")).toBeGreaterThan(stageRank(undefined));
  expect(stageRank("garbage")).toBe(0);
});
