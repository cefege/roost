import { homedir } from "node:os";

// Expand a leading `~` / `~/` to the user's home dir; absolute and relative
// paths pass through untouched. node:fs does not expand `~` (the shell does
// on spawn), so RPC/spawn paths must run through this first. Canonical for
// the worker — consolidates the former copies in file-rpcs.ts + session-manager.ts.
export function expandTilde(path: string): string {
  if (path === "~") return homedir();
  if (path.startsWith("~/")) return homedir() + path.slice(1);
  return path;
}
