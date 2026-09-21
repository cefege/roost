// Catch-up scheduling uses the process-owned durable update owner. These tests
// pin admission, periodic re-evaluation, and exact-target requests without
// filesystem timers or a deployment subprocess.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  CATCH_UP_COOLDOWN_MS,
  _catchUpDeployDecision,
  createWorkerCatchUpScheduler,
  type CatchUpDeployDecisionInputs,
  type CatchUpWorkerRow,
} from "../src/worker-catchup-deploy.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import type { WorkerUpdateOperation } from "@roost/shared/worker-update-operation";

const FLEET_SHA = "b".repeat(40);
const BEHIND_SHA = "a".repeat(40);
const WORKER_FP = "f".repeat(64);
const HOST = "m1-us.tailnet.ts.net";
const BEHIND_WORKER: CatchUpWorkerRow = {
  fp: WORKER_FP,
  os: "linux",
  label: "m1-us",
  reachableAddr: HOST,
  gitSha: BEHIND_SHA,
  keeperRuntimeJson: null,
};

function decisionInputs(
  overrides: Partial<CatchUpDeployDecisionInputs> = {},
): CatchUpDeployDecisionInputs {
  return {
    worker: BEHIND_WORKER,
    coordGitSha: FLEET_SHA,
    routable: true,
    updateInFlight: false,
    retryAfterMs: null,
    nowMs: 1_000,
    ...overrides,
  };
}

describe("worker catch-up admission", () => {
  test("admits one routable POSIX worker behind the coordinator", () => {
    expect(_catchUpDeployDecision(decisionInputs())).toEqual({ start: true, host: HOST });
  });

  test("separates offline, in-flight, cooldown, current, and unsupported outcomes", () => {
    expect(_catchUpDeployDecision(decisionInputs({ routable: false }))).toEqual({
      start: false, reason: "offline",
    });
    expect(_catchUpDeployDecision(decisionInputs({ updateInFlight: true }))).toEqual({
      start: false, reason: "deploy_in_flight",
    });
    expect(_catchUpDeployDecision(decisionInputs({
      retryAfterMs: CATCH_UP_COOLDOWN_MS + 1,
    }))).toEqual({ start: false, reason: "failure_cooldown" });
    expect(_catchUpDeployDecision(decisionInputs({
      worker: { ...BEHIND_WORKER, gitSha: FLEET_SHA },
    }))).toEqual({ start: false, reason: "up_to_date" });
    expect(_catchUpDeployDecision(decisionInputs({
      worker: { ...BEHIND_WORKER, os: "win32" },
    }))).toEqual({ start: false, reason: "unsupported_platform" });
  });
});

describe("worker catch-up scheduler", () => {
  let opened: DbHandle;
  let directory: string;

  beforeEach(async () => {
    directory = mkdtempSync(join(tmpdir(), "roost-catchup-scheduler-"));
    opened = openDb(join(directory, "coord.db"));
    await runMigrations(opened.sqlite);
    const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
    await opened.db.insertInto("workers").values({
      fp: WORKER_FP,
      dashboard_id: tenant.dashboardId,
      label: "m1-us",
      os: "linux",
      git_sha: BEHIND_SHA,
      reachable_addr: HOST,
      registered_at_ms: 1,
      last_seen_ms: 1,
    }).execute();
    __setConnectWorkerForTest(WORKER_FP, { workerFp: WORKER_FP, send: () => 1 });
  });

  afterEach(async () => {
    __setConnectWorkerForTest(WORKER_FP, null);
    await opened.close();
    rmSync(directory, { recursive: true, force: true });
  });

  test("sweep requests one exact catch-up target through the shared owner", async () => {
    const starts: unknown[] = [];
    const owner = {
      readSummary: (_workerFp: string): WorkerUpdateOperation | null => null,
      startDeploy: async (request: unknown) => {
        starts.push(request);
        return { ok: true, jobId: "11111111-1111-4111-8111-111111111111" };
      },
      sweep: async () => {},
    };
    const scheduler = createWorkerCatchUpScheduler({
      db: opened.db,
      updateOwner: owner,
      sourceRoot: "/srv/roost-release",
      coordGitSha: FLEET_SHA,
      startTimer: false,
    });
    await scheduler.sweep();
    expect(starts).toEqual([{
      workerFp: WORKER_FP,
      host: HOST,
      expectedGitSha: FLEET_SHA,
      source: "catchup",
      sourceRoot: "/srv/roost-release",
      sourceMode: "coordinator-pinned",
    }]);
    scheduler.dispose();
  });
});
