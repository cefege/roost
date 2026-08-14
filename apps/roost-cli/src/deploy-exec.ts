// Shell / ssh execution layer for `roost deploy` — process spawning,
// capture-or-throw, and the ssh option set shared by every remote step.

import { spawn } from "bun";

export async function run(cmd: string[], opts: { quiet?: boolean; env?: Record<string, string> } = {}): Promise<{ exit: number; stdout: string; stderr: string }> {
  const proc = spawn({
    cmd,
    stdio: opts.quiet ? ["ignore", "pipe", "pipe"] : ["inherit", "inherit", "inherit"],
    env: opts.env ? { ...(process.env as Record<string, string>), ...opts.env } : undefined,
  });
  // Drain stdout + stderr in parallel; serial drain risks a deadlock when
  // both pipe buffers fill (~64K each) and the producer blocks on stderr
  // while we await stdout. `exited` joined with the drains so exitCode is
  // populated by the time we read it. Signal-killed subprocesses report
  // exitCode=null in Bun → coerce to 128+sig (or 1) so callers see failure.
  const [stdout, stderr] = opts.quiet
    ? await Promise.all([
        new Response(proc.stdout as ReadableStream).text(),
        new Response(proc.stderr as ReadableStream).text(),
      ])
    : ["", ""];
  await proc.exited;
  const exit = proc.exitCode ?? (proc.signalCode ? 128 : 1);
  return { exit, stdout, stderr };
}

export function finishWorkerDeploy(
  result: { exit: number; stdout: string; stderr: string },
  successMessage: string,
): void {
  if (result.exit !== 0) {
    throw new Error(
      `worker service verification failed (exit ${result.exit})\n`
      + `stdout:\n${result.stdout || "(empty)"}\n`
      + `stderr:\n${result.stderr || "(empty)"}`,
    );
  }
  const output = result.stdout.trim();
  if (output) console.log(output.split("\n").map((line) => `   ${line}`).join("\n"));
  console.log(successMessage);
}

// Capture-or-throw helper for steps where a non-zero exit is fatal. Used
// for rsync calls (the original code discarded the result) and any other
// `await run(...)` that should not silently continue on failure.
export async function runOrDie(cmd: string[], label: string): Promise<void> {
  const r = await run(cmd, { quiet: true });
  if (r.exit !== 0) {
    console.error(`>> ${label} failed (exit ${r.exit})`);
    if (r.stdout) console.error(r.stdout);
    if (r.stderr) console.error(r.stderr);
    process.exit(r.exit || 1);
  }
}

// StrictHostKeyChecking=accept-new auto-trusts unknown host keys on the
// first connection and writes them to known_hosts. Without this, a
// fresh deploy target (or a worker the SSH client has never seen — the
// common case when the Deploy button runs from coord rather than the
// developer's interactive shell) fails with "Host key verification
// failed" and exit 2. UserKnownHostsFile defaults are fine; we only
// adjust the prompt behavior.
export const SSH_OPTS = [
  "-o", "StrictHostKeyChecking=accept-new",
  "-o", "ConnectTimeout=10",
];
export const RSYNC_RSH = `ssh ${SSH_OPTS.map((s) => s.includes(" ") ? `'${s}'` : s).join(" ")}`;

export async function sshExec(host: string, remoteCmd: string): Promise<{ exit: number; stdout: string; stderr: string }> {
  // Non-interactive ssh skips ~/.zshrc, so PATH is bare. Prepend the
  // standard macOS Apple-Silicon homebrew + bun locations explicitly.
  const wrapped = `export PATH="/opt/homebrew/bin:$HOME/.bun/bin:$PATH"; ${remoteCmd}`;
  return run(["ssh", ...SSH_OPTS, host, wrapped], { quiet: true });
}

/** Local git HEAD to stamp into the deployed service's GIT_SHA, with the
 *  dirty-tree guard every deploy path shares. Refuses (exit 7) on
 *  uncommitted changes unless ROOST_ALLOW_DIRTY=1, because otherwise the
 *  shipped tree and the stamp disagree and the SPA's drift badge fires
 *  falsely until the next clean deploy. */
export function resolveLocalGitShaOrDie(): string {
  let sha = "";
  try {
    const r = Bun.spawnSync(["git", "rev-parse", "HEAD"]);
    if (r.exitCode === 0) sha = r.stdout.toString().trim();
  } catch (e) { console.error("git check failed:", String(e)); }
  let isDirty = false;
  try {
    const st = Bun.spawnSync(["git", "status", "--porcelain"]);
    if (st.exitCode === 0 && st.stdout.toString().trim().length > 0) isDirty = true;
  } catch (e) { console.error("git check failed:", String(e)); }
  if (!isDirty) return sha;
  if (process.env.ROOST_ALLOW_DIRTY === "1") {
    console.warn(`>> WARN: uncommitted changes — stamping GIT_SHA=${sha}-dirty (ROOST_ALLOW_DIRTY=1)`);
    return sha ? `${sha}-dirty` : "";
  }
  console.error("ERROR: uncommitted changes in working tree.");
  console.error("  Commit first, OR re-run with ROOST_ALLOW_DIRTY=1 to ship the dirty state");
  console.error("  with a `-dirty` GIT_SHA suffix (the drift badge will then keep firing");
  console.error("  until you commit + deploy clean, which is the point).");
  console.error("  Run `git status` to see what's pending.");
  process.exit(7);
}
