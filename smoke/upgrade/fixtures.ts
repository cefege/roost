// Builds the install an upgrade run acts on: a coordinator database a tagged
// release created and migrated, a coordinator from the working tree booted over
// it, and workers still running the previously deployed commit. Specs receive
// that stack plus both staged releases; teardown stops the deployed worker,
// the stack, and both git worktrees.

import { test as base, expect } from "@playwright/test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import {
  REPOSITORY_ROOT,
  logTail,
  startCoordinatorService,
  stopChild,
  waitFor,
} from "../terminal/stack-runtime.ts";
import { startTerminalTestStack, type TerminalTestStack } from "../terminal/stack.ts";
import {
  priorCommitRef,
  releaseTagRef,
  resolveGitSha,
  stageRelease,
  type StagedRelease,
} from "./release-checkouts.ts";

const RELEASE_COORD_START_TIMEOUT_MS = 30_000;

export interface UpgradeInstall {
  stack: TerminalTestStack;
  /** Commit the install runs before the upgrade. */
  installedRelease: StagedRelease;
  /** Release that created the coordinator database. */
  databaseRelease: StagedRelease;
  /** Working-tree build identity the upgrade deploys. */
  workingTreeGitSha: string;
  /** Migrations the release's coordinator had applied before the upgrade. */
  releaseMigrations: readonly string[];
  /** Scratch root the deploy writes its worker pid file into. */
  stateRoot: string;
  /** Coordinator log written while the working tree booted over that database. */
  coordLog(): string;
}

type UpgradeFixtures = {
  install: UpgradeInstall;
  /** A release whose keeper bundle differs from the running keeper's. */
  changedKeeperRelease: StagedRelease;
};

export const test = base.extend<UpgradeFixtures>({
  install: async ({}, use, testInfo) => {
    const databaseRelease = stageRelease(releaseTagRef(), "release");
    let installedRelease: StagedRelease | undefined;
    let stack: TerminalTestStack | undefined;
    const stateRoot = mkdtempSync(join("/tmp", "roost-upgrade-state-"));
    try {
      installedRelease = stageRelease(priorCommitRef(), "installed");
      const coordDbPath = join(stateRoot, "coord.db");
      const releaseMigrations = await createReleaseDatabase(
        databaseRelease,
        coordDbPath,
        stateRoot,
      );
      const workingTreeGitSha = resolveGitSha("HEAD");
      const started = await startTerminalTestStack({
        coordRelease: { sourceRoot: REPOSITORY_ROOT, gitSha: workingTreeGitSha },
        workerRelease: installedRelease,
        coordDbPath,
      });
      stack = started;
      await use({
        stack: started,
        installedRelease,
        databaseRelease,
        workingTreeGitSha,
        releaseMigrations,
        stateRoot,
        coordLog: () => readFileSync(started.coordLogPath, "utf8"),
      });
    } finally {
      if (stack && testInfo.status !== testInfo.expectedStatus) {
        await testInfo.attach("coord.log", {
          body: readFileSync(stack.coordLogPath),
          contentType: "text/plain",
        });
        await testInfo.attach("worker.log", {
          body: readFileSync(stack.workerLogPath),
          contentType: "text/plain",
        });
      }
      if (stack) await stack.stop().catch(() => undefined);
      rmSync(stateRoot, { recursive: true, force: true });
      installedRelease?.remove();
      databaseRelease.remove();
    }
  },
  changedKeeperRelease: async ({}, use) => {
    // A distinct implementation identity is the whole precondition: the digest
    // names a keeper bundle, and any value that is not the running keeper's
    // makes this release one that cannot adopt it.
    const digest = new Bun.CryptoHasher("sha256")
      .update("roost-upgrade-changed-keeper-bundle")
      .digest("hex");
    const release = stageRelease(priorCommitRef(), "changed-keeper", digest);
    try {
      await use(release);
    } finally {
      release.remove();
    }
  },
});

/** Run the tagged release's coordinator once so the database carries the schema
 *  and migration history that release shipped, then stop it. This is the
 *  pre-existing install every upgrade meets on real hardware. */
async function createReleaseDatabase(
  release: StagedRelease,
  dbPath: string,
  stateRoot: string,
): Promise<readonly string[]> {
  const home = join(stateRoot, "release-home");
  const tmpDir = join(stateRoot, "release-tmp");
  const logPath = join(stateRoot, "release-coord.log");
  mkdirSync(home, { recursive: true });
  mkdirSync(tmpDir, { recursive: true });
  const coord = startCoordinatorService({
    bunExecutable: process.env.ROOST_TEST_BUN ?? "bun",
    sourceRoot: release.sourceRoot,
    root: stateRoot,
    home,
    tmpDir,
    bind: "127.0.0.1:0",
    dbPath,
    logPath,
    gitSha: release.gitSha,
  });
  try {
    await waitFor("release coordinator startup", RELEASE_COORD_START_TIMEOUT_MS, () =>
      /"msg":"listening"/.test(logTail(logPath)) ? true : undefined,
    ).catch((error: unknown) => {
      throw new Error(`${String(error)}\nrelease coord log:\n${logTail(logPath)}`);
    });
  } finally {
    await stopChild(coord);
  }
  // Full file, not a tail: the release applies its whole migration set on
  // first boot and the count is the assertion the upgrade compares against.
  const applied = readFileSync(logPath, "utf8")
    .matchAll(/"msg":"migration_applied","name":"([^"]+)"/g);
  return [...applied].map((match) => match[1]!);
}

export { expect };
