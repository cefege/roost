// readGitBranch: resolves the current branch for a repo cwd, null otherwise.
// Drives real `git` — this test file lives inside the roost git repo, so
// process.cwd() is a repo; "/" is not.

import { test, expect } from "bun:test";
import { readGitBranch } from "../src/git-branch.ts";

test("readGitBranch resolves a branch inside the repo", async () => {
  const branch = await readGitBranch(process.cwd());
  expect(typeof branch).toBe("string");
  expect((branch ?? "").length).toBeGreaterThan(0);
});

test("readGitBranch returns null outside any repo", async () => {
  expect(await readGitBranch("/")).toBeNull();
});
