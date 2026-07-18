// Pure launch-agent resolver tripwire. resolveAgentFrom maps a stored
// (selected, custom) choice to the effective agent the FAB launches:
//   - a built-in id → that agent's command, isCustom false,
//   - "custom" + text → the text as command, isCustom true, glyph = first char,
//   - "custom" + empty → claude fallback,
//   - unknown id → claude fallback.

import { describe, test, expect } from "bun:test";
import { resolveAgentFrom } from "../src/lib/agents.ts";

describe("resolveAgentFrom", () => {
  test("built-in id resolves to its command", () => {
    const r = resolveAgentFrom("codex", "");
    expect(r.command).toBe("codex");
    expect(r.isCustom).toBe(false);
  });

  test("custom command resolves to the typed text", () => {
    const r = resolveAgentFrom("custom", "aider -x");
    expect(r.command).toBe("aider -x");
    expect(r.isCustom).toBe(true);
    expect(r.glyph).toBe("A");
  });

  test("empty custom falls back to claude", () => {
    expect(resolveAgentFrom("custom", "").command).toBe("claude");
  });

  test("unknown id falls back to claude", () => {
    expect(resolveAgentFrom("zzz", "").command).toBe("claude");
  });
});
