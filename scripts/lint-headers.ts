// scripts/lint-headers.ts — owns the mandatory file-header ratchet.
// lint-roost.ts consumes its counts and specifier. Hand-written app and package
// source is checked; generated files and tests remain exempt. The snapshot in
// scripts/header-baseline.json freezes pre-existing short headers.

import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { collectCounts, type RatchetSpec } from "./lint-ratchet.ts";

const REPO = new URL("..", import.meta.url).pathname;
const HEADER_MIN_LINES = 3;
const HEADER_BASELINE_FILE = join(REPO, "scripts/header-baseline.json");
const HEADER_EXCLUDE = [
  /^(apps|packages)\/[^/]+\/src\/gen\//,
  /\.generated\.ts$/,
  /\.d\.ts$/,
];

// Length of the leading comment run: consecutive // lines plus one /*…*/
// block. Blank lines and a shebang before the run count neither way.
function leadingHeaderLines(text: string): number {
  const lines = text.split("\n");
  let index = lines[0]?.startsWith("#!") ? 1 : 0;
  while (index < lines.length && lines[index]!.trim() === "") index++;
  let count = 0;
  let inBlock = false;
  for (; index < lines.length; index++) {
    const trimmed = lines[index]!.trim();
    if (inBlock) {
      count++;
      if (trimmed.includes("*/")) inBlock = false;
    } else if (trimmed.startsWith("//")) count++;
    else if (trimmed.startsWith("/*")) {
      count++;
      inBlock = !trimmed.includes("*/");
    } else break;
  }
  return count;
}

function headerRoots(): string[] {
  const roots: string[] = [];
  for (const parent of ["apps", "packages"]) {
    let entries;
    try { entries = readdirSync(join(REPO, parent), { withFileTypes: true }); }
    catch { continue; }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      roots.push(join(REPO, parent, entry.name, "src"));
    }
  }
  return roots.filter((path) => {
    try { return statSync(path).isDirectory(); } catch { return false; }
  });
}

export function headerCounts(): Record<string, number> {
  return collectCounts(
    headerRoots(),
    (rel) => /\.(ts|tsx)$/.test(rel)
      && !HEADER_EXCLUDE.some((pattern) => pattern.test(rel))
      && !rel.includes("/tests/")
      && !/\.(test|spec)\.(ts|tsx)$/.test(rel),
    leadingHeaderLines,
    true,
  );
}

export const HEADER_RATCHET: RatchetSpec = {
  baselineFile: HEADER_BASELINE_FILE,
  updateFlag: "--update-header-baseline",
  freshAllowance: HEADER_MIN_LINES,
  guardFloor: 0,
  snapshot: (count) => count < HEADER_MIN_LINES,
  regressed: (count, allowed) => count < allowed,
  text: (count, allowed) =>
    `file opens with ${count === 0 ? "no WHY header" : `a ${count}-line header`} (need ≥${HEADER_MIN_LINES}; grandfathered at ${allowed}) — add the mandatory file-header comment`,
  rule: "headers: every source file opens with a WHY-file-header comment (ratcheted)",
  memory: "CLAUDE.md — coding standards",
  unit: "header lines",
};
