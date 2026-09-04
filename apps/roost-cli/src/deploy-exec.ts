// Shell / ssh execution layer for `roost deploy` — process spawning,
// capture-or-throw, and the ssh option set shared by every remote step.

import { posix } from "node:path";
import { spawn } from "bun";
import { REMOTE_DEPLOY_LOCK_PROGRAM } from "./remote-deploy-lock-program.ts";
import { posixShellQuote } from "@roost/shared/shell-quote";

export interface RunOptions {
  quiet?: boolean;
  /** With quiet, replay captured stdout+stderr through console.log after a
   *  clean exit — capture-or-throw for callers that must also SHOW tool
   *  output (coordinator push steps) without losing failure detail. */
  echo?: boolean;
  env?: Record<string, string>;
  cwd?: string;
  signal?: AbortSignal;
}

export async function run(cmd: string[], opts: RunOptions = {}): Promise<{ exit: number; stdout: string; stderr: string }> {
  if (opts.signal?.aborted) {
    const reason = opts.signal.reason;
    return {
      exit: reason instanceof DeployFailure ? reason.exitCode : 9,
      stdout: "",
      stderr: reason instanceof Error ? reason.message : String(reason ?? "operation aborted"),
    };
  }
  const proc = spawn({
    cmd,
    stdio: opts.quiet ? ["ignore", "pipe", "pipe"] : ["inherit", "inherit", "inherit"],
    env: opts.env ? { ...(process.env as Record<string, string>), ...opts.env } : undefined,
    cwd: opts.cwd,
  });
  const abort = () => {
    try {
      proc.kill();
    } catch {
      // The process won the race and already exited.
    }
  };
  opts.signal?.addEventListener("abort", abort, { once: true });
  if (opts.signal?.aborted) abort();
  try {
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
    const abortReason = opts.signal?.reason;
    const aborted = opts.signal?.aborted === true;
    const exit = aborted
      ? abortReason instanceof DeployFailure ? abortReason.exitCode : 9
      : proc.exitCode ?? (proc.signalCode ? 128 : 1);
    const abortMessage = abortReason instanceof Error
      ? abortReason.message
      : String(abortReason ?? "operation aborted");
    if (opts.echo && opts.quiet && exit === 0) {
      const echoed = `${stdout}${stderr}`.trim();
      if (echoed) console.log(echoed);
    }
    return {
      exit,
      stdout,
      stderr: aborted ? [stderr, abortMessage].filter(Boolean).join("\n") : stderr,
    };
  } finally {
    opts.signal?.removeEventListener("abort", abort);
  }
}

export class DeployFailure extends Error {
  constructor(readonly exitCode: number, message: string) {
    super(message);
    this.name = "DeployFailure";
  }
}

export function failDeploy(exitCode: number, message: string): never {
  throw new DeployFailure(exitCode, message);
}

export type DeployWorkerOs = "darwin" | "linux";
export const REMOTE_MACHINE_TRANSACTION_PATHS = {
  darwin: "Library/Application Support/RoostWorkerV2/service/machine-transaction.sqlite",
  linux: ".local/share/RoostWorkerV2/service/machine-transaction.sqlite",
} as const satisfies Record<DeployWorkerOs, string>;
export const POSIX_WORKER_DEPLOY_JOURNAL_PATHS = Object.freeze({
  local: "transactions/worker-deploy.json",
  linux: "worker-deploy-journal",
  darwin: "macos-worker-deploy-v1.json",
  coordinator: "transactions/coordinator-deploy.json",
} as const);

export function remoteMachineTransactionPath(
  os: DeployWorkerOs,
  installedEnvironment: Readonly<Record<string, string>>,
): string {
  const serviceDir = installedEnvironment.ROOST_SERVICE_DIR;
  const workerDataDir = installedEnvironment.ROOST_WORKER_DATA_DIR;
  const configured = serviceDir
    ? posix.join(serviceDir, "machine-transaction.sqlite")
    : workerDataDir
      ? posix.join(workerDataDir, "service", "machine-transaction.sqlite")
      : null;
  if (configured !== null) {
    if (!posix.isAbsolute(configured) || /[\r\n\0]/.test(configured)) {
      throw new DeployFailure(2, `installed machine transaction path is unsafe: ${JSON.stringify(configured)}`);
    }
    return configured;
  }
  return REMOTE_MACHINE_TRANSACTION_PATHS[os];
}

export function workerServiceIsRunning(output: string, os: DeployWorkerOs): boolean {
  if (os === "darwin") {
    return /^\s*state = running\s*$/m.test(output)
      && /^\s*active count = 1\s*$/m.test(output)
      && /^\s*pid = [1-9]\d*\s*$/m.test(output);
  }
  return /^ActiveState=active\s*$/m.test(output)
    && /^SubState=running\s*$/m.test(output)
    && /^MainPID=[1-9]\d*\s*$/m.test(output);
}

export function workerServiceMatchesRelease(output: string): boolean {
  return /^RoostReleaseMatch=yes$/m.test(output);
}

export function finishWorkerDeploy(
  result: { exit: number; stdout: string; stderr: string },
  successMessage: string,
  os: DeployWorkerOs,
): void {
  if (result.exit !== 0 || !workerServiceIsRunning(result.stdout, os)) {
    throw new DeployFailure(
      result.exit || 8,
      `worker service verification failed (exit ${result.exit})\n`
      + `stdout:\n${result.stdout || "(empty)"}\n`
      + `stderr:\n${result.stderr || "(empty)"}`,
    );
  }
  const output = result.stdout.trim();
  if (output) console.log(output.split("\n").map((line) => `   ${line}`).join("\n"));
  console.log(successMessage);
}

// Capture-or-throw helper for steps where a non-zero exit is fatal — rsync
// fan-out, remote lock traffic, and the coordinator push's local git/build
// steps (pass { cwd, echo: true } to reproduce runChecked semantics there).
// The bare AbortSignal form stays legal for the many rsync callers.
export async function runOrDie(cmd: string[], label: string, opts?: RunOptions | AbortSignal): Promise<void> {
  const r = await run(cmd, { quiet: true, ...(opts instanceof AbortSignal ? { signal: opts } : opts) });
  if (r.exit !== 0) {
    throw new DeployFailure(
      r.exit || 1,
      [
        `${label} failed (exit ${r.exit})`,
        r.stdout || "",
        r.stderr || "",
      ].filter(Boolean).join("\n"),
    );
  }
}

// StrictHostKeyChecking=accept-new auto-trusts unknown host keys on the
// first connection and writes them to known_hosts. Without this, a
// fresh deploy target (or a worker the SSH client has never seen — the
// common case when the Deploy button runs from coord rather than the
// developer's interactive shell) fails with "Host key verification
// failed" and exit 2. SSH keepalives also bound a dead session, so a lock
// refresh cannot block its owner from releasing indefinitely.
export const SSH_OPTS = [
  "-o", "StrictHostKeyChecking=accept-new",
  "-o", "ConnectTimeout=10",
  "-o", "ServerAliveInterval=5",
  "-o", "ServerAliveCountMax=3",
];
export const RSYNC_RSH = `ssh ${SSH_OPTS.map((s) => s.includes(" ") ? `'${s}'` : s).join(" ")}`;

export async function sshExec(host: string, remoteCmd: string, signal?: AbortSignal): Promise<{ exit: number; stdout: string; stderr: string }> {
  // Non-interactive ssh skips ~/.zshrc, so PATH is bare. Prepend the
  // standard macOS Apple-Silicon homebrew + bun locations explicitly.
  const wrapped = `export PATH="/opt/homebrew/bin:$HOME/.bun/bin:$PATH"; ${remoteCmd}`;
  return run(["ssh", ...SSH_OPTS, "--", host, wrapped], { quiet: true, signal });
}

// The remote lock is a SQLite-backed renewable lease. SQLite serializes stale
// takeover atomically and releases its kernel lock if a command is killed;
// the durable lease row then bounds recovery if the deploying CLI disappears.
const REMOTE_DEPLOY_LOCK_LEASE_SECONDS = 15 * 60;
export interface RemoteDeployLockHandle {
  signal: AbortSignal;
}
interface DeploySignalTarget {
  once(signal: "SIGINT" | "SIGTERM", listener: () => void): unknown;
  removeListener(signal: "SIGINT" | "SIGTERM", listener: () => void): unknown;
}
interface RemoteDeployLockRefresh extends RemoteDeployLockHandle {
  renewNow(): Promise<void>;
  stop(): void;
  settled(): Promise<DeployFailure | null>;
}
const remoteDeployLockRefreshes = new Map<string, RemoteDeployLockRefresh>();


export function _remoteDeployLockCommands(
  lockPath: string,
  owner: string,
  leaseSeconds = REMOTE_DEPLOY_LOCK_LEASE_SECONDS,
): { acquire: string; renew: string; release: string } {
  if (!Number.isSafeInteger(leaseSeconds) || leaseSeconds < 1) {
    throw new Error("remote deployment lock lease must be a positive whole number of seconds");
  }
  const lock = posixShellQuote(lockPath);
  const ownerValue = posixShellQuote(owner);
  const program = posixShellQuote(REMOTE_DEPLOY_LOCK_PROGRAM);
  const command = (action: "acquire" | "renew" | "release") =>
    `set -e; umask 077; lock_spec=${lock}; case "$lock_spec" in /*) lock="$lock_spec";; *) lock="$HOME/$lock_spec";; esac; mkdir -p "$(dirname "$lock")"; ` +
    `ROOST_DEPLOY_LOCK_ACTION=${action} ROOST_DEPLOY_LOCK_PATH="$lock" ` +
    `ROOST_DEPLOY_LOCK_OWNER=${ownerValue} ROOST_DEPLOY_LOCK_LEASE=${leaseSeconds} ` +
    `bun -e ${program}`;
  return {
    acquire: command("acquire"),
    renew: command("renew"),
    release: command("release"),
  };
}

function createRemoteDeployLockRefresh(
  host: string,
  executeRenew: (signal: AbortSignal) => Promise<{ exit: number; stdout: string; stderr: string }>,
  signalTarget: DeploySignalTarget,
  scheduleRenewals: boolean,
): RemoteDeployLockRefresh {
  const controller = new AbortController();
  let refreshing = false;
  let stopping = false;
  let inFlight = Promise.resolve();
  let timer: Timer | undefined;
  let refreshFailure: DeployFailure | null = null;
  const abortForSignal = (signal: "SIGINT" | "SIGTERM") => {
    if (controller.signal.aborted) return;
    stopping = true;
    clearInterval(timer);
    controller.abort(new DeployFailure(signal === "SIGINT" ? 130 : 143, `deployment interrupted by ${signal}`));
  };
  const onSigint = () => abortForSignal("SIGINT");
  const onSigterm = () => abortForSignal("SIGTERM");
  const removeSignalHandlers = () => {
    signalTarget.removeListener("SIGINT", onSigint);
    signalTarget.removeListener("SIGTERM", onSigterm);
  };
  signalTarget.once("SIGINT", onSigint);
  signalTarget.once("SIGTERM", onSigterm);
  const recordRefreshFailure = (detail: string) => {
    if (refreshFailure || controller.signal.aborted) return;
    refreshFailure = new DeployFailure(
      9,
      `deployment lock on ${host} can no longer be proven: ${detail || "refresh failed"}`,
    );
    stopping = true;
    clearInterval(timer);
    controller.abort(refreshFailure);
    console.error(`fatal: ${refreshFailure.message}`);
  };
  const renewNow = async () => {
    if (refreshing || stopping) return;
    refreshing = true;
    inFlight = (async () => {
      try {
        const result = await executeRenew(controller.signal);
        if (result.exit !== 0) {
          recordRefreshFailure((result.stderr || result.stdout).trim());
        }
      } catch (error) {
        recordRefreshFailure(String(error));
      } finally {
        refreshing = false;
      }
    })();
    await inFlight;
  };
  if (scheduleRenewals) {
    timer = setInterval(() => void renewNow(), Math.floor(REMOTE_DEPLOY_LOCK_LEASE_SECONDS * 1000 / 3));
    timer.unref();
  }
  return {
    signal: controller.signal,
    renewNow,
    stop: () => {
      stopping = true;
      clearInterval(timer);
      removeSignalHandlers();
    },
    settled: async () => {
      await inFlight;
      if (refreshFailure) return refreshFailure;
      const reason = controller.signal.reason;
      return reason instanceof DeployFailure ? reason : null;
    },
  };
}

export function _startRemoteDeployLockRefreshForTest(
  executeRenew: (signal: AbortSignal) => Promise<{ exit: number; stdout: string; stderr: string }>,
  signalTarget: DeploySignalTarget,
): RemoteDeployLockHandle & {
  renewNow(): Promise<void>;
  stop(): Promise<DeployFailure | null>;
} {
  const refresh = createRemoteDeployLockRefresh("test-host", executeRenew, signalTarget, false);
  return {
    signal: refresh.signal,
    renewNow: () => refresh.renewNow(),
    stop: async () => {
      refresh.stop();
      return await refresh.settled();
    },
  };
}

async function startRemoteDeployLockRefresh(host: string, lockPath: string, owner: string): Promise<RemoteDeployLockHandle> {
  const key = `${host}\0${lockPath}\0${owner}`;
  const previous = remoteDeployLockRefreshes.get(key);
  if (previous) {
    previous.stop();
    remoteDeployLockRefreshes.delete(key);
    const previousFailure = await previous.settled();
    if (previousFailure) throw previousFailure;
  }

  const renew = _remoteDeployLockCommands(lockPath, owner).renew;
  const refresh = createRemoteDeployLockRefresh(
    host,
    (signal) => sshExec(host, renew, signal),
    process,
    true,
  );
  remoteDeployLockRefreshes.set(key, refresh);
  return { signal: refresh.signal };
}

async function stopRemoteDeployLockRefresh(
  host: string,
  lockPath: string,
  owner: string,
): Promise<DeployFailure | null> {
  const key = `${host}\0${lockPath}\0${owner}`;
  const refresh = remoteDeployLockRefreshes.get(key);
  if (!refresh) return null;
  refresh.stop();
  remoteDeployLockRefreshes.delete(key);
  return await refresh.settled();
}

export async function acquireRemoteDeployLock(
  host: string,
  lockPath: string,
  owner: string,
): Promise<RemoteDeployLockHandle> {
  const commands = _remoteDeployLockCommands(lockPath, owner);
  const result = await sshExec(host, commands.acquire);
  if (result.exit !== 0) {
    throw new DeployFailure(
      result.exit || 9,
      `cannot acquire deployment lock on ${host}\n${result.stdout}\n${result.stderr}`,
    );
  }
  return await startRemoteDeployLockRefresh(host, lockPath, owner);
}

export async function releaseRemoteDeployLock(
  host: string,
  lockPath: string,
  owner: string,
): Promise<void> {
  const refreshFailure = await stopRemoteDeployLockRefresh(host, lockPath, owner);
  const commands = _remoteDeployLockCommands(lockPath, owner);
  const result = await sshExec(host, commands.release);
  if (result.exit !== 0) {
    console.warn(`warning: failed to release deployment lock on ${host}: ${result.stderr.trim()}`);
  }
  if (refreshFailure) throw refreshFailure;
}

/** Local git HEAD to stamp into the deployed service's GIT_SHA, with the
 *  dirty-tree guard every deploy path shares. Refuses (exit 7) on
 *  uncommitted changes unless ROOST_ALLOW_DIRTY=1, because otherwise the
 *  shipped tree and the stamp disagree and the SPA's drift badge fires
 *  falsely until the next clean deploy. */
export function resolveLocalGitShaOrDie(cwd: string = process.cwd()): string {
  let sha = "";
  try {
    const result = Bun.spawnSync(["git", "rev-parse", "HEAD"], { cwd });
    if (result.exitCode !== 0) {
      throw new Error(result.stderr.toString().trim() || `git rev-parse exited ${result.exitCode}`);
    }
    sha = result.stdout.toString().trim();
    if (!/^[0-9a-f]{40}$/i.test(sha)) throw new Error("git rev-parse returned an invalid commit");
  } catch (error) {
    throw new DeployFailure(7, `cannot resolve the source commit: ${String(error)}`);
  }
  let isDirty: boolean;
  try {
    const result = Bun.spawnSync(["git", "status", "--porcelain"], { cwd });
    if (result.exitCode !== 0) {
      throw new Error(result.stderr.toString().trim() || `git status exited ${result.exitCode}`);
    }
    isDirty = result.stdout.toString().trim().length > 0;
  } catch (error) {
    throw new DeployFailure(7, `cannot verify the source working tree: ${String(error)}`);
  }
  if (!isDirty) return sha;
  if (process.env.ROOST_ALLOW_DIRTY === "1") {
    console.warn(`>> WARN: uncommitted changes — stamping GIT_SHA=${sha}-dirty (ROOST_ALLOW_DIRTY=1)`);
    return `${sha}-dirty`;
  }
  throw new DeployFailure(
    7,
    [
      "uncommitted changes in working tree.",
      "Commit first, OR re-run with ROOST_ALLOW_DIRTY=1 to ship the dirty state",
      "with a `-dirty` GIT_SHA suffix. Run `git status` to see what's pending.",
    ].join("\n"),
  );
}

export interface GitPublishTarget {
  branch: string;
  remote: string;
  mergeRef: string;
}

export function resolveGitPublishTargetOrDie(cwd: string): GitPublishTarget {
  const text = (args: string[], label: string): string => {
    const result = Bun.spawnSync(["git", ...args], { cwd });
    const value = result.stdout.toString().trim();
    if (result.exitCode !== 0 || !value || /[\r\n\0]/.test(value)) {
      throw new DeployFailure(
        7,
        `${label}: ${result.stderr.toString().trim() || `git exited ${result.exitCode}`}`,
      );
    }
    return value;
  };
  const branch = text(["symbolic-ref", "--quiet", "--short", "HEAD"], "source HEAD has no publishable branch");
  const remote = text(["config", "--get", `branch.${branch}.remote`], "source branch has no configured remote");
  const mergeRef = text(["config", "--get", `branch.${branch}.merge`], "source branch has no configured upstream ref");
  if (remote === "." || !mergeRef.startsWith("refs/heads/") || /\s/.test(mergeRef)
    || ["~", "^", ":", "?", "*", "[", "\\"].some((character) => mergeRef.includes(character))) {
    throw new DeployFailure(7, "source branch upstream is not a publishable remote branch");
  }
  return { branch, remote, mergeRef };
}

/** Prove a clean source HEAD is the exact tip of its refreshed configured
 * upstream before any POSIX host mutation. */
export function resolvePublishedGitShaOrDie(
  cwd: string,
  expectedSha?: string,
): string {
  const sha = resolveLocalGitShaOrDie(cwd);
  if (sha.endsWith("-dirty")) {
    throw new DeployFailure(7, "a published deploy requires a clean committed source snapshot");
  }
  if (expectedSha !== undefined && sha.toLowerCase() !== expectedSha.toLowerCase()) {
    throw new DeployFailure(
      7,
      `source HEAD ${sha.slice(0, 8)} does not match required build ${expectedSha.slice(0, 8)}`,
    );
  }
  const { remote, mergeRef } = resolveGitPublishTargetOrDie(cwd);
  const fetched = Bun.spawnSync(
    ["git", "fetch", "--quiet", "--no-tags", "--", remote, mergeRef],
    { cwd },
  );
  if (fetched.exitCode !== 0) {
    throw new DeployFailure(
      7,
      `cannot refresh source upstream: ${fetched.stderr.toString().trim() || `git fetch exited ${fetched.exitCode}`}`,
    );
  }
  const remoteShaResult = Bun.spawnSync(["git", "rev-parse", "FETCH_HEAD"], { cwd });
  const remoteSha = remoteShaResult.stdout.toString().trim();
  if (remoteShaResult.exitCode !== 0 || !/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/i.test(remoteSha)) {
    throw new DeployFailure(
      7,
      `cannot resolve refreshed source upstream: ${remoteShaResult.stderr.toString().trim() || `git exited ${remoteShaResult.exitCode}`}`,
    );
  }
  if (remoteSha.toLowerCase() !== sha.toLowerCase()) {
    throw new DeployFailure(
      7,
      `source HEAD ${sha.slice(0, 8)} is not the exact refreshed upstream tip ${remoteSha.slice(0, 8)}`,
    );
  }
  return sha;
}
