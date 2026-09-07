// Builds durable coordinator deploy fixtures around real temporary SQLite state.
// Coordinator deployment tests use these factories to exercise recovery and
// finalization without touching the host coordinator service.
import { Database } from "bun:sqlite";
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/shared/keeper-update";
import {
  coordinatorDatabaseSnapshotPath,
  writeCoordinatorDeployJournal,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV2,
} from "../src/coordinator-deploy-journal.ts";
import type { CoordinatorDeployRecoveryOptions } from "../src/coordinator-deploy-recovery.ts";
import { createCoordinatorRollbackSnapshot } from "../src/coordinator-deploy-snapshot.ts";
import type { StatusReport, WorkerStatus } from "../src/status.ts";

export const PRIOR_SHA = "1".repeat(40);
export const TARGET_SHA = "2".repeat(40);
export const WORKER_FP = "a".repeat(64);
const ROLLOUT_ID = "12345678-1234-4123-8123-123456789abc";
const RELEASE_ID = "87654321-4321-4321-8321-cba987654321";
const KEEPER_DIGEST = "c".repeat(64);

export interface CoordinatorFixture {
  root: string;
  journalPath: string;
  context: CoordinatorDeployJournalContext;
  journal: CoordinatorDeployJournalV2;
  priorDefinition: string;
  targetDefinition: string;
}

export async function coordinatorFixture(
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
    schemaVersion: 3,
    phase,
    admissionRecordedAtMs: 1,
    rolloutId: ROLLOUT_ID,
    targetWorkerFingerprints,
    workerKeeperPlans: targetWorkerFingerprints.map(workerKeeperPlan),
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

export function workerStatus(
  fingerprint: string = WORKER_FP,
  gitSha: string | null = TARGET_SHA,
  stale = false,
): WorkerStatus {
  return {
    fingerprint,
    label: "worker",
    os: "linux",
    reachableAddr: "worker.example.test",
    gitSha,
    keeperRuntime: {
      schema_version: 1,
      running_contract: keeperContract(PRIOR_SHA),
      keeper_pid: 41,
      keeper_epoch: "00000000-0000-4000-8000-000000000002",
      channel_count: 0,
      binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
      reconciled_at_ms: 1,
    },
    coordinatorOpenSessionIds: [],
    lastSeenMs: 10,
    ageMs: 0,
    stale,
  };
}

export function statusReport(sha: string | null, workers: WorkerStatus[] = []): StatusReport {
  return {
    tailscale: { required: false, state: "disabled", fqdn: null, running: false },
    coordAgentLoaded: true,
    workerAgentLoaded: true,
    coord: { reachable: sha !== null, gitSha: sha },
    workers,
    tlsMode: "direct",
    url: "https://coordinator.example.test:4102",
    handoff: null,
    publicOrigin: { state: "unconfigured" },
  };
}

export function successfulRuntime(
  sha: string,
  events: string[] = [],
): CoordinatorDeployRecoveryOptions {
  return {
    readStatus: async () => statusReport(sha, [workerStatus()]),
    readRoutableWorkers: async () => new Set([workerStatus().fingerprint]),
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

export function databaseValue(path: string): string {
  const sqlite = new Database(path, { readonly: true });
  try {
    return sqlite.query<{ value: string }, []>("SELECT value FROM rollout_state").get()!.value;
  } finally {
    sqlite.close(true);
  }
}

function keeperContract(buildSha: string) {
  return {
    protocol_version: 2,
    supported_features: ["keeper-contract-v1"],
    required_features: ["keeper-contract-v1"],
    implementation_digest: KEEPER_DIGEST,
    bun_abi: "1.2.3",
    platform: "linux" as const,
    arch: "x64",
    build_sha: buildSha,
  };
}

function workerKeeperPlan(fingerprint: string) {
  return {
    fingerprint,
    keeperUpdate: {
      admission: {
        classification: "worker-only-safe" as const,
        source_contract_digest: KEEPER_DIGEST,
        target_contract_digest: KEEPER_DIGEST,
        expected_keeper_pid: 41,
        expected_keeper_epoch: "00000000-0000-4000-8000-000000000002",
        expected_binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
        required_action: "preserve" as const,
      },
      source_contract: keeperContract(PRIOR_SHA),
      target_contract: keeperContract(TARGET_SHA),
    },
  };
}

function serviceDefinition(repo: string, sha: string, databasePath: string): string {
  return [
    "[Service]",
    `WorkingDirectory=${repo}`,
    `Environment="ROOST_GIT_SHA=${sha}"`,
    `Environment="ROOST_COORDINATOR_DB=${databasePath}"`,
  ].join("\n");
}
