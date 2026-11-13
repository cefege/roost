import { lstat, readFile, realpath } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import type { BuiltinRequirements } from "./builtin-requirements";
import type { FixtureContext } from "./scenario";

export interface BoundaryRow { channel: string; value: unknown }
export interface BuiltinCaptureContext extends Pick<FixtureContext, "extensionContext" | "workspace" | "load" | "record"> {}
export interface BuiltinCaptureRequest {
  channels: string[];
  requests: Array<{ channel: string; pointer?: string; occurrence?: number }>;
}
export interface CaptureState {
  requirement: BuiltinRequirements;
  context: BuiltinCaptureContext;
  home: string;
  ownedRoots: string[];
}

export function objectValue(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${label}: expected object`);
  return value as Record<string, unknown>;
}

export function pointerParts(pointer: string): string[] {
  if (!pointer.startsWith("/")) throw new Error(`Invalid capture pointer: ${pointer}`);
  return pointer.slice(1).split("/").map(part => part.replace(/~1/g, "/").replace(/~0/g, "~"));
}

export function requestedPointers(state: CaptureState, channel: string): string[][] {
  return state.requirement.captures.filter(capture => capture.channel === channel && capture.pointer !== undefined)
    .map(capture => pointerParts(capture.pointer!));
}

export function assignPath(target: Record<string, unknown>, parts: readonly string[], value: unknown): void {
  if (parts.length === 0) throw new Error("Cannot assign empty capture path");
  let current = target;
  for (const part of parts.slice(0, -1)) {
    if (!Object.hasOwn(current, part)) Object.defineProperty(current, part, { value: {}, enumerable: true, writable: true, configurable: true });
    current = objectValue(current[part], `Capture path ${part}`);
  }
  Object.defineProperty(current, parts[parts.length - 1], { value, enumerable: true, writable: true, configurable: true });
}


function isWithin(root: string, file: string): boolean {
  const suffix = relative(resolve(root), resolve(file));
  return suffix === "" || (!suffix.startsWith(`..${sep}`) && suffix !== ".." && !isAbsolute(suffix));
}

export async function ownedPath(state: CaptureState, file: string): Promise<string> {
  const absolute = resolve(file);
  if (!state.ownedRoots.some(root => isWithin(root, absolute))) throw new Error(`Capture path outside owned profile/workspace: ${absolute}`);
  let ancestor = absolute;
  for (;;) {
    try {
      const canonical = await realpath(ancestor);
      const roots = await Promise.all(state.ownedRoots.map(root => realpath(root)));
      if (!roots.some(root => isWithin(root, canonical))) throw new Error(`Capture symlink escapes owned directories: ${absolute}`);
      break;
    } catch (error) {
      if (!isMissing(error)) throw error;
      const parent = resolve(ancestor, "..");
      if (parent === ancestor) throw error;
      ancestor = parent;
    }
  }
  return absolute;
}

export function isMissing(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

export async function exists(state: CaptureState, file: string): Promise<boolean> {
  try { await lstat(await ownedPath(state, file)); return true; }
  catch (error) { if (isMissing(error)) return false; throw error; }
}

export async function readOwnedText(state: CaptureState, file: string): Promise<string> {
  return readFile(await ownedPath(state, file), "utf8");
}

export async function readBoundaryRows(state: CaptureState, environment: string): Promise<BoundaryRow[]> {
  const file = process.env[environment];
  if (!file) throw new Error(`Missing real boundary trace: ${environment}`);
  const text = await readOwnedText(state, file);
  const rows: BoundaryRow[] = [];
  for (const [index, line] of text.split("\n").entries()) {
    if (!line.trim()) continue;
    let decoded: unknown;
    try { decoded = JSON.parse(line); }
    catch (error) { throw new Error(`Invalid boundary JSONL ${file}:${index + 1}`, { cause: error }); }
    const row = objectValue(decoded, `Boundary row ${file}:${index + 1}`);
    if (typeof row.channel !== "string" || !Object.hasOwn(row, "value")) throw new Error(`Invalid boundary row ${file}:${index + 1}`);
    rows.push({ channel: row.channel, value: row.value });
  }
  return rows;
}

export async function boundaryValue(state: CaptureState, environment: string, channel: string): Promise<unknown> {
  const rows = await readBoundaryRows(state, environment);
  const row = rows.findLast(candidate => candidate.channel === channel);
  if (!row) throw new Error(`Missing actual ${channel} observation in ${environment}`);
  return row.value;
}
