// Coordinator self-update journal, snapshot, rollback, and finalization contracts.
// Fixtures use real SQLite files and gzip archives while service/status operations
// are injected, so corruption and durable phase boundaries are exercised without
// touching the host's coordinator service.
import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  coordinatorDatabaseSnapshotPath,
  coordinatorDeployRecoveryAction,
  coordinatorStagedReleasePathIsSafe,
  loadCoordinatorDeployJournal,
  parseCoordinatorDeployJournal,
  writeCoordinatorDeployJournal,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV2,
} from "../src/coordinator-deploy-journal.ts";
import {
  beginCoordinatorDeployFinalization,
  finalizeCoordinatorDeploy,
  markCoordinatorFleetConverging,
  recoverCoordinatorDeploy,
  rollbackCoordinatorDeploy,
  type CoordinatorDeployRecoveryOptions,
} from "../src/coordinator-deploy-recovery.ts";
import { createCoordinatorRollbackSnapshot } from "../src/coordinator-deploy-snapshot.ts";
import { coordinatorRestartCommand, coordinatorStopCommand } from "../src/coordinator-service-definition.ts";
import type { StatusReport, WorkerStatus } from "../src/status.ts";

const PRIOR_SHA = "1".repeat(40);
const TARGET_SHA = "2".repeat(40);
const WORKER_FP = "a".repeat(64);
const ROLLOUT_ID = "12345678-1234-4123-8123-123456789abc";
const RELEASE_ID = "87654321-4321-4321-8321-cba987654321";

interface CoordinatorFixture {
  root: string;
  journalPath: string;
  context: CoordinatorDeployJournalContext;
  journal: CoordinatorDeployJournalV2;
  priorDefinition: string;
  targetDefinition: string;
}

function serviceDefinition(repo: string, sha: string, databasePath: string): string {
  return [
    "[Service]",
    `WorkingDirectory=${repo}`,
    `Environment="ROOST_GIT_SHA=${sha}"`,
    `Environment="ROOST_COORDINATOR_DB=${databasePath}"`,
  ].join("\n");
}

async function coordinatorFixture(
  phase: CoordinatorDeployJournalV2["phase"],
  targetWorkerFingerprints: string[] = [WORKER_FP],
): Promise<CoordinatorFixture> {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "roost-coordinator-deploy-")));
  const serviceRoot = join(root, "service");
  const releaseRoot = join(serviceRoot, "releases", "coord");
  const transactionRoot = join(serviceRoot, "transactions");
  const sourceReleasePath = join(root, "source");
  const stagingRepoPath = join(root, "staging-repo");
  const stagedReleasePath = join(releaseRoot, `${TARGET_SHA}-${RELEASE_ID}`);
  const stateRoot = join(root, "state");
  const databasePath = join(stateRoot, "coordinator_v2.db");
  const servicePath = join(root, "roost-coord.service");
  const journalPath = join(transactionRoot, "coordinator-deploy.json");
  for (const directory of [
    releaseRoot,
    transactionRoot,
    sourceReleasePath,
    stagingRepoPath,
    stagedReleasePath,
    stateRoot,
  ]) mkdirSync(directory, { recursive: true });
  const priorDefinition = serviceDefinition(sourceReleasePath, PRIOR_SHA, databasePath);
  const targetDefinition = serviceDefinition(stagedReleasePath, TARGET_SHA, databasePath);
  writeFileSync(servicePath, priorDefinition, { mode: 0o600 });
  chmodSync(servicePath, 0o600);

  const liveDatabase = new Database(databasePath);
  liveDatabase.exec("CREATE TABLE rollout_state (value TEXT NOT NULL)");
  liveDatabase.query("INSERT INTO rollout_state VALUES (?)").run("prior");
  const databaseSnapshotPath = coordinatorDatabaseSnapshotPath(transactionRoot, ROLLOUT_ID);
  const snapshot = await createCoordinatorRollbackSnapshot(databasePath, databaseSnapshotPath);
  liveDatabase.query("UPDATE rollout_state SET value = ?").run("target");
  liveDatabase.close(true);

  if (phase !== "prepared") writeFileSync(servicePath, targetDefinition, { mode: 0o600 });
  const journal: CoordinatorDeployJournalV2 = {
    schemaVersion: 2,
    phase,
    rolloutId: ROLLOUT_ID,
    targetWorkerFingerprints,
    priorDefinitionBase64: Buffer.from(priorDefinition).toString("base64"),
    priorDefinitionMode: 0o600,
    priorSha: PRIOR_SHA,
    targetSha: TARGET_SHA,
    servicePath,
    sourceReleasePath,
    stagingRepoPath,
    stagedReleasePath,
    databasePath,
    databaseSnapshotPath,
    databaseSnapshotSha256: snapshot.sha256,
  };
  const context = { servicePath, releaseRoot, transactionRoot, platform: "linux" as const };
  await writeCoordinatorDeployJournal(journalPath, journal);
  return { root, journalPath, context, journal, priorDefinition, targetDefinition };
}

function workerStatus(
  fingerprint: string = WORKER_FP,
  gitSha: string | null = TARGET_SHA,
  keeperState: WorkerStatus["keeperState"] = "current",
): WorkerStatus {
  return {
    fingerprint,
    label: "worker",
    os: "linux",
    reachableAddr: "worker.example.test",
    gitSha,
    keeperState,
    keeperBuild: PRIOR_SHA,
    lastSeenMs: 10,
    ageMs: 0,
    stale: false,
  };
}

function statusReport(sha: string | null, workers: WorkerStatus[] = []): StatusReport {
  return {
    tailscale: { required: false, state: "disabled", fqdn: null, running: false },
    coordAgentLoaded: true,
    workerAgentLoaded: true,
    coord: { reachable: sha !== null, gitSha: sha },
    workers,
    tlsMode: "direct",
    url: "https://coordinator.example.test:4102",
    handoff: null,
  };
}

function successfulRuntime(
  sha: string,
  events: string[] = [],
): CoordinatorDeployRecoveryOptions {
  return {
    readStatus: async () => statusReport(sha, [workerStatus()]),
    runCommand: async (command) => {
      const script = command.at(-1) ?? "";
      if (script.includes(" is-enabled ")) return { exit: 0, stdout: "enabled\n", stderr: "" };
      if (script.includes(" stop ")) events.push("stop");
      if (script.includes(" restart ")) events.push("restart");
      return { exit: 0, stdout: "", stderr: "" };
    },
    now: () => 0,
    verifyTimeoutMs: 0,
  };
}

function databaseValue(path: string): string {
  const sqlite = new Database(path, { readonly: true });
  try {
    return sqlite.query<{ value: string }, []>("SELECT value FROM rollout_state").get()!.value;
  } finally {
    sqlite.close(true);
  }
}

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
      workerStatus(WORKER_FP, TARGET_SHA, "stale"),
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
