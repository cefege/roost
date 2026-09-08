// The upgrade-continuity gate: an install a tagged release created and the
// previous commit runs meets the working tree. It proves the coordinator opens
// the pre-existing database, the keeper keeps its pid and both channels, both
// PTY markers still paint in a browser, and the upgraded worker reports the
// keeper runtime the NEXT upgrade admits on. The destructive path stays
// opt-in: a keeper this release cannot adopt is refused, never discarded.

import { readFileSync } from "node:fs";
import { test, expect } from "./fixtures.ts";
import {
  keeperRuntimeOrThrow,
  openMarkedTerminals,
  runReleaseHandoff,
  targetKeeperClassification,
  waitForHeartbeatAfter,
  waitForKeeperReconciliationAfter,
  waitForPaintedMarkers,
  workerRow,
} from "./upgrade-probes.ts";

test("an existing install survives the working tree with its keeper and PTYs intact", async ({
  browser,
  install,
}, testInfo) => {
  const { stack } = install;
  const coordLog = install.coordLog();
  expect(install.releaseMigrations.length).toBeGreaterThan(0);
  expect(coordLog).not.toContain("Applied migration history is not an exact prefix");
  expect(coordLog).not.toContain("migration_failed");
  // The working tree carried the release's database forward instead of starting
  // a new one: only migrations the release never shipped ran on this boot.
  const upgradeMigrations = [...coordLog.matchAll(/"msg":"migration_applied","name":"([^"]+)"/g)]
    .map((match) => match[1]!);
  expect(upgradeMigrations.length).toBeGreaterThan(0);
  for (const applied of upgradeMigrations) {
    expect(install.releaseMigrations).not.toContain(applied);
  }

  const terminals = await openMarkedTerminals(browser, stack, testInfo);
  const before = keeperRuntimeOrThrow(workerRow(stack));
  expect(before.channel_count).toBe(terminals.length);
  const installedWorkerPid = stack.workerPid();
  expect(installedWorkerPid).toBeDefined();
  expect(workerRow(stack).gitSha).toBe(install.installedRelease.gitSha);

  // What the product's own admission requires of this release, decided before
  // the deploy runs so the deploy cannot pick whichever outcome it manages.
  expect(await targetKeeperClassification(stack, install.workingTreeGitSha))
    .toBe("worker-only-safe");

  const deploy = await runReleaseHandoff(install, installedWorkerPid!);

  // The keeper the upgraded worker found, proven fresh so this is not the row
  // the replaced worker left behind. Whatever else a deploy does, it may not
  // cost a live PTY: pid, epoch, channel count and bindings all hold.
  const after = await waitForKeeperReconciliationAfter(stack, before.reconciled_at_ms);
  expect(after.keeper_pid).toBe(before.keeper_pid);
  expect(after.keeper_epoch).toBe(before.keeper_epoch);
  expect(after.channel_count).toBe(before.channel_count);
  expect(after.binding_digest).toBe(before.binding_digest);

  expect(deploy.exitCode, `deploy failed: ${deploy.stderr}`).toBe(0);
  expect(deploy.stdout).toContain("keeper preserve cutover");
  // A different worker process now runs the new build, and the fresh
  // reconciliation above is the admission the next upgrade reads. Without it a
  // worker becomes permanently unupgradable.
  expect(workerRow(stack).gitSha).toBe(install.workingTreeGitSha);
  expect(readFileSync(deploy.workerPidFilePath, "utf8").trim())
    .not.toBe(String(installedWorkerPid));

  await waitForPaintedMarkers(browser, stack, testInfo, terminals);
});

test("a keeper the release cannot adopt is refused, not discarded", async ({
  browser,
  install,
  changedKeeperRelease,
}, testInfo) => {
  const { stack } = install;
  const terminals = await openMarkedTerminals(browser, stack, testInfo);
  const installedWorker = workerRow(stack);
  const before = keeperRuntimeOrThrow(installedWorker);
  const installedWorkerPid = stack.workerPid();
  expect(installedWorkerPid).toBeDefined();

  expect(await targetKeeperClassification(stack, install.workingTreeGitSha))
    .toBe("worker-only-safe");
  const refused = await runReleaseHandoff(install, installedWorkerPid!, changedKeeperRelease);
  expect(refused.exitCode).toBe(5);
  expect(refused.stderr).toContain("keeper update is blocked or unproven");
  expect(refused.stderr).toContain("--force-live");

  // Nothing was destroyed: the installed worker's next heartbeat still reports
  // the same keeper, channels and bindings, so this is state observed after
  // the refusal rather than the row that predates it.
  const after = await waitForHeartbeatAfter(stack, installedWorker.lastSeenMs);
  expect(after.keeper_pid).toBe(before.keeper_pid);
  expect(after.keeper_epoch).toBe(before.keeper_epoch);
  expect(after.channel_count).toBe(before.channel_count);
  expect(after.binding_digest).toBe(before.binding_digest);
  expect(stack.workerPid()).toBe(installedWorkerPid);
  expect(workerRow(stack).gitSha).toBe(install.installedRelease.gitSha);
  await waitForPaintedMarkers(browser, stack, testInfo, terminals);
});
