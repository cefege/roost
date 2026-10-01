// Enrollment checkout regression coverage.
// `_deployJoinedPosixWorker` still deploys a joined worker's clean detached
// coordinator commit, so that half is exercised here.
//
// The sibling-checkout half of this file is GONE, and so is its test.
// `join.sh` used to clone a clean worker checkout beside a dirty source and pin
// it to the coordinator's commit before running the v2 CLI; that spine was
// deleted in `d9a4c038` because every step of it was a v2 artefact. The v3
// script's own properties are pinned in `crates/roost-cli/tests/join_script.rs`.
// A test left behind here asserts a command trace the script no longer emits,
// which is a red gate naming a deleted feature.
import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  _deployJoinedPosixWorker,
} from "../src/join.ts";
import type { JournaledKeeperUpdateCallbacks } from "../src/direct-keeper-update.ts";

const temporaryRoots: string[] = [];


function git(cwd: string, args: string[]): string {
  const result = Bun.spawnSync(["git", ...args], { cwd });
  if (result.exitCode !== 0) {
    throw new Error(result.stderr.toString());
  }
  return result.stdout.toString().trim();
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

test("deploys join.sh's clean detached coordinator commit", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-join-detached-"));
  temporaryRoots.push(root);
  git(root, ["init", "--quiet"]);
  git(root, ["config", "user.email", "test@example.com"]);
  git(root, ["config", "user.name", "Roost Test"]);
  writeFileSync(join(root, "README"), "join\n");
  git(root, ["add", "README"]);
  git(root, ["commit", "--quiet", "-m", "coordinator commit"]);
  const coordinatorSha = git(root, ["rev-parse", "HEAD"]);
  git(root, ["checkout", "--quiet", "--detach", coordinatorSha]);
  let deployedSha: string | undefined;
  const keeperCallbacks: JournaledKeeperUpdateCallbacks = {
    apply: async () => undefined,
    prove: async () => undefined,
  };

  await _deployJoinedPosixWorker(
    root,
    async (_host, options) => { deployedSha = options.gitSha; },
    keeperCallbacks,
  );

  expect(deployedSha).toBe(coordinatorSha);
});

