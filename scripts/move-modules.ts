#!/usr/bin/env bun
// Manifest-driven module mover for repository restructures.
// It validates the whole plan, rewrites tracked references, then git-mv sources.
// Maintainers run it with --manifest and an optional --dry-run preview.

import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, extname, join, posix, relative, resolve } from "node:path";

const CODE_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".mjs", ".js"]);
const TEXT_EXTENSIONS = new Set([
  ".md", ".json", ".yml", ".yaml", ".sh", ".ps1", ".toml", ".proto", ".css", ".html", ".trace",
]);
const PATH_CHARACTER = /[A-Za-z0-9_./-]/;
const MODULE_SPECIFIER = /(\bfrom\s*|\bimport\s*|\bimport\s*\(\s*|\bmock\s*\.\s*module\s*\(\s*|\brequire\s*\(\s*|\bnew\s+URL\s*\(\s*)(["'])([^"'\r\n]*)\2/g;

type Move = { from: string; to: string };
type SpecifierRewrite = { from: string; to: string };
type MoveManifest = { moves: Move[]; specifierRewrites: SpecifierRewrite[] };
type CliOptions = { manifest: string; dryRun: boolean; normalize: boolean };
type MoveContext = {
  root: string;
  tracked: string[];
  moves: Map<string, string>;
  moveList: Move[];
  specifierRewrites: SpecifierRewrite[];
  normalize: boolean;
  packageData: Map<string, { name: string; exports: Record<string, unknown> }>;
};

function parseCli(argv: string[]): CliOptions {
  let manifest: string | undefined;
  let dryRun = false;
  let normalize = false;
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]!;
    if (argument === "--dry-run") dryRun = true;
    else if (argument === "--normalize-cross-workspace") normalize = true;
    else if (argument === "--manifest" || argument.startsWith("--manifest=")) {
      const inline = argument.slice("--manifest=".length);
      manifest = argument === "--manifest" ? argv[++index] : inline;
      if (!manifest) throw new Error("--manifest requires a path");
    } else throw new Error(`unknown argument: ${argument}`);
  }
  if (!manifest) throw new Error("usage: bun scripts/move-modules.ts --manifest <path.json> [--dry-run]");
  return { manifest, dryRun, normalize };
}

function readManifest(path: string): MoveManifest {
  const parsed = JSON.parse(readFileSync(path, "utf8")) as Partial<MoveManifest>;
  if (!parsed || !Array.isArray(parsed.moves) || !Array.isArray(parsed.specifierRewrites)) {
    throw new Error("manifest must contain moves and specifierRewrites arrays");
  }
  for (const move of parsed.moves) {
    if (!move || typeof move.from !== "string" || typeof move.to !== "string") {
      throw new Error("each move must have string from and to paths");
    }
  }
  const rewriteSources = new Set<string>();
  for (const rewrite of parsed.specifierRewrites) {
    if (!rewrite || typeof rewrite.from !== "string" || typeof rewrite.to !== "string") {
      throw new Error("each specifier rewrite must have string from and to values");
    }
    if (rewriteSources.has(rewrite.from)) throw new Error(`duplicate specifier rewrite: ${rewrite.from}`);
    rewriteSources.add(rewrite.from);
  }
  return parsed as MoveManifest;
}

function git(root: string, args: string[]): string {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(result.stderr.trim() || `git ${args.join(" ")} failed`);
  return result.stdout;
}

function trackedFiles(root: string): string[] {
  return git(root, ["ls-files", "-z"]).split("\0").filter(Boolean).sort();
}

function repoPath(value: string, label: string, directory = false): string {
  const hadSlash = value.endsWith("/");
  const withoutSlash = value.replace(/\/$/, "");
  const normalized = posix.normalize(withoutSlash);
  if (!value || value.startsWith("/") || value.includes("\\") || normalized === "." || normalized.startsWith("../") || normalized.includes("/../")) {
    throw new Error(`${label} must be a normalized repo-relative path: ${value}`);
  }
  if (normalized !== withoutSlash || (!directory && hadSlash)) {
    throw new Error(`${label} must be a normalized repo-relative path: ${value}`);
  }
  return normalized;
}

function expandMoves(inputs: Move[], trackedSet: Set<string>, root: string): Move[] {
  const expanded: Move[] = [];
  const sources = new Set<string>();
  const targets = new Set<string>();
  for (const input of inputs) {
    const isDirectory = input.from.endsWith("/");
    const from = repoPath(input.from, "move from", isDirectory);
    const to = repoPath(input.to, "move to", isDirectory);
    const matches = isDirectory ? [...trackedSet].filter((file) => file.startsWith(`${from}/`)) : [from];
    if (matches.length === 0) throw new Error(`move source has no tracked files: ${from}`);
    if (isDirectory && existsSync(join(root, to))) {
      throw new Error(`move target already exists: ${to}`);
    }
    for (const source of matches) {
      const target = isDirectory ? posix.join(to, source.slice(from.length + 1)) : to;
      if (!trackedSet.has(source) || !existsSync(join(root, source))) {
        throw new Error(`move source is not a tracked file on disk: ${source}`);
      }
      if (existsSync(join(root, target))) throw new Error(`move target already exists: ${target}`);
      if (sources.has(source)) throw new Error(`duplicate move source: ${source}`);
      if (targets.has(target)) throw new Error(`duplicate move target: ${target}`);
      sources.add(source);
      targets.add(target);
      expanded.push({ from: source, to: target });
    }
  }
  return expanded;
}

function excluded(file: string, includeGenerated: boolean): boolean {
  const parts = file.split("/");
  return file === "bun.lock" || file.startsWith("apps/coord/migrations/")
    || parts.includes("node_modules") || parts.includes("dist")
    || (!includeGenerated && /(^|\/)src\/gen\//.test(file));
}

function codeMask(source: string): Uint8Array {
  const mask = new Uint8Array(source.length).fill(1);
  let index = 0;
  while (index < source.length) {
    const character = source[index]!;
    const next = source[index + 1];
    let end = index;
    if (character === "/" && next === "/") {
      end = source.indexOf("\n", index + 2); if (end < 0) end = source.length;
    } else if (character === "/" && next === "*") {
      end = source.indexOf("*/", index + 2); end = end < 0 ? source.length : end + 2;
    } else if (character === "\"" || character === "'" || character === "`") {
      end = index + 1;
      while (end < source.length) {
        if (source[end] === "\\") end += 2;
        else if (source[end] === character) { end += 1; break; }
        else end += 1;
      }
    } else { index += 1; continue; }
    mask.fill(0, index, end);
    index = end;
  }
  return mask;
}

function resolveRelative(
  root: string,
  importer: string,
  specifier: string,
  allowDirectory = false,
): string | undefined {
  const base = posix.normalize(posix.join(posix.dirname(importer), specifier));
  try {
    const baseStat = statSync(join(root, base));
    if (baseStat.isFile() || (allowDirectory && baseStat.isDirectory())) {
      return baseStat.isDirectory() ? `${base.replace(/\/$/, "")}/` : base;
    }
  } catch { /* extension or index resolution follows */ }
  const candidates = [`${base}.ts`, `${base}.tsx`, posix.join(base, "index.ts")];
  return candidates.find((candidate) => {
    try { return statSync(join(root, candidate)).isFile(); } catch { return false; }
  });
}

function workspaceFor(file: string): string | undefined {
  return /^(?:apps|packages)\/[^/]+(?:\/|$)/.exec(file)?.[0].replace(/\/$/, "");
}

function packageSpecifier(context: MoveContext, workspace: string, target: string): string {
  let data = context.packageData.get(workspace);
  if (!data) {
    try {
      const parsed = JSON.parse(readFileSync(join(context.root, workspace, "package.json"), "utf8")) as { name?: unknown; exports?: unknown };
      if (typeof parsed.name !== "string" || !parsed.exports || typeof parsed.exports !== "object") {
        throw new Error("invalid package exports");
      }
      data = { name: parsed.name, exports: parsed.exports as Record<string, unknown> };
      context.packageData.set(workspace, data);
    } catch {
      throw new Error(`missing export: ${workspace} ${target}`);
    }
  }
  const relativeTarget = target.slice(workspace.length + 1);
  const find = (value: unknown, key?: string): string | undefined => {
    if (typeof value === "string" && key?.startsWith(".")) {
      const pattern = value.replace(/^\.\//, "");
      const star = pattern.indexOf("*");
      let capture = "";
      const matches = star < 0 ? pattern === relativeTarget
        : relativeTarget.startsWith(pattern.slice(0, star)) && relativeTarget.endsWith(pattern.slice(star + 1))
          && relativeTarget.length >= pattern.length - 1
          && (capture = relativeTarget.slice(star, relativeTarget.length - (pattern.length - star - 1))) !== undefined;
      if (!matches) return undefined;
      const subpath = key.includes("*") ? key.replace("*", capture) : key;
      return data!.name + (subpath === "." ? "" : subpath.slice(1));
    }
    if (Array.isArray(value)) {
      for (const item of value) { const found = find(item, key); if (found) return found; }
    } else if (value && typeof value === "object") {
      for (const [childKey, child] of Object.entries(value)) {
        const found = find(child, childKey.startsWith(".") ? childKey : key);
        if (found) return found;
      }
    }
    return undefined;
  };
  return find(data.exports) ?? (() => { throw new Error(`missing export: ${workspace} ${target}`); })();
}

function preserveRelative(importer: string, target: string, original: string): string {
  let result = relative(posix.dirname(importer), target).replaceAll("\\", "/");
  if (!/\.[A-Za-z0-9]+$/.test(original)) result = result.replace(/\.(?:[cm]?[jt]s|tsx)$/, "");
  if (original.startsWith("./") && !result.startsWith(".")) result = `./${result}`;
  if (target.endsWith("/") && !result.endsWith("/")) result += "/";
  return result;
}

function rewriteSpecifier(context: MoveContext, importer: string, specifier: string, allowDirectory: boolean): string {
  const exact = context.specifierRewrites.find((entry) => !entry.from.endsWith("/") && entry.from === specifier);
  if (exact) return exact.to;
  const prefix = context.specifierRewrites.filter((entry) => entry.from.endsWith("/"))
    .sort((left, right) => right.from.length - left.from.length)
    .find((entry) => specifier.startsWith(entry.from));
  if (prefix) return prefix.to + specifier.slice(prefix.from.length);
  if (!specifier.startsWith(".")) return specifier;
  if (context.moves.size === 0 && !context.normalize) return specifier;
  const oldTarget = resolveRelative(context.root, importer, specifier, allowDirectory);
  if (!oldTarget) {
    if (context.moves.has(importer)) throw new Error(`cannot resolve relative import in ${importer}: ${specifier}`);
    return specifier;
  }
  const newTarget = context.moves.get(oldTarget) ?? oldTarget;
  const newImporter = context.moves.get(importer) ?? importer;
  if (oldTarget === newTarget && newImporter === importer && !context.normalize) return specifier;
  const importerWorkspace = workspaceFor(newImporter);
  const targetWorkspace = workspaceFor(newTarget);
  const exempt = newImporter === "scripts" || newImporter.startsWith("scripts/")
    || newImporter === "smoke" || newImporter.startsWith("smoke/");
  const packageTest = newImporter.startsWith("packages/") && newImporter.includes("/tests/");
  const generatedBuildInput = /\.generated\.ts$/.test(newImporter);
  if (importerWorkspace && targetWorkspace && importerWorkspace !== targetWorkspace && !exempt && !packageTest && !generatedBuildInput) {
    return packageSpecifier(context, targetWorkspace, newTarget);
  }
  return preserveRelative(newImporter, newTarget, specifier);
}

function rewriteImports(context: MoveContext, file: string, source: string): { text: string; count: number } {
  if (excluded(file, false)) return { text: source, count: 0 };
  const mask = codeMask(source);
  let count = 0;
  const text = source.replace(MODULE_SPECIFIER, (match, prefix: string, quote: string, specifier: string, offset: number) => {
    if (!mask[offset]) return match;
    const replacement = rewriteSpecifier(context, file, specifier, prefix.includes("URL"));
    if (replacement === specifier) return match;
    count += 1;
    return `${prefix}${quote}${replacement}${quote}`;
  });
  return { text, count };
}

function wildcardCapture(pattern: string, value: string): string | undefined {
  const star = pattern.indexOf("*");
  if (star < 0) return pattern === value ? "" : undefined;
  const suffixLength = pattern.length - star - 1;
  return value.startsWith(pattern.slice(0, star)) && value.endsWith(pattern.slice(star + 1))
    && value.length >= pattern.length - 1 ? value.slice(star, value.length - suffixLength) : undefined;
}

function rewriteExportValue(context: MoveContext, workspace: string, value: string): string {
  if (!value.startsWith("./src/")) return value;
  const pattern = value.slice(2);
  const star = pattern.indexOf("*");
  if (star < 0) {
    const target = context.moves.get(`${workspace}/${pattern}`);
    if (!target) return value;
    const targetWorkspace = workspaceFor(target);
    return targetWorkspace ? `./${relative(targetWorkspace, target).replaceAll("\\", "/")}` : value;
  }
  const outputs = context.tracked.flatMap((file): Array<{ capture: string; path: string; workspace: string }> => {
    if (!file.startsWith(`${workspace}/`)) return [];
    const capture = wildcardCapture(pattern, file.slice(workspace.length + 1));
    if (capture === undefined) return [];
    const path = context.moves.get(file) ?? file;
    return [{ capture, path, workspace: workspaceFor(path) ?? workspace }];
  });
  const moved = outputs.filter(({ capture, path, workspace: outputWorkspace }) => {
    return path !== `${workspace}/${pattern.replace("*", capture)}` || outputWorkspace !== workspace;
  });
  if (moved.length === 0) return value;
  const outputWorkspaces = new Set(outputs.map(({ workspace: outputWorkspace }) => outputWorkspace));
  if (outputWorkspaces.size !== 1) throw new Error(`cannot represent moved files as one export pattern: ${workspace} ${value}`);
  const outputWorkspace = outputs[0]!.workspace;
  const paths = outputs.map(({ capture, path, workspace: movedWorkspace }) => ({
    capture, path: relative(movedWorkspace, path).replaceAll("\\", "/"),
  }));
  const first = paths[0]!;
  const firstStar = first.path.indexOf(first.capture);
  const prefix = first.path.slice(0, firstStar);
  const suffix = first.path.slice(firstStar + first.capture.length);
  if (firstStar < 0 || paths.some(({ capture, path }) => !path.startsWith(prefix + capture + suffix))) {
    throw new Error(`cannot represent moved files as one export pattern: ${workspace} ${value}`);
  }
  return `./${prefix}*${suffix}`;
}

function rewritePackageJson(context: MoveContext, file: string, source: string): string {
  const workspace = posix.dirname(file);
  const parsed = JSON.parse(source) as { exports?: unknown };
  if (!parsed.exports) return source;
  let changed = false;
  const rewrite = (value: unknown): unknown => {
    if (typeof value === "string") {
      const replacement = rewriteExportValue(context, workspace, value);
      changed ||= replacement !== value;
      return replacement;
    }
    if (Array.isArray(value)) return value.map(rewrite);
    if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, rewrite(child)]));
    return value;
  };
  parsed.exports = rewrite(parsed.exports);
  return changed ? `${JSON.stringify(parsed, null, 2)}\n` : source;
}

function rewritePaths(moves: Move[], source: string): string {
  const replacements = [...moves].sort((left, right) => right.from.length - left.from.length);
  if (replacements.length === 0) return source;
  const bySource = new Map(replacements.map((move) => [move.from, move.to]));
  const pattern = new RegExp(replacements.map((move) => move.from.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|"), "g");
  return source.replace(pattern, (match, offset: number, whole: string) => {
    const before = whole[offset - 1] ?? "";
    const after = whole[offset + match.length] ?? "";
    return PATH_CHARACTER.test(before) || PATH_CHARACTER.test(after) ? match : bySource.get(match)!;
  });
}

function planContent(context: MoveContext): { changes: Map<string, string>; imports: number; texts: number } {
  const changes = new Map<string, string>();
  let imports = 0;
  let texts = 0;
  for (const file of context.tracked) {
    const isCode = CODE_EXTENSIONS.has(extname(file)) && !excluded(file, true);
    const isText = TEXT_EXTENSIONS.has(extname(file)) && !excluded(file, true);
    if (!isCode && !isText) continue;
    const original = readFileSync(join(context.root, file), "utf8");
    let text = original;
    if (isCode && !excluded(file, false)) {
      const rewritten = rewriteImports(context, file, text);
      text = rewritten.text;
      imports += rewritten.count;
    }
    if (file.endsWith("/package.json") || file === "package.json") text = rewritePackageJson(context, file, text);
    if (isCode || isText) {
      const rewritten = rewritePaths(context.moveList, text);
      if (rewritten !== text) texts += 1;
      text = rewritten;
    }
    if (text !== original) changes.set(file, text);
  }
  return { changes, imports, texts };
}

function main(): void {
  const options = parseCli(process.argv.slice(2));
  const root = resolve(import.meta.dir, "..");
  const manifest = readManifest(resolve(process.cwd(), options.manifest));
  const tracked = trackedFiles(root);
  const trackedSet = new Set(tracked);
  const moves = expandMoves(manifest.moves, trackedSet, root);
  const context: MoveContext = {
    root, tracked, moves: new Map(moves.map((move) => [move.from, move.to])), moveList: moves,
    specifierRewrites: manifest.specifierRewrites, normalize: options.normalize, packageData: new Map(),
  };
  const planned = planContent(context);
  if (!options.dryRun) {
    for (const [file, text] of planned.changes) writeFileSync(join(root, file), text);
    for (const move of moves) {
      mkdirSync(dirname(join(root, move.to)), { recursive: true });
      git(root, ["mv", move.from, move.to]);
    }
  }
  console.log(`moved=${moves.length} importsRewritten=${planned.imports} textFilesRewritten=${planned.texts}`);
}

try { main(); } catch (error) { console.error(error instanceof Error ? error.message : error); process.exit(1); }
