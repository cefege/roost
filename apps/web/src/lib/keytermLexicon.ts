// keytermLexicon — a self-building vocabulary that learns which terms recur
// across your dictations, so a freshly-opened pane (thin on-screen context)
// still biases toward your project's jargon (Roost, coord, Kysely, tailnet).
//
// Each recording feeds its live-context terms in via learnTerms: every stored
// score decays (×DECAY) then the new terms get +1. Terms you keep using
// plateau high; one-off noise decays out. No curated list, no codebase scan —
// it converges on your actual vocabulary.
//
// Persistence: localStorage (sync, tiny map — IndexedDB would be overkill).
// The merge/rank logic is pure + unit-tested; only read/write touch storage.

import { safeJsonParse } from "@roost/shared/json";

export type Lexicon = Record<string, number>;

const STORAGE_KEY = "roost.keytermLexicon.v1";
const DECAY = 0.9; // recurring term steady-state ≈ 1/(1-DECAY) = 10 — bounded
const PRUNE_BELOW = 0.3; // drop terms that decayed toward zero
const MAX_STORED = 200; // cap the stored vocabulary

/** Pure: age existing scores, bump the new terms, prune + cap. */
export function mergeLexicon(prev: Lexicon, terms: readonly string[]): Lexicon {
  const next: Lexicon = {};
  for (const k in prev) {
    const v = prev[k]! * DECAY;
    if (v >= PRUNE_BELOW) next[k] = v;
  }
  for (const t of terms) next[t] = (next[t] ?? 0) + 1;
  return Object.fromEntries(
    Object.entries(next)
      .sort((a, b) => b[1] - a[1])
      .slice(0, MAX_STORED),
  );
}

/** Pure: top-n terms by score. */
export function topTerms(lex: Lexicon, n: number): string[] {
  return Object.entries(lex)
    .sort((a, b) => b[1] - a[1])
    .slice(0, n)
    .map(([t]) => t);
}

function read(): Lexicon {
  return safeJsonParse<Lexicon>(localStorage.getItem(STORAGE_KEY), {}, "keytermLexicon");
}

function write(lex: Lexicon): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(lex));
  } catch {
    /* private mode / quota — lexicon is best-effort */
  }
}

/** Fold a recording's live-context terms into the persisted lexicon. Feed only
 *  terms extracted WITHOUT the lexicon seed, or recurring seeds never decay. */
export function learnTerms(terms: readonly string[]): void {
  if (terms.length === 0) return;
  write(mergeLexicon(read(), terms));
}

/** Top-n persisted terms — cold-start seed for a thin pane. */
export function lexiconTopTerms(n: number): string[] {
  return topTerms(read(), n);
}
