// Source checkout identity proofs for deploy and coordinator publication.
// deploy-exec.ts re-exports this stable surface for existing CLI callers.
// Pinned coordinator jobs prove an immutable local checkout without fetching.

import { lstatSync, realpathSync } from "node:fs";
import { resolve } from "node:path";
import type { WorkerUpdateFailure } from "@roost/shared/worker-update-operation";
import { DeployFailure } from "./deploy-exec.ts";

const FULL_GIT_SHA_RE = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i;
interface GitCommandResult {
  exitCode: number;
  stdout: Uint8Array;
  stderr: Uint8Array;
}


function pinnedSourceFailure(message: string): never {
  const workerUpdateFailure: WorkerUpdateFailure = {
    code: "source_unavailable",
    phase: "preflight",
    message,
    journal: null,
    expectedKeeper: null,
    observedKeeper: null,
    targetContract: null,
  };
  throw new DeployFailure(7, message, workerUpdateFailure);
}

function canonicalPinnedSourceRootOrDie(sourceRoot: string): string {
  if (!sourceRoot || /[\r\n\0]/.test(sourceRoot) || sourceRoot !== resolve(sourceRoot)) {
    pinnedSourceFailure("pinned source root must be a canonical absolute path");
  }
  try {
    const sourceStat = lstatSync(sourceRoot);
    if (sourceStat.isSymbolicLink() || !sourceStat.isDirectory() || realpathSync(sourceRoot) !== sourceRoot) {
      pinnedSourceFailure("pinned source root must be a canonical real directory");
    }
  } catch {
    pinnedSourceFailure("pinned source root is unavailable");
  }
  return sourceRoot;
}

function pinnedGitOutputOrDie(sourceRoot: string, args: string[], failure: string): string {
  let result: GitCommandResult;
  try {
    result = Bun.spawnSync(["git", ...args], { cwd: sourceRoot });
  } catch {
    pinnedSourceFailure(failure);
  }
  const output = result.stdout.toString().trim();
  if (result.exitCode !== 0 || !output || /[\r\n\0]/.test(output)) {
    pinnedSourceFailure(failure);
  }
  return output;
}

/** Proves an immutable coordinator checkout already contains exactly the
 * requested commit. This path never fetches, selects a branch, or permits a
 * dirty-tree override because retained job records reference this exact root. */
export function resolvePinnedSourceShaOrDie(sourceRoot: string, expectedSha: string): string {
  if (!FULL_GIT_SHA_RE.test(expectedSha)) {
    pinnedSourceFailure("pinned source target must be a full 40- or 64-hex commit");
  }
  const canonicalRoot = canonicalPinnedSourceRootOrDie(sourceRoot);
  const gitTopLevel = pinnedGitOutputOrDie(
    canonicalRoot,
    ["rev-parse", "--show-toplevel"],
    "pinned source root is not a Git checkout top level",
  );
  try {
    if (realpathSync(gitTopLevel) !== canonicalRoot) {
      pinnedSourceFailure("pinned source root is not a Git checkout top level");
    }
  } catch {
    pinnedSourceFailure("pinned source root is not a Git checkout top level");
  }
  let status: GitCommandResult;
  try {
    status = Bun.spawnSync(
      ["git", "status", "--porcelain=v1", "--untracked-files=all"],
      { cwd: canonicalRoot },
    );
  } catch {
    pinnedSourceFailure("pinned source checkout cannot be verified");
  }
  if (status.exitCode !== 0) {
    pinnedSourceFailure("pinned source checkout cannot be verified");
  }
  if (status.stdout.toString().length !== 0) {
    pinnedSourceFailure("pinned source checkout has uncommitted tracked or untracked files");
  }
  const head = pinnedGitOutputOrDie(
    canonicalRoot,
    ["rev-parse", "--verify", "HEAD"],
    "pinned source HEAD cannot be resolved",
  );
  if (!FULL_GIT_SHA_RE.test(head)) {
    pinnedSourceFailure("pinned source HEAD cannot be resolved");
  }
  if (head.toLowerCase() !== expectedSha.toLowerCase()) {
    pinnedSourceFailure("pinned source HEAD does not match the requested target");
  }
  return head.toLowerCase();
}

/** Local git HEAD to stamp into the deployed service's GIT_SHA, with the
 * dirty-tree guard every deploy path shares. Refuses (exit 7) on uncommitted
 * changes unless ROOST_ALLOW_DIRTY=1, which remains unavailable to pinned jobs. */
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
 * upstream before any ordinary POSIX host mutation. */
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
