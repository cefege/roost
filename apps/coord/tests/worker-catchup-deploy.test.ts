import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BoundedBus } from "../src/buses.ts";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  __clearDeployJobsForTest,
  _deployJobs,
  type DeployJob,
  type DeployStreamMsg,
} from "../src/deploy-jobs.ts";
import {
  __clearCatchUpDeployStateForTest,
  _catchUpDeployDecision,
  _startCatchUpDeployForWorker,
  _watchCatchUpDeployOutcome,
  CATCH_UP_COOLDOWN_MS,
  type CatchUpDeployDecisionInputs,
  type CatchUpWorkerRow,
  startCatchUpDeployOnAttach,
} from "../src/worker-catchup-deploy.ts";

const FLEET_SHA = "b".repeat(40);
const BEHIND_SHA = "a".repeat(40);
const HOST = "m1-us.tailnet.ts.net";

const BEHIND_WORKER: CatchUpWorkerRow = {
  fp: "f".repeat(64),
  os: "linux",
  label: "m1-us",
  reachableAddr: HOST,
  gitSha: BEHIND_SHA,
};

function decisionInputs(
  overrides: Partial<CatchUpDeployDecisionInputs> = {},
): CatchUpDeployDecisionInputs {
  return {
    worker: BEHIND_WORKER,
    coordGitSha: FLEET_SHA,
    hostsWithDeployInFlight: new Set<string>(),
    cooldownUntilMsByHost: new Map<string, number>(),
    operatorRolloutActive: false,
    nowMs: 1_000,
    ...overrides,
  };
}

function runningDeployJob(host: string): DeployJob {
  return {
    jobId: crypto.randomUUID(),
    host,
    startedAt: 0,
    lines: [],
    status: "running",
    bus: new BoundedBus<DeployStreamMsg>(8),
    gcTimer: null,
  };
}

function failedDeployJob(host: string, error: string): DeployJob {
  return { ...runningDeployJob(host), status: "done", exitCode: 1, error };
}

describe("worker catch-up deploy admission", () => {
  test("admits a POSIX worker behind the coordinator's own SHA", () => {
    expect(_catchUpDeployDecision(decisionInputs())).toEqual({
      start: true,
      host: HOST,
    });
  });

  test("deploys to the registered label when no reachable address is known", () => {
    expect(_catchUpDeployDecision(decisionInputs({
      worker: { ...BEHIND_WORKER, reachableAddr: null },
    }))).toEqual({ start: true, host: "m1-us" });
  });

  test("refuses a Windows worker: the signed broker owns that path", () => {
    expect(_catchUpDeployDecision(decisionInputs({
      worker: { ...BEHIND_WORKER, os: "win32" },
    }))).toEqual({ start: false, reason: "windows_broker_owned" });
  });

  test("refuses a worker already on the fleet SHA", () => {
    expect(_catchUpDeployDecision(decisionInputs({
      worker: { ...BEHIND_WORKER, gitSha: FLEET_SHA },
    }))).toEqual({ start: false, reason: "up_to_date" });
  });

  test("refuses when the coordinator's own SHA is not a release identity", () => {
    for (const coordGitSha of [null, "dev", "b1c2d3e"]) {
      expect(_catchUpDeployDecision(decisionInputs({ coordGitSha }))).toEqual({
        start: false,
        reason: "coordinator_sha_unknown",
      });
    }
  });

  test("refuses when the worker reports no SHA", () => {
    for (const gitSha of [null, "", "dev"]) {
      expect(_catchUpDeployDecision(decisionInputs({
        worker: { ...BEHIND_WORKER, gitSha },
      }))).toEqual({ start: false, reason: "worker_sha_unknown" });
    }
  });

  test("refuses while a deploy for that host is already in flight", () => {
    expect(_catchUpDeployDecision(decisionInputs({
      hostsWithDeployInFlight: new Set([HOST]),
    }))).toEqual({ start: false, reason: "deploy_in_flight" });
  });

  test("refuses inside the failure cooldown and admits once it lapses", () => {
    const failedAtMs = 5_000;
    const cooldownUntilMsByHost = new Map([
      [HOST, failedAtMs + CATCH_UP_COOLDOWN_MS],
    ]);
    expect(_catchUpDeployDecision(decisionInputs({
      cooldownUntilMsByHost,
      nowMs: failedAtMs + CATCH_UP_COOLDOWN_MS - 1,
    }))).toEqual({ start: false, reason: "failure_cooldown" });
    expect(_catchUpDeployDecision(decisionInputs({
      cooldownUntilMsByHost,
      nowMs: failedAtMs + CATCH_UP_COOLDOWN_MS,
    }))).toEqual({ start: true, host: HOST });
  });

  test("refuses while an operator fleet rollout owns the fleet", () => {
    expect(_catchUpDeployDecision(decisionInputs({
      operatorRolloutActive: true,
    }))).toEqual({ start: false, reason: "operator_rollout_in_progress" });
  });

  test("refuses when nothing addressable resolves for the worker", () => {
    for (const label of ["", "   ", "old laptop"]) {
      expect(_catchUpDeployDecision(decisionInputs({
        worker: { ...BEHIND_WORKER, label, reachableAddr: null },
      }))).toEqual({ start: false, reason: "no_reachable_host" });
    }
  });
});

describe("worker catch-up deploy wiring", () => {
  let serviceDir: string;
  let priorServiceDir: string | undefined;
  let started: string[];

  beforeEach(() => {
    priorServiceDir = process.env.ROOST_SERVICE_DIR;
    serviceDir = mkdtempSync(join(tmpdir(), "roost-catchup-"));
    process.env.ROOST_SERVICE_DIR = serviceDir;
    started = [];
    __clearCatchUpDeployStateForTest();
    __clearDeployJobsForTest();
  });

  afterEach(() => {
    if (priorServiceDir === undefined) delete process.env.ROOST_SERVICE_DIR;
    else process.env.ROOST_SERVICE_DIR = priorServiceDir;
    rmSync(serviceDir, { recursive: true, force: true });
    __clearCatchUpDeployStateForTest();
    __clearDeployJobsForTest();
  });

  function recordingStarter(jobId?: string) {
    return (host: string) => {
      started.push(host);
      return jobId ? { ok: true, jobId } : { ok: false, error: "no coordinator URL" };
    };
  }

  test("starts one catch-up per host, then holds off until the cooldown lapses", async () => {
    const job = failedDeployJob(HOST, "deploy exit 1");
    _deployJobs.set(job.jobId, job);
    const deployStarter = (host: string) => {
      started.push(host);
      return { ok: true, jobId: job.jobId };
    };

    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter,
    })).toEqual({ start: true, host: HOST });
    expect(started).toEqual([HOST]);

    // Synchronous, so no outcome can have been observed yet: this module's own
    // in-flight entry is what has to stop a second attach from doubling up.
    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter,
    })).toEqual({ start: false, reason: "deploy_in_flight" });
    expect(started).toEqual([HOST]);

    await _watchCatchUpDeployOutcome(HOST, job.jobId);

    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter,
    })).toEqual({ start: false, reason: "failure_cooldown" });
    expect(started).toEqual([HOST]);
  });

  test("starts nothing for a Windows worker", () => {
    expect(_startCatchUpDeployForWorker({ ...BEHIND_WORKER, os: "win32" }, {
      coordGitSha: FLEET_SHA,
      deployStarter: recordingStarter(crypto.randomUUID()),
    })).toEqual({ start: false, reason: "windows_broker_owned" });
    expect(started).toEqual([]);
  });

  test("starts nothing while the coordinator deploy journal is on disk", () => {
    mkdirSync(join(serviceDir, "transactions"), { recursive: true });
    writeFileSync(
      join(serviceDir, "transactions", "coordinator-deploy.json"),
      "{}",
    );

    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter: recordingStarter(crypto.randomUUID()),
    })).toEqual({ start: false, reason: "operator_rollout_in_progress" });
    expect(started).toEqual([]);
  });

  test("starts nothing while an operator-started deploy job holds the host", () => {
    const job = runningDeployJob(HOST);
    _deployJobs.set(job.jobId, job);

    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter: recordingStarter(crypto.randomUUID()),
    })).toEqual({ start: false, reason: "deploy_in_flight" });
    expect(started).toEqual([]);
  });

  test("arms the cooldown when the deploy job itself cannot be spawned", () => {
    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter: recordingStarter(),
    })).toEqual({ start: false, reason: "start_failed" });
    expect(started).toEqual([HOST]);

    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: FLEET_SHA,
      deployStarter: recordingStarter(),
    })).toEqual({ start: false, reason: "failure_cooldown" });
    expect(started).toEqual([HOST]);
  });

  test("the coordinator's own unknown SHA never starts a catch-up", () => {
    expect(_startCatchUpDeployForWorker(BEHIND_WORKER, {
      coordGitSha: "dev",
      deployStarter: recordingStarter(crypto.randomUUID()),
    })).toEqual({ start: false, reason: "coordinator_sha_unknown" });
    expect(started).toEqual([]);
  });
});

describe("worker catch-up deploy on attach", () => {
  const workerFp = "e".repeat(64);
  let workdir: string;
  let priorServiceDir: string | undefined;
  let opened: DbHandle;
  let started: string[];

  beforeEach(async () => {
    priorServiceDir = process.env.ROOST_SERVICE_DIR;
    workdir = mkdtempSync(join(tmpdir(), "roost-catchup-attach-"));
    process.env.ROOST_SERVICE_DIR = join(workdir, "service");
    opened = openDb(join(workdir, "coord.db"));
    await runMigrations(opened.sqlite);
    const tenant = ensureSelfHostedTenant(opened.sqlite, {
      backfillLegacyScopes: false,
    });
    await opened.db.insertInto("workers").values({
      fp: workerFp,
      dashboard_id: tenant.dashboardId,
      label: "m1-us",
      os: "linux",
      git_sha: BEHIND_SHA,
      reachable_addr: HOST,
      registered_at_ms: 1,
      last_seen_ms: 1,
    }).execute();
    started = [];
    __clearCatchUpDeployStateForTest();
    __clearDeployJobsForTest();
  });

  afterEach(async () => {
    if (priorServiceDir === undefined) delete process.env.ROOST_SERVICE_DIR;
    else process.env.ROOST_SERVICE_DIR = priorServiceDir;
    // One case closes the handle itself to break the query deliberately.
    await opened.close().catch(() => {});
    rmSync(workdir, { recursive: true, force: true });
    __clearCatchUpDeployStateForTest();
    __clearDeployJobsForTest();
  });

  function attachOptions() {
    return {
      coordGitSha: FLEET_SHA,
      deployStarter: (host: string) => {
        started.push(host);
        return { ok: false, error: "no coordinator URL" };
      },
    };
  }

  test("deploys to the registered worker's reachable address", async () => {
    await startCatchUpDeployOnAttach(opened.db, workerFp, attachOptions());
    expect(started).toEqual([HOST]);
  });

  test("starts nothing for a soft-deleted worker row", async () => {
    await opened.db.updateTable("workers")
      .set({ deleted_at_ms: 2 })
      .where("fp", "=", workerFp)
      .execute();

    await startCatchUpDeployOnAttach(opened.db, workerFp, attachOptions());
    expect(started).toEqual([]);
  });

  test("starts nothing for a fingerprint that is not registered", async () => {
    await startCatchUpDeployOnAttach(opened.db, "d".repeat(64), attachOptions());
    expect(started).toEqual([]);
  });

  test("an unusable database degrades to a skip instead of failing the attach", async () => {
    await opened.close();
    await expect(startCatchUpDeployOnAttach(opened.db, workerFp, attachOptions()))
      .resolves.toBeUndefined();
    expect(started).toEqual([]);
  });
});
