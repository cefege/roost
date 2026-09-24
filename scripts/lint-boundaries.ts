// Boundary lint for app and package source trees.
// It understands the import forms used by the move tool and reports layering
// violations with stable locations. Callers choose which boundary rules block.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { walk } from "./lint-ratchet.ts";

const REPO = new URL("..", import.meta.url).pathname;
const SOURCE_EXTENSIONS = /\.(?:ts|tsx|mts|mjs|js)$/;
const MEMORY = "CLAUDE.md — layering";
const RULES = {
  B1: "layers: no relative import escapes its workspace",
  B2: "layers: workspace import allowlist",
  B3: "layers: apps/web/src/client is framework-free",
  B4: "layers: only UI imports components",
} as const;
type BoundaryId = keyof typeof RULES;

export const BOUNDARY_ENFORCED = new Set<BoundaryId>();

interface Violation {
  file: string;
  line: number;
  text: string;
  rule: string;
  memory: string;
}
interface Specifier {
  value: string;
  index: number;
  line: number;
  text: string;
  isType: boolean;
}

function sourceFiles(): string[] {
  const files: string[] = [];
  for (const group of ["apps", "packages"]) {
    const groupPath = join(REPO, group);
    let entries;
    try { entries = readdirSync(groupPath, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const sourceRoot = join(groupPath, entry.name, "src");
      try { if (!statSync(sourceRoot).isDirectory()) continue; } catch { continue; }
      for (const file of walk(sourceRoot)) {
        if (!SOURCE_EXTENSIONS.test(file)) continue;
        const rel = relative(REPO, file).split(sep).join("/");
        if (/(^|\/)(?:tests|smoke|scripts)(\/|$)/.test(rel)) continue;
        files.push(rel);
      }
    }
  }
  return files.sort();
}

function lineAt(text: string, index: number): number {
  return text.slice(0, index).split("\n").length;
}

function specifiers(text: string): Specifier[] {
  const scannable = text.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, (comment) => comment.replace(/[^\n]/g, " "));
  const found: Specifier[] = [];
  const forms: RegExp[] = [
    /\bfrom\s*(["'])([^"']+)\1/g,
    /\bimport\s*(["'])([^"']+)\1/g,
    /\bimport\s*\(\s*(["'])([^"']+)\1\s*\)/g,
    /\bmock\.module\s*\(\s*(["'])([^"']+)\1/g,
    /\brequire\s*\(\s*(["'])([^"']+)\1\s*\)/g,
    /\bnew\s+URL\s*\(\s*(["'])([^"']+)\1/g,
  ];
  for (const form of forms) {
    for (const match of scannable.matchAll(form)) {
      const index = match.index ?? 0;
      const value = match[2]!;
      const line = lineAt(text, index);
      const before = scannable.slice(0, index);
      const statementStart = Math.max(before.lastIndexOf(";"), before.lastIndexOf("\n\n"));
      const statement = before.slice(statementStart + 1);
      found.push({
        value,
        index,
        line,
        text: text.split("\n")[line - 1]?.trim().slice(0, 140) ?? value,
        isType: /\bimport\s+type\b[\s\S]*$/.test(statement),
      });
    }
  }
  return found.sort((a, b) => a.index - b.index)
    .filter((entry, index, all) => index === 0 || entry.index !== all[index - 1]!.index);
}

function workspace(rel: string): { root: string; name: string } | undefined {
  const match = /^(apps|packages)\/([^/]+)\//.exec(rel);
  return match ? { root: `${match[1]}/${match[2]}`, name: `${match[1]}/${match[2]}` } : undefined;
}

function inside(path: string, root: string): boolean {
  return path === root || path.startsWith(`${root}${sep}`);
}

function resolveRelative(importer: string, specifier: string): string {
  return resolve(REPO, dirname(importer), specifier.split(/[?#]/, 1)[0]!).split(sep).join("/");
}

function packageAllowed(workspaceName: string, specifier: string, isType: boolean): boolean {
  if (specifier.startsWith("@roost/")) {
    if (workspaceName === "packages/observability") return false;
    if (workspaceName === "packages/protocol") return specifier.startsWith("@roost/observability/") || specifier === "@wterm/core" && isType;
    if (workspaceName === "packages/platform") return false;
    if (workspaceName === "packages/wterm") return specifier.startsWith("@roost/protocol/") || specifier.startsWith("@roost/observability/") || specifier === "@wterm/core";
    if (workspaceName === "packages/host") return specifier.startsWith("@roost/protocol/") || specifier.startsWith("@roost/platform/") || specifier.startsWith("@roost/observability/");
    if (workspaceName === "apps/web") return specifier.startsWith("@roost/protocol/") || specifier.startsWith("@roost/platform/") || specifier.startsWith("@roost/observability/");
    if (workspaceName === "apps/coord" || workspaceName === "apps/worker") return /^(?:@roost\/(?:observability|protocol|platform|wterm|host)\/)/.test(specifier);
    if (workspaceName === "apps/roost-cli") return /^(?:@roost\/(?:observability|protocol|platform|wterm|host|coord|worker)\/)/.test(specifier);
    if (workspaceName === "apps/site") return false;
    return false;
  }
  if (workspaceName === "packages/observability") return specifier === "zod";
  if (workspaceName === "packages/protocol") return specifier === "@bufbuild/protobuf" || specifier.startsWith("@bufbuild/protobuf/") || specifier === "zod";
  if (workspaceName === "packages/platform") return false;
  if (workspaceName === "packages/wterm") return specifier === "bun" || specifier.startsWith("bun:") || specifier.startsWith("node:") || specifier === "@wterm/core";
  if (workspaceName === "packages/host") return specifier === "bun" || specifier.startsWith("bun:") || specifier.startsWith("node:") || specifier === "zod";
  if (workspaceName === "apps/web") return specifier !== "bun" && !specifier.startsWith("bun:") && !specifier.startsWith("node:");
  if (workspaceName === "apps/coord" || workspaceName === "apps/worker" || workspaceName === "apps/roost-cli") return true;
  if (workspaceName === "apps/site") return true;
  return false;
}

function add(out: Violation[], file: string, specifier: Specifier, id: BoundaryId, detail: string): void {
  out.push({ file, line: specifier.line, text: `${detail}: ${specifier.text}`, rule: RULES[id], memory: MEMORY });
}

export function runBoundaryCheck(): Violation[] {
  const all: Violation[] = [];
  for (const file of sourceFiles()) {
    const owner = workspace(file);
    if (!owner) continue;
    let text: string;
    try { text = readFileSync(join(REPO, file), "utf8"); } catch { continue; }
    if (file.startsWith("apps/web/src/client/") && file.endsWith(".tsx")) {
      all.push({ file, line: 1, text: "client source files must not be .tsx", rule: RULES.B3, memory: MEMORY });
    }
    for (const specifier of specifiers(text)) {
      if (specifier.value.startsWith(".")) {
        const target = resolveRelative(file, specifier.value);
        if (!inside(target, resolve(REPO, owner.root))) add(all, file, specifier, "B1", specifier.value);
        if (file.startsWith("apps/web/src/client/") && !inside(target, resolve(REPO, "apps/web/src/client"))) add(all, file, specifier, "B3", specifier.value);
        if (file.startsWith("apps/web/src/") && !file.startsWith("apps/web/src/components/") && !["App.tsx", "main.tsx", "entry.ts", "routes.ts"].includes(file.slice("apps/web/src/".length)) && inside(target, resolve(REPO, "apps/web/src/components"))) add(all, file, specifier, "B4", specifier.value);
      } else if (!packageAllowed(owner.name, specifier.value, specifier.isType)) {
        add(all, file, specifier, "B2", specifier.value);
      }
      if (file.startsWith("apps/web/src/client/") && /^(?:solid-js(?:\/|$)|@solidjs\/|@kobalte\/)/.test(specifier.value)) add(all, file, specifier, "B3", specifier.value);
    }
  }
  const enforced = all.filter((entry) => {
    const id = (Object.entries(RULES).find(([, rule]) => rule === entry.rule)?.[0] ?? "") as BoundaryId;
    return BOUNDARY_ENFORCED.has(id);
  });
  const reportOnly = all.filter((entry) => !enforced.includes(entry));
  if (reportOnly.length > 0) {
    console.log("lint-boundaries (report-only):");
    for (const entry of reportOnly) console.log(`${entry.file}:${entry.line}\n  ${entry.text}\n  rule: ${entry.rule}`);
  }
  return enforced;
}
