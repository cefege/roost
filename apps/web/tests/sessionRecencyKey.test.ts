// Unit coverage for the shared sidebar urgency-ranking helpers.
// Mirrors src/lib/sessionRecencyKey.ts.

import { test, expect } from "bun:test";
import type { Session } from "@roost/shared/wire";
import { stageRank, claudeStageOf } from "../src/lib/sessionRecencyKey.ts";

const mk = (o: Record<string, unknown>): Session => o as unknown as Session;

test("stageRank ranks urgency needs-input > running == workflow > idle > done > unknown", () => {
  expect(stageRank("needs-input")).toBeGreaterThan(stageRank("running"));
  expect(stageRank("running")).toBe(stageRank("running-workflow"));
  expect(stageRank("running")).toBeGreaterThan(stageRank("idle"));
  expect(stageRank("idle")).toBeGreaterThan(stageRank("done"));
  expect(stageRank("done")).toBeGreaterThan(stageRank(undefined));
  expect(stageRank("garbage")).toBe(0);
});

test("claudeStageOf sorts non-claude below all claude sessions", () => {
  expect(claudeStageOf(mk({ kind: "shell", agent: undefined }))).toBe(-1);
  expect(claudeStageOf(mk({ kind: "claude", agent: { status: "idle" } as Session["agent"] }))).toBeGreaterThan(-1);
});
