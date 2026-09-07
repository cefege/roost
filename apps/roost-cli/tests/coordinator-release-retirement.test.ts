// Retiring the prior coordinator release at deploy settlement. A release
// staged by rsync is an ordinary directory, not a git worktree, and this step
// runs AFTER the new coordinator is already live — so refusing such a release
// reports a failed deploy of a coordinator that is actually serving.
import { expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, existsSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { retirePriorCoordinatorRelease } from "../src/coordinator-deploy-release.ts";
import type { CoordinatorDeployJournalV2 } from "../src/coordinator-deploy-journal-schema.ts";

const PRIOR_SHA = "a".repeat(40);
const TARGET_SHA = "b".repeat(40);

function release(releaseRoot: string, sha: string): string {
  const path = join(releaseRoot, sha);
  mkdirSync(join(path, "apps", "coord", "src"), { recursive: true });
  writeFileSync(join(path, "apps", "coord", "src", "main.ts"), "export {};\n");
  return path;
}

function journalFor(
  root: string,
  sourceReleasePath: string,
  stagedReleasePath: string,
): CoordinatorDeployJournalV2 {
  return {
    schemaVersion: 3,
    phase: "finalizing",
    admissionRecordedAtMs: 1,
    rolloutId: "rollout",
    targetWorkerFingerprints: [],
    workerKeeperPlans: [],
    priorDefinitionBase64: "",
    priorDefinitionMode: 0o600,
    priorSha: PRIOR_SHA,
    targetSha: TARGET_SHA,
    servicePath: join(root, "coord.service"),
    sourceReleasePath,
    stagingRepoPath: join(root, "repo"),
    stagedReleasePath,
    databasePath: join(root, "coordinator_v2.db"),
    databaseSnapshotPath: join(root, "snapshot.db"),
    databaseSnapshotSha256: "c".repeat(64),
  };
}

test("an rsync-staged prior coordinator release is retired, not refused", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-coord-retire-"));
  const releaseRoot = join(root, "releases", "coord");
  mkdirSync(releaseRoot, { recursive: true });
  const prior = release(releaseRoot, PRIOR_SHA);
  const staged = release(releaseRoot, TARGET_SHA);
  try {
    await retirePriorCoordinatorRelease(
      releaseRoot,
      journalFor(root, prior, staged),
      "linux",
    );
    expect(existsSync(prior)).toBe(false);
    expect(existsSync(staged)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a prior release outside the release root is left alone", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-coord-retire-outside-"));
  const releaseRoot = join(root, "releases", "coord");
  mkdirSync(releaseRoot, { recursive: true });
  const staged = release(releaseRoot, TARGET_SHA);
  const outside = join(root, "elsewhere");
  mkdirSync(outside, { recursive: true });
  try {
    await retirePriorCoordinatorRelease(
      releaseRoot,
      journalFor(root, outside, staged),
      "linux",
    );
    expect(existsSync(outside)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
