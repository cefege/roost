// Documentation path lint for tracked Markdown and transport allowlists.
// It checks repository-root references after removing line and fragment anchors.
// The exported function returns the same violation shape as the main linter.
// Quarantined archives are historical claims, not current navigation paths;
// Phase 2 removes them from the tracked tree.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import * as transportAllowlists from "./lint-transport-allowlists.ts";

const REPO = new URL("..", import.meta.url).pathname;
const RULE = "docs: repo-root path references must exist";
const MEMORY = "CLAUDE.md — documentation";
const PATH_TOKEN = /`((?:apps|packages|crates|protocol|scripts|smoke|docs|native|xtask|third_party)\/[^`\s]+)`/g;

interface Violation {
  file: string;
  line: number;
  text: string;
  rule: string;
  memory: string;
}

function trackedMarkdown(): string[] {
  let output: string;
  try {
    output = execFileSync("git", ["ls-files", "-z", "--", "*.md"], { cwd: REPO, encoding: "utf8" });
  } catch {
    return [];
  }
  return output.split("\0").filter((file) =>
    file.endsWith(".md") && !/(^|\/)(?:node_modules|dist|docs\/(?:archive|snapshots))(\/|$)/.test(file),
  ).sort();
}

function referencedPath(token: string): string | undefined {
  if (/[*{<…]|\.\.\./.test(token)) return undefined;
  return token.replace(/#.*$/, "").replace(/:\d+(?:-\d+)?$/, "");
}

function missingPath(file: string, line: number, path: string): Violation {
  return { file, line, text: `missing repository path: ${path}`, rule: RULE, memory: MEMORY };
}

export function runDocPathCheck(): Violation[] {
  const violations: Violation[] = [];
  for (const file of trackedMarkdown()) {
    let text: string;
    try { text = readFileSync(join(REPO, file), "utf8"); } catch { continue; }
    for (const [lineIndex, line] of text.split("\n").entries()) {
      for (const match of line.matchAll(PATH_TOKEN)) {
        const path = referencedPath(match[1]!);
        if (path && !existsSync(join(REPO, path))) violations.push(missingPath(file, lineIndex + 1, path));
      }
    }
  }
  const allowlistPaths = new Set<string>();
  for (const value of Object.values(transportAllowlists)) {
    if (!Array.isArray(value)) continue;
    for (const path of value) if (typeof path === "string") allowlistPaths.add(path);
  }
  for (const path of [...allowlistPaths].sort()) {
    if (!existsSync(join(REPO, path))) violations.push(missingPath("scripts/lint-transport-allowlists.ts", 1, path));
  }
  return violations;
}
