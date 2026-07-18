// keytermLexicon — self-building vocabulary merge/rank logic (pure parts).
// Asserts: recurring terms plateau, one-off noise decays out, top-n ranks,
// stored vocabulary is capped.

import { describe, test, expect } from "bun:test";
import { mergeLexicon, topTerms, type Lexicon } from "../src/lib/keytermLexicon.ts";

describe("mergeLexicon", () => {
  test("recurring term accumulates; one-off decays toward pruning", () => {
    let lex: Lexicon = {};
    for (let i = 0; i < 5; i++) lex = mergeLexicon(lex, ["Kysely"]); // typed every time
    lex = mergeLexicon(lex, ["oneOff"]); // typed once
    expect(lex["Kysely"]).toBeGreaterThan(lex["oneOff"]!);
  });

  test("a term not seen again decays below the prune floor and disappears", () => {
    let lex = mergeLexicon({}, ["staleTok"]); // score 1
    for (let i = 0; i < 15; i++) lex = mergeLexicon(lex, ["other"]); // age staleTok out
    expect(lex["staleTok"]).toBeUndefined();
  });

  test("recurring term plateaus (bounded steady-state, no runaway)", () => {
    let lex: Lexicon = {};
    for (let i = 0; i < 100; i++) lex = mergeLexicon(lex, ["coord"]);
    // DECAY 0.9 → steady-state 1/(1-0.9) = 10.
    expect(lex["coord"]).toBeLessThan(11);
    expect(lex["coord"]).toBeGreaterThan(9);
  });

  test("caps stored vocabulary at 200", () => {
    const many = Array.from({ length: 300 }, (_, i) => `t${i}`);
    const lex = mergeLexicon({}, many);
    expect(Object.keys(lex).length).toBeLessThanOrEqual(200);
  });
});

describe("topTerms", () => {
  test("ranks by score, descending", () => {
    const lex = { low: 1, high: 9, mid: 5 };
    expect(topTerms(lex, 2)).toEqual(["high", "mid"]);
  });
});
