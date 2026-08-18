// Per-cwd HISTFILE so `↑` arrow recall survives across worker restarts
// and per-session restarts. The path is keyed on the cwd hash so any
// shell respawn at the same folder shares the same on-disk history.
//
// Both zsh and bash honour HISTFILE. Zsh additionally requires
// HISTSIZE + SAVEHIST > 0 to actually persist on shell exit, so we
// set all three. Defaults of zsh (HISTSIZE=10, SAVEHIST=10) would
// silently truncate.
// PowerShell uses a distinct file in the same hashed root through
// Set-PSReadLineOption -HistorySavePath.
//
// Trade-off: a user with `setopt SHARE_HISTORY` in ~/.zshrc loses
// the global cross-tab history for sessions inside roost — they get
// per-cwd isolation instead. Acceptable v1; if anyone notices we
// can layer in a 'global' mode.

import { createHash } from "node:crypto";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";
import { workerDataDir } from "@roost/shared/paths";
import { supportedHostPlatform } from "@roost/shared/platform";

const HOST_PLATFORM = supportedHostPlatform();
const HISTORY_ROOT = HOST_PLATFORM === "win32"
  ? join(workerDataDir(process.env, HOST_PLATFORM), "history")
  : join(homedir(), ".roost", "history");
let _ensuredRoot = false;

function ensureRoot(): void {
  if (_ensuredRoot) return;
  mkdirSync(HISTORY_ROOT, { recursive: true });
  _ensuredRoot = true;
}

function historyPathFor(cwd: string, suffix: string): string {
  const identity = HOST_PLATFORM === "win32"
    ? cwd.replaceAll("\\", "/").toLocaleLowerCase("en-US")
    : cwd;
  const slug = createHash("sha256").update(identity).digest("hex").slice(0, 12);
  return join(HISTORY_ROOT, `${slug}.${suffix}`);
}

export function psReadLineHistoryPath(cwd: string): string {
  ensureRoot();
  return historyPathFor(cwd, "psreadline_history");
}

/** Env block to merge into a shell PTY spawn. Returns the HISTFILE
 * pointing at the per-cwd path plus HISTSIZE/SAVEHIST high enough
 * that long sessions don't get truncated mid-day. mkdir-p the
 * parent dir on first call. */
export function withHistfile(cwd: string): Record<string, string> {
  ensureRoot();
  return {
    HISTFILE: historyPathFor(cwd, "history"),
    HISTSIZE: "10000",
    SAVEHIST: "10000",
  };
}
