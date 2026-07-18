// Pure agent-attention vocab (lib/agentStatus.ts) — the herdr rollup + ranking.
// attentionOf reads the reactive store (covered live); rank/rollup/present are pure.

import { describe, test, expect } from "bun:test";
import { rankOf, isActionable, rollupLevels, presentationOf, type AttentionLevel } from "../src/lib/agentStatus.ts";

const LEVELS: AttentionLevel[] = ["blocked", "done", "working", "idle", "unknown"];

describe("ranking", () => {
  test("herdr order blocked > done > working > idle > unknown", () => {
    expect(rankOf("blocked")).toBeGreaterThan(rankOf("done"));
    expect(rankOf("done")).toBeGreaterThan(rankOf("working"));
    expect(rankOf("working")).toBeGreaterThan(rankOf("idle"));
    expect(rankOf("idle")).toBeGreaterThan(rankOf("unknown"));
  });
  test("actionable = blocked/done/working; idle/unknown are calm", () => {
    expect((["blocked", "done", "working"] as AttentionLevel[]).every(isActionable)).toBe(true);
    expect((["idle", "unknown"] as AttentionLevel[]).some(isActionable)).toBe(false);
  });
});

describe("rollupLevels (group = max)", () => {
  test("empty → unknown", () => expect(rollupLevels([])).toBe("unknown"));
  test("picks the highest-attention level", () => {
    expect(rollupLevels(["idle", "working", "done"])).toBe("done");
    expect(rollupLevels(["idle", "blocked", "working"])).toBe("blocked");
    expect(rollupLevels(["idle", "idle"])).toBe("idle");
    expect(rollupLevels(["unknown", "idle"])).toBe("idle");
    expect(rollupLevels(["done", "blocked"])).toBe("blocked");
  });
});

describe("presentationOf", () => {
  test("every level → a defined --md-* token + non-empty label", () => {
    for (const l of LEVELS) {
      const v = presentationOf(l);
      expect(v.color).toMatch(/var\(--md-/);
      expect(v.label.length).toBeGreaterThan(0);
    }
  });
  test("done is distinct from working", () => {
    expect(presentationOf("done").color).not.toBe(presentationOf("working").color);
  });
  test("blocked is distinct from working (needs-input must not equal working)", () => {
    expect(presentationOf("blocked").color).not.toBe(presentationOf("working").color);
  });
  test("meaningful levels carry a non-empty short label", () => {
    for (const l of ["blocked", "done", "working", "idle"] as AttentionLevel[]) {
      expect(presentationOf(l).short.length).toBeGreaterThan(0);
    }
  });
});
