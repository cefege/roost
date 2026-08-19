// Which workspaces a staged worker release must contain.
//
// A macOS deploy rsyncs a slim tree, but it also ships the canonical root
// `package.json` + `bun.lock` so a given Git SHA installs byte-identical
// dependencies. That makes `bun install --frozen-lockfile` on the target
// authoritative: EVERY workspace the lockfile declares must exist in the staged
// tree, or bun reports "lockfile had changes, but lockfile is frozen" and the
// deploy dies before it installs anything. A workspace whose code the worker
// never runs still needs its manifest — an otherwise empty directory holding
// one `package.json` is enough.
//
// This used to be a hand-written list of two (`apps/roost-cli`, `smoke`).
// `apps/site` was added to the root `workspaces` globs long afterwards and
// every macOS deploy failed from that commit on, so the set is derived from the
// root manifest instead of enumerated here.

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

/** Rsynced whole by the macOS deploy; they need no manifest-only pass. */
const FULLY_RSYNCED_WORKSPACES: readonly string[] = [
  "apps/worker",
  "apps/shared",
  "apps/coord",
  "apps/web",
];

/** Workspace directories, relative to the checkout root, that must be staged
 *  with only their `package.json`. Sorted, so the deploy log and the remote
 *  `mkdir` are stable across hosts and runs. */
export function manifestOnlyWorkspaces(sourceRoot: string): readonly string[] {
  const root = JSON.parse(readFileSync(join(sourceRoot, "package.json"), "utf8")) as {
    workspaces?: readonly string[];
  };
  const found = new Set<string>();
  for (const pattern of root.workspaces ?? []) {
    if (!pattern.endsWith("/*")) {
      if (existsSync(join(sourceRoot, pattern, "package.json"))) found.add(pattern);
      continue;
    }
    const parent = pattern.slice(0, -2);
    for (const entry of readdirSync(join(sourceRoot, parent), { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const relative = `${parent}/${entry.name}`;
      if (existsSync(join(sourceRoot, relative, "package.json"))) found.add(relative);
    }
  }
  for (const whole of FULLY_RSYNCED_WORKSPACES) found.delete(whole);
  return [...found].sort();
}
