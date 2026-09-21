// Durable worker-update owner tests pin synchronous coalescing and the global
// two-child limit with injected subprocess handles. Real platform journal/lease
// behavior remains covered by the existing Linux/macOS recovery suites.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { WorkerUpdateOwner } from "../src/worker-update-owner.ts";
import {
  deployJobRecordPath,
  loadDeployJobRecord,
  persistDeployJobRecord,
} from "../src/deploy-job-record.ts";
import type {
  DeployJobRuntimeCallbacks,
  DeployJobRuntimeHandle,
  DeployJobRuntimeResult,
} from "../src/deploy-job-runtime.ts";
import {
  initialDeployJobRecord,
  initialWorkerUpdateOperation,
  nextDeployJobRecord,
} from "../src/worker-update-owner-types.ts";
import type { WorkerUpdateStartRequest } from "@roost/shared/worker-update-operation";

const TARGET_SHA = "c".repeat(40);
let directory: string;
let priorDataDir: string | undefined;

beforeEach(() => {
  directory = realpathSync(mkdtempSync(join(tmpdir(), "roost-deploy-owner-")));
  priorDataDir = process.env.ROOST_COORD_DATA_DIR;
  process.env.ROOST_COORD_DATA_DIR = join(directory, "coord-data");
});

afterEach(() => {
  if (priorDataDir === undefined) delete process.env.ROOST_COORD_DATA_DIR;
  else process.env.ROOST_COORD_DATA_DIR = priorDataDir;
  rmSync(directory, { recursive: true, force: true });
});

function request(index: number): WorkerUpdateStartRequest {
  return {
    workerFp: index.toString(16).padStart(64, "0"),
    host: `worker-${index}.example.test`,
    expectedGitSha: TARGET_SHA,
    source: "manual",
    sourceRoot: directory,
    sourceMode: "coordinator-pinned",
  };
}

function baseline() {
  return {
    heartbeatAtMs: 1,
    processEpoch: "epoch",
    gitSha: "a".repeat(40),
    keeperPid: null,
    keeperEpoch: null,
    bindingDigest: null,
    sessionIds: [],
  };
}

function failedRuntimeResult(error: string): DeployJobRuntimeResult {
  return {
    exitCode: 1,
    timedOut: false,
    settlementProven: false,
    error,
    failure: null,
  };
}

function waitingRecord(index: number, jobId: string) {
  const operation = initialWorkerUpdateOperation({
    request: request(index),
    jobId,
    revision: 1,
    atMs: 1,
  });
  return nextDeployJobRecord(initialDeployJobRecord({
    operation,
    sourceRoot: directory,
    baseline: baseline(),
    coordinatorOrigin: "https://coord.example.test",
  }), {
    status: "waiting",
    phase: "recovery",
    reasonCode: "coordinator_restarting",
    message: "Waiting for coordinator recovery",
    nextAttemptAtMs: 0,
  }, "Coordinator restarted; recovery is pending", 2);
}

describe("WorkerUpdateOwner", () => {
  test("two manual starts for one target coalesce to one durable job", async () => {
    const runtimeResult = Promise.withResolvers<DeployJobRuntimeResult>();
    const runtimeStarted = Promise.withResolvers<void>();
    let runtimeStarts = 0;
    const owner = new WorkerUpdateOwner({
      coordinatorOrigin: "https://coord.example.test",
      coordinatorDialUrl: "https://coord.example.test",
      readBaseline: async () => baseline(),
      readVerification: async () => null,
      publishOperation: () => {},
      workerExists: async () => true,
      workerRoutable: () => true,
      startRuntime: () => {
        runtimeStarts += 1;
        runtimeStarted.resolve();
        return { result: runtimeResult.promise, stop: () => {} };
      },
    });
    await owner.initialize();
    const first = await owner.startDeploy(request(1));
    const second = await owner.startDeploy(request(1));
    await runtimeStarted.promise;
    expect(first.ok).toBe(true);
    expect(second).toEqual(first);
    expect(runtimeStarts).toBe(1);
    runtimeResult.resolve(failedRuntimeResult("fixture stop"));
    await owner.dispose();
  });

  test("only two host mutations execute concurrently", async () => {
    const pending = new Map<
      string,
      ReturnType<typeof Promise.withResolvers<DeployJobRuntimeResult>>
    >();
    const started: string[] = [];
    const twoStarted = Promise.withResolvers<void>();
    const queuedStarted = Promise.withResolvers<void>();
    const owner = new WorkerUpdateOwner({
      coordinatorOrigin: "https://coord.example.test",
      coordinatorDialUrl: "https://coord.example.test",
      readBaseline: async () => baseline(),
      readVerification: async () => null,
      publishOperation: () => {},
      workerExists: async () => true,
      workerRoutable: () => true,
      startRuntime: (
        updateRequest: WorkerUpdateStartRequest,
        _jobId: string,
        _coordinatorUrl: string,
        _callbacks: DeployJobRuntimeCallbacks,
      ): DeployJobRuntimeHandle => {
        started.push(updateRequest.host);
        if (started.length === 2) twoStarted.resolve();
        if (started.length === 3) queuedStarted.resolve();
        const result = Promise.withResolvers<DeployJobRuntimeResult>();
        pending.set(updateRequest.host, result);
        return { result: result.promise, stop: () => {} };
      },
    });
    await owner.initialize();
    await Promise.all([
      owner.startDeploy(request(1)),
      owner.startDeploy(request(2)),
      owner.startDeploy(request(3)),
    ]);
    await twoStarted.promise;
    expect(started).toHaveLength(2);
    expect(new Set(started).size).toBe(2);
    const queuedHost = [
      "worker-1.example.test",
      "worker-2.example.test",
      "worker-3.example.test",
    ].find(host => !started.includes(host));
    if (!queuedHost) throw new Error("expected one queued worker");
    pending.get(started[0]!)?.resolve(failedRuntimeResult("unreachable"));
    await queuedStarted.promise;
    expect(started).toContain(queuedHost);
    for (const result of pending.values()) {
      result.resolve(failedRuntimeResult("fixture stop"));
    }
    await owner.dispose();
  });

  test("keeps every record for a corrupted worker out of recovery", async () => {
    const corruptJobId = "11111111-1111-4111-8111-111111111111";
    const recoverableJobId = "22222222-2222-4222-8222-222222222222";
    const corruptRecord = waitingRecord(1, corruptJobId);
    const recoverableRecord = waitingRecord(2, recoverableJobId);
    await persistDeployJobRecord(corruptRecord);
    await persistDeployJobRecord(recoverableRecord);
    const malformedLeaf = join(
      deployJobRecordPath(corruptRecord.operation.workerFp, corruptJobId),
      "..",
      "unexpected-leaf",
    );
    writeFileSync(malformedLeaf, "{}");

    const runtimeStarted = Promise.withResolvers<void>();
    const runtimeResult = Promise.withResolvers<DeployJobRuntimeResult>();
    const startedWorkerFps: string[] = [];
    const owner = new WorkerUpdateOwner({
      coordinatorOrigin: "https://coord.example.test",
      coordinatorDialUrl: "https://coord.example.test",
      readBaseline: async () => baseline(),
      readVerification: async () => null,
      publishOperation: () => {},
      workerExists: async () => true,
      workerRoutable: () => true,
      now: () => 10,
      startRuntime: updateRequest => {
        startedWorkerFps.push(updateRequest.workerFp);
        runtimeStarted.resolve();
        return {
          result: runtimeResult.promise,
          stop: () => runtimeResult.resolve(failedRuntimeResult("fixture stop")),
        };
      },
    });

    await owner.initialize();
    expect(owner.ownsJob(corruptJobId)).toBe(false);
    expect(owner.readSummary(corruptRecord.operation.workerFp)).toBeNull();
    expect(owner.ownsJob(recoverableJobId)).toBe(true);
    expect(await owner.startDeploy(request(1))).toEqual({
      ok: false,
      error: "worker update record is corrupt; retained for diagnosis",
    });

    await owner.sweep();
    await runtimeStarted.promise;
    expect(startedWorkerFps).toEqual([recoverableRecord.operation.workerFp]);
    runtimeResult.resolve(failedRuntimeResult("fixture stop"));
    await owner.dispose();
  });

  test("coordinator shutdown keeps the same admitted job recoverable", async () => {
    const runtimeStarted = Promise.withResolvers<void>();
    const runtimeResult = Promise.withResolvers<DeployJobRuntimeResult>();
    const owner = new WorkerUpdateOwner({
      coordinatorOrigin: "https://coord.example.test",
      coordinatorDialUrl: "https://coord.example.test",
      readBaseline: async () => baseline(),
      readVerification: async () => null,
      publishOperation: () => {},
      workerExists: async () => true,
      workerRoutable: () => true,
      startRuntime: () => {
        runtimeStarted.resolve();
        return {
          result: runtimeResult.promise,
          stop: () => {
            runtimeResult.resolve(failedRuntimeResult("coordinator shutdown"));
          },
        };
      },
    });
    await owner.initialize();
    const started = await owner.startDeploy(request(1));
    if (!started.jobId) throw new Error("update job did not start");
    await runtimeStarted.promise;
    await owner.dispose();
    const loaded = await loadDeployJobRecord(request(1).workerFp, started.jobId);
    expect(loaded.kind).toBe("record");
    if (loaded.kind === "record") {
      expect(loaded.record.operation.status).toBe("waiting");
      expect(loaded.record.operation.reasonCode).toBe("coordinator_restarting");
    }
  });
  test("shutdown keeps a confirmation-phase job recoverable", async () => {
    const sleepStarted = Promise.withResolvers<void>();
    const releaseSleep = Promise.withResolvers<void>();
    const owner = new WorkerUpdateOwner({
      coordinatorOrigin: "https://coord.example.test",
      coordinatorDialUrl: "https://coord.example.test",
      readBaseline: async () => baseline(),
      readVerification: async () => null,
      publishOperation: () => {},
      workerExists: async () => true,
      workerRoutable: () => true,
      sleep: async () => {
        sleepStarted.resolve();
        await releaseSleep.promise;
      },
      startRuntime: () => ({
        result: Promise.resolve({
          exitCode: 0,
          timedOut: false,
          settlementProven: true,
          error: null,
          failure: null,
        }),
        stop: () => {},
      }),
    });
    await owner.initialize();
    const started = await owner.startDeploy(request(1));
    if (!started.jobId) throw new Error("update job did not start");
    await sleepStarted.promise;
    const disposing = owner.dispose();
    releaseSleep.resolve();
    await disposing;
    const loaded = await loadDeployJobRecord(request(1).workerFp, started.jobId);
    expect(loaded.kind).toBe("record");
    if (loaded.kind === "record") {
      expect(loaded.record.operation.status).toBe("waiting");
      expect(loaded.record.operation.reasonCode).toBe("coordinator_restarting");
    }
  });
});
