// Coordinator self-update journal, snapshot, rollback, and finalization contracts.
// Fixtures use real SQLite files and gzip archives while service/status operations
// are injected, so corruption and durable phase boundaries are exercised without
// touching the host's coordinator service.
import { describe, expect, test } from "bun:test";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
  coordinatorDeployRecoveryAction,
  coordinatorStagedReleasePathIsSafe,
  loadCoordinatorDeployJournal,
  parseCoordinatorDeployJournal,
} from "../src/coordinator-deploy-journal.ts";
import {
  beginCoordinatorDeployFinalization,
  finalizeCoordinatorDeploy,
  markCoordinatorFleetConverging,
  recoverCoordinatorDeploy,
  rollbackCoordinatorDeploy,
} from "../src/coordinator-deploy-recovery.ts";
import {
  coordinatorRestartCommand,
  coordinatorStopCommand,
} from "../src/coordinator-service-definition.ts";
import {
  PRIOR_SHA,
  TARGET_SHA,
  WORKER_FP,
  coordinatorFixture,
  databaseValue,
  statusReport,
  successfulRuntime,
  workerStatus,
} from "./coordinator-deploy-fixture.ts";

describe("coordinator deploy journal v2", () => {
  test("parses canonical database, snapshot, rollout, and worker identity", async () => {
    const fixture = await coordinatorFixture("prepared");
    try {
      expect(parseCoordinatorDeployJournal(
        JSON.stringify(fixture.journal),
        fixture.context,
      )).toEqual(fixture.journal);
      expect(() => parseCoordinatorDeployJournal(JSON.stringify({
        ...fixture.journal,
        databaseSnapshotSha256: "f".repeat(63),
      }), fixture.context)).toThrow("databaseSnapshotSha256");
      expect(() => parseCoordinatorDeployJournal(JSON.stringify({
        ...fixture.journal,
        databasePath: join(fixture.root, "other.db"),
      }), fixture.context)).toThrow("does not match databasePath");
      expect(() => parseCoordinatorDeployJournal(JSON.stringify({
        ...fixture.journal,
        targetWorkerFingerprints: ["b".repeat(64), "a".repeat(64)],
      }), fixture.context)).toThrow("canonical order");
      expect(coordinatorStagedReleasePathIsSafe(
        fixture.context.releaseRoot,
        fixture.journal.stagedReleasePath,
        TARGET_SHA,
      )).toBeTrue();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("never turns interrupted fleet convergence into a coordinator-only commit", () => {
    expect(coordinatorDeployRecoveryAction("prepared")).toBe("clean-prepared");
    expect(coordinatorDeployRecoveryAction("activating")).toBe("rollback-prior");
    expect(coordinatorDeployRecoveryAction("fleet-converging")).toBe("rollback-prior");
    expect(coordinatorDeployRecoveryAction("finalizing")).toBe("finish-finalize");
  });
});

describe("coordinator snapshot rollback", () => {
  test("stops first and refuses a corrupt archive without replacing the database", async () => {
    const fixture = await coordinatorFixture("activating");
    const events: string[] = [];
    try {
      writeFileSync(fixture.journal.databaseSnapshotPath, "corrupt snapshot");
      await expect(rollbackCoordinatorDeploy(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(PRIOR_SHA, events),
      )).rejects.toThrow();
      expect(events).toEqual(["stop"]);
      expect(databaseValue(fixture.journal.databasePath)).toBe("target");
      expect(readFileSync(fixture.journal.servicePath, "utf8")).toBe(fixture.targetDefinition);
      expect(existsSync(fixture.journalPath)).toBeTrue();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("rolls back target startup failure through atomic DB restore and restart", async () => {
    const fixture = await coordinatorFixture("activating");
    const events: string[] = [];
    try {
      await rollbackCoordinatorDeploy(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(PRIOR_SHA, events),
      );
      expect(events).toEqual(["stop", "restart"]);
      expect(databaseValue(fixture.journal.databasePath)).toBe("prior");
      expect(readFileSync(fixture.journal.servicePath, "utf8")).toBe(fixture.priorDefinition);
      expect(existsSync(fixture.journalPath)).toBeFalse();
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeFalse();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("retains rollback state until the restarted service proves the prior SHA", async () => {
    const fixture = await coordinatorFixture("activating");
    try {
      await expect(rollbackCoordinatorDeploy(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(TARGET_SHA),
      )).rejects.toThrow(PRIOR_SHA);
      expect(databaseValue(fixture.journal.databasePath)).toBe("prior");
      expect(existsSync(fixture.journalPath)).toBeTrue();
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeTrue();
      await rollbackCoordinatorDeploy(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(PRIOR_SHA),
      );
      expect(existsSync(fixture.journalPath)).toBeFalse();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("rolls the fleet back before coordinator state after a post-start interruption", async () => {
    const fixture = await coordinatorFixture("fleet-converging");
    const events: string[] = [];
    try {
      await recoverCoordinatorDeploy(fixture.journalPath, fixture.context, {
        ...successfulRuntime(PRIOR_SHA, events),
        rollbackFleet: async () => { events.push("fleet"); },
      });
      expect(events).toEqual(["fleet", "stop", "restart"]);
      expect(databaseValue(fixture.journal.databasePath)).toBe("prior");
      expect(existsSync(fixture.journalPath)).toBeFalse();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });
});

describe("coordinator fleet finalization", () => {
  test("retains journal and snapshot at target health", async () => {
    const fixture = await coordinatorFixture("activating");
    try {
      const converging = await markCoordinatorFleetConverging(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(TARGET_SHA),
      );
      expect(converging.phase).toBe("fleet-converging");
      expect(loadCoordinatorDeployJournal(fixture.journalPath, fixture.context)?.phase)
        .toBe("fleet-converging");
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeTrue();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("leaves activating rollback state when target startup never proves health", async () => {
    const fixture = await coordinatorFixture("activating");
    try {
      await expect(markCoordinatorFleetConverging(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(PRIOR_SHA),
      )).rejects.toThrow(TARGET_SHA);
      expect(loadCoordinatorDeployJournal(fixture.journalPath, fixture.context)?.phase)
        .toBe("activating");
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeTrue();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("refuses a final decision without the exact worker and keeper set", async () => {
    const fixture = await coordinatorFixture("fleet-converging");
    const runtime = successfulRuntime(TARGET_SHA);
    runtime.readStatus = async () => statusReport(TARGET_SHA, [
      workerStatus(WORKER_FP, TARGET_SHA, true),
      workerStatus("b".repeat(64)),
    ]);
    try {
      await expect(beginCoordinatorDeployFinalization(
        fixture.journalPath,
        fixture.context,
        runtime,
      )).rejects.toThrow("registered worker set");
      expect(loadCoordinatorDeployJournal(fixture.journalPath, fixture.context)?.phase)
        .toBe("fleet-converging");
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeTrue();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("refuses finalization when a planned worker is not routable", async () => {
    const fixture = await coordinatorFixture("fleet-converging");
    const runtime = successfulRuntime(TARGET_SHA);
    runtime.readRoutableWorkers = async () => new Set();
    try {
      await expect(beginCoordinatorDeployFinalization(
        fixture.journalPath,
        fixture.context,
        runtime,
      )).rejects.toThrow("not coordinator-routable");
      expect(loadCoordinatorDeployJournal(fixture.journalPath, fixture.context)?.phase)
        .toBe("fleet-converging");
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });

  test("explicitly checkpoints global finalization before fleet cleanup", async () => {
    const fixture = await coordinatorFixture("fleet-converging");
    const events: string[] = [];
    try {
      const finalizing = await beginCoordinatorDeployFinalization(
        fixture.journalPath,
        fixture.context,
        successfulRuntime(TARGET_SHA),
      );
      expect(finalizing.phase).toBe("finalizing");
      expect(existsSync(fixture.journalPath)).toBeTrue();
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeTrue();
      await expect(recoverCoordinatorDeploy(
        fixture.journalPath, fixture.context, successfulRuntime(TARGET_SHA),
      )).rejects.toThrow("requires fleet finalization");

      await finalizeCoordinatorDeploy(
        fixture.journalPath,
        fixture.context,
        async () => { events.push("fleet-finalized"); },
        successfulRuntime(TARGET_SHA, events),
      );
      expect(events).toEqual(["fleet-finalized"]);
      expect(existsSync(fixture.journalPath)).toBeFalse();
      expect(existsSync(fixture.journal.databaseSnapshotPath)).toBeFalse();
    } finally {
      rmSync(fixture.root, { recursive: true, force: true });
    }
  });
});

describe("coordinator configured service identity", () => {
  test("uses configured service identities for stop and restart", () => {
    expect(coordinatorStopCommand("linux", "custom-coord"))
      .toContain("stop 'custom-coord.service'");
    expect(coordinatorRestartCommand("/tmp/custom.service", "linux", "custom-coord"))
      .toContain("restart 'custom-coord.service'");
    expect(coordinatorRestartCommand(
      "/Users/test/Library/LaunchAgents/custom.plist",
      "darwin",
      "org.example.custom",
    )).toContain("gui/$uid/'org.example.custom'");
  });
});
