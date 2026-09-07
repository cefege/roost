// Retiring the prior worker release at local-deploy settlement. Every release
// a real install has on disk was staged by rsync, so it is an ordinary
// directory rather than a git worktree; this pins that such a release is
// retired instead of failing settlement after the new release already serves.
import { expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, existsSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { _removeManagedPriorRelease } from "../src/deploy-local-journal-runtime.ts";

function stage(): { root: string; releaseRoot: string; repo: string } {
  const root = mkdtempSync(join(tmpdir(), "roost-release-retire-"));
  const releaseRoot = join(root, "releases", "worker");
  mkdirSync(releaseRoot, { recursive: true });
  const repo = join(root, "repo");
  mkdirSync(repo, { recursive: true });
  return { root, releaseRoot, repo };
}

function plainRelease(releaseRoot: string, name: string): string {
  const path = join(releaseRoot, name);
  mkdirSync(join(path, "apps", "worker", "src"), { recursive: true });
  writeFileSync(join(path, "apps", "worker", "src", "main.ts"), "export {};\n");
  return path;
}

test("an rsync-staged prior release is retired even though it is no git worktree", async () => {
  const { root, releaseRoot, repo } = stage();
  const prior = plainRelease(releaseRoot, "aaaaaaaa-1111");
  try {
    // `repo` is not a git repository at all, so the worktree query fails
    // exactly as it does when the release simply is not registered.
    await _removeManagedPriorRelease(repo, releaseRoot, prior);
    expect(existsSync(prior)).toBe(false);
    expect(existsSync(releaseRoot)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a release outside its release root is never removed", async () => {
  const { root, releaseRoot, repo } = stage();
  const outside = join(root, "not-a-release");
  mkdirSync(outside, { recursive: true });
  try {
    await _removeManagedPriorRelease(repo, releaseRoot, outside);
    expect(existsSync(outside)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("no prior release is a no-op", async () => {
  const { root, releaseRoot, repo } = stage();
  try {
    await _removeManagedPriorRelease(repo, releaseRoot, null);
    expect(existsSync(releaseRoot)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
