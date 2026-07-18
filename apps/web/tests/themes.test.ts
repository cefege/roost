// Locks the theme engine's core invariant: every registered theme defines
// every canonical token with a valid color. The Record<CanonicalToken,string>
// type already enforces presence at compile time; this is the runtime
// belt-and-suspenders (catches empty strings + malformed colors the type
// can't) and guards id uniqueness.

import { test, expect, describe } from "bun:test";
import { CANONICAL_TOKENS } from "../src/lib/themeTokens.ts";
import { THEMES, THEMES_BY_ID, DEFAULT_THEME_ID } from "../src/lib/themes.ts";

const COLOR_RE = /^(#[0-9a-fA-F]{3}|#[0-9a-fA-F]{6}|#[0-9a-fA-F]{8}|rgba?\([^)]+\))$/;

describe("theme registry", () => {
  test("at least Dark + Light registered", () => {
    expect(THEMES.length).toBeGreaterThanOrEqual(2);
  });

  test("theme ids are unique", () => {
    const ids = THEMES.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  test("DEFAULT_THEME_ID resolves", () => {
    expect(THEMES_BY_ID[DEFAULT_THEME_ID]).toBeDefined();
  });

  for (const theme of THEMES) {
    describe(`theme "${theme.id}"`, () => {
      test("defines every canonical token", () => {
        for (const token of CANONICAL_TOKENS) {
          expect(theme.tokens[token]).toBeDefined();
        }
      });

      test("no extra/unknown tokens", () => {
        const known = new Set<string>(CANONICAL_TOKENS);
        for (const key of Object.keys(theme.tokens)) {
          expect(known.has(key)).toBe(true);
        }
      });

      test("every value is a valid color", () => {
        for (const token of CANONICAL_TOKENS) {
          const value = theme.tokens[token];
          expect(value.length).toBeGreaterThan(0);
          expect(value).toMatch(COLOR_RE);
        }
      });

      test("has label + appearance + group", () => {
        expect(theme.label.length).toBeGreaterThan(0);
        expect(["light", "dark"]).toContain(theme.appearance);
        expect(["System", "Light", "Dark", "Palette"]).toContain(theme.group);
      });
    });
  }
});
