// Shared engine for lint-roost.ts's count-based ratchets: walk repo source
// trees, collect per-file counts, compare against a JSON baseline, fail only
// on regression past a file's allowance, and rewrite the baseline sorted via
// each ratchet's --update-* flag. Message bytes printed here are contract —
// tests and CI match the `lint-roost:` lines verbatim.

import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const SKIP_DIRS = new Set([
  "node_modules", ".git", "dist", ".turbo", "target",
  "coverage", "test-results", ".claude",
]);

export function* walk(dir: string): Generator<string> {
  for (const ent of readdirSync(dir, { withFileTypes: true })) {
    if (SKIP_DIRS.has(ent.name)) continue;
    const p = join(dir, ent.name);
    if (ent.isDirectory()) yield* walk(p);
    else if (ent.isFile()) yield p;
  }
}

export interface RatchetSpec {
  baselineFile: string;
  updateFlag: string;
  /** Allowance for a file absent from the baseline. */
  freshAllowance: number;
  /** Counts below this floor take no part in compare or snapshot. */
  guardFloor?: number;
  /** Snapshot filter override; defaults to the compare-side floor. */
  snapshot?: (count: number) => boolean;
  /** Regression predicate; defaults to "grew past its allowance". */
  regressed?: (count: number, allowed: number) => boolean;
  text: (count: number, allowed: number) => string;
  rule: string;
  memory: string;
  unit: string;
}

export function collectCounts(
  roots: string[],
  include: (rel: string) => boolean,
  count: (text: string) => number,
  keepZero = false,
): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const root of roots) {
    for (const file of walk(root)) {
      const rel = file.slice(new URL("..", import.meta.url).pathname.length).replace(/^\/+/, "");
      if (!include(rel)) continue;
      let text: string;
      try { text = readFileSync(file, "utf8"); } catch { continue; }
      const n = count(text);
      if (n > 0 || keepZero) counts[rel] = n;
    }
  }
  return counts;
}

export function runRatchet(counts: Record<string, number>, spec: RatchetSpec): Array<{
  file: string;
  line: number;
  text: string;
  rule: string;
  memory: string;
}> {
  if (process.argv.includes(spec.updateFlag)) {
    const keep = spec.snapshot ?? ((n: number) => n >= (spec.guardFloor ?? 1));
    const snapshotted = Object.entries(counts).filter(([, n]) => keep(n))
      .sort(([a], [b]) => a.localeCompare(b));
    writeFileSync(spec.baselineFile, JSON.stringify(Object.fromEntries(snapshotted), null, 2) + "\n");
    const total = snapshotted.reduce((sum, [, n]) => sum + n, 0);
    console.log(`lint-roost: wrote ${spec.baselineFile.split("/").pop()} — ${snapshotted.length} files, ${total} ${spec.unit}`);
    process.exit(0);
  }
  let baseline: Record<string, number> = {};
  try { baseline = JSON.parse(readFileSync(spec.baselineFile, "utf8")) as Record<string, number>; }
  catch { baseline = {}; }
  const out: Array<{ file: string; line: number; text: string; rule: string; memory: string }> = [];
  for (const [rel, n] of Object.entries(counts)) {
    if (n < (spec.guardFloor ?? 1)) continue;
    const allowed = baseline[rel] ?? spec.freshAllowance;
    if ((spec.regressed ?? ((count, limit) => count > limit))(n, allowed)) {
      out.push({ file: rel, line: 1, text: spec.text(n, allowed), rule: spec.rule, memory: spec.memory });
    }
  }
  return out;
}
