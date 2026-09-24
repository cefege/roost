// A deploy the coordinator starts (catch-up, Machines "Update") runs from the
// coordinator's detached release worktree. This pins that such a deploy proves
// the installed coordinator release — its service WorkingDirectory and
// ROOST_GIT_SHA — rather than requiring a branch at the upstream tip.
import { expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { resolveCoordinatorReleaseGitShaOrDie } from "../src/deploy-coordinator-release.ts";
import { DeployFailure } from "../src/deploy-exec.ts";

interface ReleaseFixture {
  root: string;
  repo: string;
  release: string;
  sha: string;
  unit: string;
}

function runGit(repo: string, args: string[]): string {
  const result = Bun.spawnSync(["git", ...args], { cwd: repo, stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new Error(new TextDecoder().decode(result.stderr));
  }
  return new TextDecoder().decode(result.stdout).trim();
}

function stageRelease(): ReleaseFixture {
  const root = mkdtempSync(join(tmpdir(), "roost-coord-release-"));
  const repo = join(root, "repo");
  runGit(root, ["init", "-q", repo]);
  runGit(repo, ["config", "user.email", "test@example.com"]);
  runGit(repo, ["config", "user.name", "Roost Test"]);
  writeFileSync(join(repo, "README"), "release\n");
  runGit(repo, ["add", "README"]);
  runGit(repo, ["commit", "-qm", "fixture"]);
  const release = join(root, "release");
  runGit(repo, ["worktree", "add", "--detach", release, "HEAD"]);
  const sha = runGit(repo, ["rev-parse", "HEAD"]);
  const unit = join(root, "roost-coord.service");
  writeFileSync(
    unit,
    `[Service]\nWorkingDirectory=${release}\nEnvironment="ROOST_GIT_SHA=${sha}"\n`,
  );
  return { root, repo, release, sha, unit };
}

function expectRefusal(prove: () => unknown, fragment: string): void {
  let caught: unknown;
  try {
    prove();
  } catch (error) {
    caught = error;
  }
  expect(caught).toBeInstanceOf(DeployFailure);
  expect((caught as DeployFailure).exitCode).toBe(7);
  expect((caught as DeployFailure).message).toContain(fragment);
}

test("a detached coordinator release at its installed SHA is admitted without any upstream", () => {
  const fixture = stageRelease();
  try {
    const service = { path: fixture.unit, platform: "linux" as const };
    expect(resolveCoordinatorReleaseGitShaOrDie(fixture.release, fixture.sha, service))
      .toBe(fixture.sha);
  } finally {
    rmSync(fixture.root, { recursive: true, force: true });
  }
});

test("a checkout that is not the installed coordinator release is refused", () => {
  const fixture = stageRelease();
  try {
    const other = join(fixture.root, "other");
    runGit(fixture.repo, ["worktree", "add", "--detach", other, "HEAD"]);
    const service = { path: fixture.unit, platform: "linux" as const };
    expectRefusal(
      () => resolveCoordinatorReleaseGitShaOrDie(other, fixture.sha, service),
      "is not the installed coordinator release",
    );
  } finally {
    rmSync(fixture.root, { recursive: true, force: true });
  }
});

test("an expected build the installed coordinator does not run is refused", () => {
  const fixture = stageRelease();
  try {
    const service = { path: fixture.unit, platform: "linux" as const };
    expectRefusal(
      () => resolveCoordinatorReleaseGitShaOrDie(fixture.release, "c".repeat(40), service),
      "installed coordinator runs",
    );
  } finally {
    rmSync(fixture.root, { recursive: true, force: true });
  }
});

test("a dirty coordinator release is refused", () => {
  const fixture = stageRelease();
  try {
    writeFileSync(join(fixture.release, "stray"), "uncommitted\n");
    const service = { path: fixture.unit, platform: "linux" as const };
    expectRefusal(
      () => resolveCoordinatorReleaseGitShaOrDie(fixture.release, fixture.sha, service),
      "uncommitted changes",
    );
  } finally {
    rmSync(fixture.root, { recursive: true, force: true });
  }
});
