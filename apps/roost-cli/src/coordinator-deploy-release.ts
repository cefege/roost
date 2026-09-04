// Flushes, removes, and retires immutable coordinator release worktrees.
// Coordinator activation and recovery call this module only after journal path
// confinement has been validated; worker service ownership protects a shared
// prior checkout from retirement while a local worker still uses it.
import { workerServicePath } from "@roost/shared/paths";
import { flushDurablePath } from "@roost/shared/durability";
import {
  existsSync,
  lstatSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { run } from "./deploy-exec.ts";
import {
  coordinatorReleasePathIsConfined,
  coordinatorStagedReleasePathIsSafe,
  type CoordinatorDeployJournalV2,
} from "./coordinator-deploy-journal.ts";
import { coordinatorRepoFromService } from "./coordinator-service-definition.ts";

export async function removeStagedCoordinatorRelease(
  releaseRoot: string,
  stagingRepoPath: string,
  stagedReleasePath: string,
  targetSha: string,
): Promise<void> {
  if (!coordinatorStagedReleasePathIsSafe(releaseRoot, stagedReleasePath, targetSha)
    || realpathSync(releaseRoot) !== releaseRoot) {
    throw new Error(`refusing to remove unsafe staged coordinator path ${stagedReleasePath}`);
  }
  if (!existsSync(stagedReleasePath)) {
    await flushDurablePath(releaseRoot);
    return;
  }

  const stagedEntry = lstatSync(stagedReleasePath);
  if (stagedEntry.isSymbolicLink() || !stagedEntry.isDirectory()) {
    rmSync(stagedReleasePath, { force: true });
    await flushDurablePath(releaseRoot);
    return;
  }
  const canonicalStage = realpathSync(stagedReleasePath);
  if (canonicalStage !== stagedReleasePath
    || !coordinatorStagedReleasePathIsSafe(releaseRoot, canonicalStage, targetSha)) {
    throw new Error("refusing to remove staged coordinator path through a symbolic link");
  }

  let removedByGit = false;
  if (existsSync(join(stagingRepoPath, ".git"))
    && realpathSync(stagingRepoPath) === stagingRepoPath) {
    const removed = await run(["git", "worktree", "remove", "--force", stagedReleasePath], {
      cwd: stagingRepoPath,
      quiet: true,
    });
    removedByGit = removed.exit === 0;
  }
  if (!removedByGit) rmSync(stagedReleasePath, { recursive: true, force: true });
  await flushDurablePath(releaseRoot);
}

export async function flushCoordinatorReleaseTree(
  releaseRoot: string,
  stagedReleasePath: string,
  targetSha: string,
): Promise<void> {
  if (!coordinatorStagedReleasePathIsSafe(releaseRoot, stagedReleasePath, targetSha)
    || realpathSync(releaseRoot) !== releaseRoot
    || realpathSync(stagedReleasePath) !== stagedReleasePath
    || !lstatSync(stagedReleasePath).isDirectory()) {
    throw new Error("staged coordinator release is not a confined real directory");
  }
  const pending = [stagedReleasePath];
  const directories: string[] = [];
  while (pending.length > 0) {
    const directory = pending.pop()!;
    directories.push(directory);
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const metadata = lstatSync(path);
      if (metadata.isSymbolicLink()) continue;
      if (metadata.isDirectory()) {
        pending.push(path);
      } else if (metadata.isFile()) {
        await flushDurablePath(path);
      } else {
        throw new Error(`staged coordinator release contains unsupported entry ${path}`);
      }
    }
  }
  for (let index = directories.length - 1; index >= 0; index--) {
    await flushDurablePath(directories[index]!);
  }
  await flushDurablePath(releaseRoot);
}

export async function retirePriorCoordinatorRelease(
  releaseRoot: string,
  journal: CoordinatorDeployJournalV2,
  platform: "darwin" | "linux",
): Promise<void> {
  const source = journal.sourceReleasePath;
  if (!coordinatorReleasePathIsConfined(releaseRoot, source)
    || dirname(source) !== releaseRoot
    || !existsSync(journal.stagedReleasePath)
    || !existsSync(source)) {
    return;
  }
  if (realpathSync(source) !== source) {
    throw new Error(`prior coordinator release is not canonical: ${source}`);
  }
  const workerPath = workerServicePath(process.env, platform);
  let workerRepo: string | null;
  if (!existsSync(workerPath)) {
    workerRepo = null;
  } else {
    const parsed = coordinatorRepoFromService(readFileSync(workerPath, "utf8"), platform);
    if (!parsed) {
      throw new Error("cannot prove whether the prior coordinator release is still used by the worker");
    }
    const resolvedWorkerRepo = resolve(parsed);
    workerRepo = existsSync(resolvedWorkerRepo) ? realpathSync(resolvedWorkerRepo) : resolvedWorkerRepo;
  }
  if (source === workerRepo) return;
  const removed = await run(["git", "worktree", "remove", "--force", source], {
    cwd: journal.stagedReleasePath,
    quiet: true,
  });
  if (removed.exit !== 0) {
    throw new Error(
      `cannot retire prior coordinator release ${source}: ${removed.stderr.trim() || `exit ${removed.exit}`}`,
    );
  }
  await flushDurablePath(releaseRoot);
}
