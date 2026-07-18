// `roost state` — generate STATE.md content. Called by Claude Code's
// Stop hook (.claude/settings.json) to auto-update STATE.md after
// each session. Writes to stdout; hook redirects to STATE.md.
// REWRITE.md R0.11.

import { spawn } from "bun";

// Resolve repo root from this file's location: apps/roost-cli/src/state.ts
// → ../../../ is the repo root regardless of where the CLI is invoked from.
const REPO_ROOT = new URL("../../../", import.meta.url).pathname;

async function out(cmd: string[]): Promise<string> {
  const p = spawn({ cmd, stdout: "pipe", stderr: "pipe", cwd: REPO_ROOT });
  const text = await new Response(p.stdout).text();
  await p.exited;
  return text.trimEnd();
}

export async function state(_args: string[]): Promise<void> {
  const branch = await out(["git", "branch", "--show-current"]);
  const commits = await out(["git", "log", "--oneline", "-5"]);
  const status = await out(["git", "status", "--short"]);
  const dateUtc = new Date().toISOString();

  // Truncate noisy status when long
  const statusBlock = status.split("\n").slice(0, 30).join("\n");

  const md = [
    "<!-- AUDIENCE: claude (auto-updated by Stop hook per R0.11) -->",
    "# STATE — Roost v2 rewrite snapshot",
    "",
    `updated=${dateUtc}`,
    `branch=${branch}`,
    "",
    "## last 5 commits",
    "```",
    commits,
    "```",
    "",
    "## git status",
    "```",
    statusBlock || "(clean)",
    "```",
    "",
    "## next action",
    "Past R4.5 cutover; `apps_legacy/` deleted (phase-24g). No queued spine-phase.",
    "Active work tracked per-commit (`phase-<slug>:`). See `git log --oneline` for current arc.",
    "Reopen REWRITE.md with a new R-anchor (R11+) before the next architectural shift.",
    "",
  ].join("\n");
  console.log(md);
}
