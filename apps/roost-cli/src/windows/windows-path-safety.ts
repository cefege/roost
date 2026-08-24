// Path-safety primitives shared by the Windows update and relocation
// brokers: containment checks, reparse-point guards, and the small string
// helpers every journal-validation path leans on. One copy exists because a
// divergence here is silently exploitable — the brokers trust these checks
// to keep SCM image paths inside their protected roots.
//
// samePath deliberately normalizes trailing separators ('C:\a\' == 'C:\a'):
// SCM stores canonical image paths, and the launch-current proof compares
// broker-computed paths against ones read back from the service definition,
// where a trailing slash may or may not survive serialization. Case-folding
// (not locale-aware) matches Win32 path semantics.

import { isAbsolute, join, relative, resolve, win32 } from "node:path";
import { lstat } from "node:fs/promises";

/** Narrow an unknown thrown value to a Node errno error when possible. */
export function nodeError(error: unknown): NodeJS.ErrnoException | null {
  return error instanceof Error && "code" in error ? error as NodeJS.ErrnoException : null;
}

/** Flatten any thrown value into one bounded, single-line log/journal entry. */
export function errorText(error: unknown): string {
  return String(error).replace(/[\r\n]+/g, " ").slice(0, 2048);
}

/** Strip the leading `v` and build-metadata suffix from a release version. */
export function normalizedVersion(version: string): string {
  return version.replace(/^v/, "").split("+")[0];
}

/** Read the injectable broker clock, falling back to wall-clock time. */
export function depsNow(deps: { now?: () => Date }): Date {
  return (deps.now ?? (() => new Date()))();
}

/** Case-folded Win32 path equality with trailing separators normalized. */
export function samePath(left: string, right: string): boolean {
  const fold = (path: string): string => win32.resolve(path).replace(/[\\/]+$/, "").toLowerCase();
  return fold(left) === fold(right);
}

/** True when `target` sits strictly below `root` (never equal, never above). */
export function pathIsUnder(root: string, target: string): boolean {
  const rel = pathRelative(root, target);
  return rel.length > 0 && !rel.startsWith("..") && !isPathAbsolute(rel);
}

function pathRelative(root: string, target: string): string {
  return win32.isAbsolute(root) || win32.isAbsolute(target)
    ? win32.relative(win32.resolve(root), win32.resolve(target))
    : relative(resolve(root), resolve(target));
}

function pathJoin(root: string, part: string): string {
  return win32.isAbsolute(root) ? win32.join(root, part) : join(root, part);
}

export function isPathAbsolute(path: string): boolean {
  return win32.isAbsolute(path) || isAbsolute(path);
}

/** Resolve `path` inside `root`, refusing anything that escapes it. */
export function resolveUnder(root: string, path: string): string {
  const target = resolve(root, path);
  const rel = relative(resolve(root), target);
  if (!rel || rel.startsWith("..") || isAbsolute(rel) || resolve(root, rel) !== target) {
    throw new Error(`asset escapes version directory: ${path}`);
  }
  return target;
}

/** Refuse `target` if `root` itself or any component on the way is a symlink
 *  or reparse point — Windows junctions otherwise let a journal path escape
 *  its protected root without leaving the drive prefix. Stops at the first
 *  missing component: nothing below can be a link yet. */
export async function assertNoReparseComponents(root: string, target: string): Promise<void> {
  if (!samePath(root, target) && !pathIsUnder(root, target)) {
    throw new Error(`protected-root path escapes its root: ${target}`);
  }
  const parts = pathRelative(root, target).split(/[\\/]/).filter(Boolean);
  let cursor = root;
  for (const part of ["", ...parts]) {
    if (part) cursor = pathJoin(cursor, part);
    try {
      const info = await lstat(cursor);
      if (info.isSymbolicLink()) throw new Error(`path traverses a link/reparse point: ${cursor}`);
    } catch (error) {
      if (nodeError(error)?.code === "ENOENT") return;
      throw error;
    }
  }
}
