#!/usr/bin/env bun
// scripts/lint-roost.ts — mechanical enforcement of the standing repo
// invariants: the recurring-failure guards (docs/FAILURE-INDEX.md), the
// design-system raw-value ratchet, the ≤400-line file cap, and the
// log-facade rule.
//
// Exit 0 = clean. Exit 1 = at least one violation (printed with file:line +
// memory pointer). Add a new check by appending to CHECKS in
// lint-failure-checks.ts.
//
// Run: bun run lint           (blocking `lint` step of the ci.yml invariants job)
// Re-snapshot a ratchet: --update-design-baseline | --update-size-baseline
//                        --update-console-baseline

import { readdirSync, readFileSync, statSync } from "node:fs";
import { collectCounts, runRatchet, walk, type RatchetSpec } from "./lint-ratchet.ts";
import { HEADER_RATCHET, headerCounts } from "./lint-headers.ts";
import { runBoundaryCheck } from "./lint-boundaries.ts";
import { runDocPathCheck } from "./lint-doc-paths.ts";
import { join } from "node:path";
import { CHECKS } from "./lint-failure-checks.ts";

const REPO = new URL("..", import.meta.url).pathname;

interface Violation {
  file: string;
  line: number;
  text: string;
  rule: string;
  memory: string;
}

// ───────────────────────────────────────────────────────────────────────

function runPatternChecks(): Violation[] {
  const out: Violation[] = [];
  const matchedFileCounts = CHECKS.map(() => 0);
  for (const file of walk(REPO)) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    const lines = text.split("\n");
    for (const [checkIndex, check] of CHECKS.entries()) {
      if (!check.files.test(rel)) continue;
      matchedFileCounts[checkIndex]!++;
      if (check.ok) {
        if (!check.ok(file, 0, lines)) {
          out.push({ file: rel, line: 1, text: "<missing required pattern>", rule: check.rule, memory: check.memory });
        }
        continue;
      }
      lines.forEach((line, lineIndex) => {
        if (check.pattern!.test(line)) {
          out.push({ file: rel, line: lineIndex + 1, text: line.trim().slice(0, 140), rule: check.rule, memory: check.memory });
        }
      });
    }
  }
  CHECKS.forEach((check, checkIndex) => {
    if (matchedFileCounts[checkIndex] !== 0) return;
    out.push({
      file: "scripts/lint-failure-checks.ts",
      line: 1,
      text: `check targets no existing file: ${check.rule}`,
      rule: "lint: every failure check must target ≥1 file",
      memory: "docs/FAILURE-INDEX.md",
    });
  });
  return out;
}

// ───────────────────────────────────────────────────────────────────────
// Color-fallback check: parse theme-vars.css for declared --names, then
// scan apps/web/src/**/*.{tsx,ts,css} for var(--name, ...) and flag any
// reference whose --name is not declared.
// ───────────────────────────────────────────────────────────────────────

function runColorFallbackCheck(): Violation[] {
  const out: Violation[] = [];
  const themeFile = join(REPO, "apps/web/src/styles/theme-vars.css");
  let theme: string;
  try { theme = readFileSync(themeFile, "utf8"); }
  catch { return out; }
  const declared = new Set<string>(
    [...theme.matchAll(/--([a-z0-9-]+)\s*:/gi)].map((m) => m[1]!),
  );
  // Other CSS files also declare tokens (sidebar.css has --df-row-h
  // scoped to aside).
  // Any token referenced by var(...) must resolve in at least one of
  // these declaration files.
  for (const css of [
    "apps/web/src/styles/sidebar.css",
    // M3 shape/spacing/typography tokens (--md-space-*, --md-title-*, etc.)
    // live here, not in theme-vars.css.
    "apps/web/src/components/Settings/md/tokens.css",
    // Voice-input FAB motion tokens (--md-ease-*) are declared + used here.
    "apps/web/src/styles/voice-input.css",
  ]) {
    try {
      const txt = readFileSync(join(REPO, css), "utf8");
      for (const m of txt.matchAll(/--([a-z0-9-]+)\s*:/gi)) declared.add(m[1]!);
    } catch { /* ok */ }
  }

  for (const file of walk(join(REPO, "apps/web/src"))) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    if (!/\.(tsx|ts|css)$/.test(rel)) continue;
    if (rel.endsWith(".test.tsx") || rel.endsWith(".test.ts") || rel.includes("/e2e/")) continue;
    // The CSS declaration files themselves reference --name in commentary
    // and inside var(...) definitions — don't lint declarations against
    // themselves.
    if (
      rel.endsWith("/theme-vars.css") ||
      rel.endsWith("/sidebar.css")
    ) continue;
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    const lines = text.split("\n");
    lines.forEach((line, i) => {
      // Skip CSS comments (rough — sufficient for one-line comments).
      if (line.trim().startsWith("//") || line.trim().startsWith("*")) return;
      for (const m of line.matchAll(/var\(\s*--([a-z0-9-]+)\s*[,)]/g)) {
        const name = m[1]!;
        if (declared.has(name)) continue;
        // Common false-positives:
        if (name.startsWith("term-")) continue;        // wterm declares its own
        if (name.startsWith("df-")) continue;          // sidebar-scoped
        if (name === "font-anthropicons") continue;    // claude.ai/code paste
        out.push({
          file: rel,
          line: i + 1,
          text: line.trim().slice(0, 140),
          rule: `L11: var(--${name}) is not declared in theme-vars.css or sidebar.css`,
          memory: "docs/FAILURE-INDEX.md",
        });
      }
    });
  }
  return out;
}

// ───────────────────────────────────────────────────────────────────────
// Hardcoded-color-fallback check: ban var(--token, #hex) / var(--token,
// rgba(...)). Every color token is guaranteed declared by the theme engine
// (lib/theme.ts writes the canonical set; theme-vars.css aliases the rest),
// so a hardcoded color fallback is dead code AND a landmine — if the token
// ever went undefined it would silently paint the wrong color against the
// active theme. Non-color fallbacks (px, font names, var refs) are allowed.
// See docs/FAILURE-INDEX.md.
// ───────────────────────────────────────────────────────────────────────

function runHardcodedFallbackCheck(): Violation[] {
  const out: Violation[] = [];
  const COLOR_FALLBACK = /var\(\s*--[a-z0-9-]+\s*,\s*(#[0-9a-fA-F]{3,8}|rgba?\([^()]*\))\s*\)/g;
  for (const file of walk(join(REPO, "apps/web/src"))) {
    const rel = file.slice(REPO.length).replace(/^\/+/, "");
    if (!/\.(tsx|ts|css)$/.test(rel)) continue;
    if (rel.endsWith(".test.tsx") || rel.endsWith(".test.ts") || rel.includes("/e2e/")) continue;
    let text: string;
    try { text = readFileSync(file, "utf8"); } catch { continue; }
    text.split("\n").forEach((line, i) => {
      if (COLOR_FALLBACK.test(line)) {
        out.push({
          file: rel, line: i + 1, text: line.trim().slice(0, 140),
          rule: "L11: hardcoded color fallback var(--x, #hex) — tokens are always defined; drop the fallback",
          memory: "docs/FAILURE-INDEX.md",
        });
      }
      COLOR_FALLBACK.lastIndex = 0;
    });
  }
  return out;
}

// ───────────────────────────────────────────────────────────────────────
// Raw-value ratchet (design cohesion): ban NEW hardcoded hex / rgb() / px
// font-sizes in component files. Token-DEFINITION files (theme-vars, md/tokens,
// syntax-vars, voice-input) are exempt — they DECLARE the values everything
// else must reference via var(). A per-file baseline
// (scripts/design-raw-baseline.json) records the existing offender count so the
// 130+ legacy offenders don't fail the build; a file FAILS only when its count
// INCREASES (new drift). Migration lowers counts → re-baseline with
// `bun scripts/lint-roost.ts --update-design-baseline`. This is the load-bearing
// design-system enforcement: hardcoding a color/size in new UI stops compiling.
// ───────────────────────────────────────────────────────────────────────

const RAW_VALUE_ALLOW = new Set([
  "apps/web/src/styles/theme-vars.css",
  "apps/web/src/components/Settings/md/tokens.css",
  // icon.css DECLARES the .md-icon font-size utility (24/18/32px) that
  // .md-icon--sm/--lg reference — moved verbatim out of md/tokens.css, same
  // definition-file exemption.
  "apps/web/src/components/Settings/md/icon.css",
  "apps/web/src/styles/voice-input.css",
  // themes.ts DEFINES the theme palettes — raw hex is the source of the
  // canonical tokens here, not drift. Exempt (design-system phase 2 triage).
  "apps/web/src/lib/themes.ts",
  // agents.ts DECLARES the BUILTIN_AGENTS brand palette — these hexes are
  // agent identity data the palette defines, not theme drift (cf. icon.css).
  "apps/web/src/lib/agents.ts",
]);
const RAW_BASELINE_FILE = join(REPO, "scripts/design-raw-baseline.json");

// Count lines in one file carrying a raw hex color, rgb()/rgba(), or px
// font-size — excluding var(--x, …) fallbacks (owned by the fallback check)
// and comment lines.
function rawValueLineCount(text: string): number {
  let n = 0;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("//") || line.startsWith("*") || line.startsWith("/*")) continue;
    // Strip var() fallbacks so their inner hex/rgb isn't double-counted here.
    const stripped = line.replace(/var\(\s*--[a-z0-9-]+\s*,[^)]*\)/g, "");
    if (
      /#[0-9a-fA-F]{3,8}\b/.test(stripped) ||
      /\brgba?\(/.test(stripped) ||
      /font-?size\s*:\s*['"]?\s*\d+px/i.test(line)
    ) n++;
  }
  return n;
}

const RAW_RATCHET: RatchetSpec = {
  baselineFile: RAW_BASELINE_FILE,
  updateFlag: "--update-design-baseline",
  freshAllowance: 0,
  text: (n, allowed) => `${n} raw hex/rgb/px-font value lines (baseline ${allowed}) — reference a theme token instead`,
  rule: "design: no NEW raw color/px-font values — use --md-*/--surface-*/--text-* + the type ramp (ratcheted)",
  memory: "CLAUDE.md — design system",
  unit: "raw-value lines",
};

function rawCounts(): Record<string, number> {
  return collectCounts([join(REPO, "apps/web/src")], (rel) =>
    /\.(tsx|ts|css)$/.test(rel)
    && !rel.endsWith(".test.tsx")
    && !rel.endsWith(".test.ts")
    && !rel.includes("/e2e/")
    && !RAW_VALUE_ALLOW.has(rel),
  rawValueLineCount);
}


// ───────────────────────────────────────────────────────────────────────
// File-size ratchet: a source file stays ≤400 lines. A per-file baseline
// (scripts/file-size-baseline.json) freezes the files that were already
// over the cap when the rule went live, so the cap is enforceable without
// one repo-wide split: a baselined file FAILS only when it grows PAST its
// recorded count, and a file ABSENT from the baseline may never exceed the
// cap at all. Splits lower counts → re-snapshot with
// `bun scripts/lint-roost.ts --update-size-baseline`. Generated protoc
// output (`apps/*/src/gen` and `packages/*/src/gen`) is excluded — nobody
// hand-splits generated protobuf modules.
// ───────────────────────────────────────────────────────────────────────

const FILE_LINE_CAP = 400;
const SIZE_BASELINE_FILE = join(REPO, "scripts/file-size-baseline.json");
const SIZE_EXCLUDE = /^(apps|packages)\/[^/]+\/src\/gen\//;

// Hand-written source roots the cap governs: every app/package's src + tests,
// plus the two tool trees. Enumerated from the filesystem so a new workspace is
// covered the day it lands; a parent that does not exist yet is skipped.
function sizeRoots(): string[] {
  const roots: string[] = [];
  for (const parent of ["apps", "packages"]) {
    let entries;
    try { entries = readdirSync(join(REPO, parent), { withFileTypes: true }); }
    catch { continue; }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      for (const child of ["src", "tests"]) {
        roots.push(join(REPO, parent, entry.name, child));
      }
    }
  }
  for (const directory of ["scripts", "smoke"]) {
    roots.push(join(REPO, directory));
  }
  return roots.filter((path) => {
    try { return statSync(path).isDirectory(); } catch { return false; }
  });
}

const SIZE_RATCHET: RatchetSpec = {
  baselineFile: SIZE_BASELINE_FILE,
  updateFlag: "--update-size-baseline",
  freshAllowance: FILE_LINE_CAP,
  guardFloor: FILE_LINE_CAP + 1,
  text: (n, allowed) => `${n} lines (cap ${FILE_LINE_CAP}, baseline ${allowed}) — split before growing`,
  rule: "size: files stay ≤400 lines; baselined files may only shrink (ratcheted)",
  memory: "CLAUDE.md — coding standards",
  unit: "lines",
};

function fileSizes(): Record<string, number> {
  return collectCounts(sizeRoots(), (rel) => /\.(ts|tsx)$/.test(rel) && !SIZE_EXCLUDE.test(rel), (text) => text.split("\n").length);
}


// ───────────────────────────────────────────────────────────────────────
// Log-facade ratchet: coord and worker are long-lived services whose output
// is machine-read, so they log through `log` from @roost/observability/log — the one
// facade that stamps ev/level and owns the console sink (packages/observability/src/log.ts,
// outside the roots scanned here). A raw console.* in a service bypasses it.
// The surviving callsites are pre-logger bootstrap and fatal-exit paths; the
// baseline (scripts/console-baseline.json) freezes them so the rule blocks NEW
// drift. apps/roost-cli is deliberately out of scope — stdout is its product
// surface — as is apps/web, which routes through diag()/signal().
// Re-snapshot: `bun scripts/lint-roost.ts --update-console-baseline`.
// ───────────────────────────────────────────────────────────────────────

const CONSOLE_BASELINE_FILE = join(REPO, "scripts/console-baseline.json");
const CONSOLE_ROOTS = ["apps/coord/src", "apps/worker/src"];
const CONSOLE_CALL = /\bconsole\.(log|warn|error|info|debug)\s*\(/;

function consoleLineCount(text: string): number {
  let n = 0;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("//") || line.startsWith("*") || line.startsWith("/*")) continue;
    if (CONSOLE_CALL.test(line)) n++;
  }
  return n;
}

const CONSOLE_RATCHET: RatchetSpec = {
  baselineFile: CONSOLE_BASELINE_FILE,
  updateFlag: "--update-console-baseline",
  freshAllowance: 0,
  text: (n, allowed) => `${n} console.* call lines (baseline ${allowed}) — log through the @roost/observability/log facade`,
  rule: "logging: use the log facade from @roost/observability/log, not console.* (ratcheted)",
  memory: "CLAUDE.md — coding standards",
  unit: "console.* lines",
};

function consoleCounts(): Record<string, number> {
  return collectCounts(CONSOLE_ROOTS.map((root) => join(REPO, root)), (rel) => /\.ts$/.test(rel), consoleLineCount);
}


const violations = [
  ...runPatternChecks(),
  ...runBoundaryCheck(),
  ...runDocPathCheck(),
  ...runColorFallbackCheck(),
  ...runHardcodedFallbackCheck(),
  ...runRatchet(rawCounts(), RAW_RATCHET),
  ...runRatchet(fileSizes(), SIZE_RATCHET),
  ...runRatchet(consoleCounts(), CONSOLE_RATCHET),
  ...runRatchet(headerCounts(), HEADER_RATCHET),
];
if (violations.length === 0) {
  console.log("lint-roost: 0 violations");
  process.exit(0);
}

console.log(`lint-roost: ${violations.length} violations\n`);
for (const v of violations) {
  console.log(`${v.file}:${v.line}`);
  console.log(`  ${v.text}`);
  console.log(`  rule: ${v.rule}`);
  console.log(`  memory: ${v.memory}`);
  console.log("");
}
process.exit(1);
