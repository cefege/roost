// Durable POSIX update record tests use private temporary storage. They pin
// strict identity/path matching, monotonic latest selection, output bounds,
// and per-worker corrupt evidence without touching platform host journals.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  DEPLOY_JOB_MAX_LINE_LENGTH,
  appendDeployOutputLine,
  deployJobRecordPath,
  latestDeployOperations,
  loadAllDeployJobRecords,
  loadDeployJobRecord,
  normalizeDeployOutputLine,
  persistDeployJobRecord,
  type PersistedDeployJob,
} from "../src/deploy-job-record.ts";

const WORKER_FP = "a".repeat(64);
const OTHER_WORKER_FP = "b".repeat(64);
const TARGET_SHA = "c".repeat(40);
let directory: string;
let priorDataDir: string | undefined;

function record(workerFp: string, jobId: string, revision: number): PersistedDeployJob {
  const operation = {
    jobId,
    workerFp: workerFp as PersistedDeployJob["operation"]["workerFp"],
    host: "worker.example.test",
    revision,
    targetGitSha: TARGET_SHA,
    source: "manual" as const,
    status: "queued" as const,
    phase: "preflight" as const,
    reasonCode: null,
    message: "Update queued",
    createdAtMs: 1,
    updatedAtMs: revision,
    startedAtMs: null,
    completedAtMs: null,
    nextAttemptAtMs: null,
    exitCode: null,
  };
  return {
    schemaVersion: 1,
    operation,
    report: {
      schemaVersion: 1,
      operation,
      observedGitSha: null,
      coordinatorOrigin: "https://coord.example.test",
      failure: null,
      events: [{ atMs: 1, phase: "preflight", message: "Update queued" }],
    },
    sourceRoot: directory,
    sourceMode: "coordinator-pinned",
    baseline: {
      heartbeatAtMs: 1,
      processEpoch: null,
      gitSha: null,
      keeperPid: null,
      keeperEpoch: null,
      bindingDigest: null,
      sessionIds: [],
    },
    lines: [],
  };
}

beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), "roost-deploy-record-"));
  priorDataDir = process.env.ROOST_COORD_DATA_DIR;
  process.env.ROOST_COORD_DATA_DIR = join(directory, "coord-data");
});

afterEach(() => {
  if (priorDataDir === undefined) delete process.env.ROOST_COORD_DATA_DIR;
  else process.env.ROOST_COORD_DATA_DIR = priorDataDir;
  rmSync(directory, { recursive: true, force: true });
});

describe("durable POSIX deploy records", () => {
  test("round-trips a private identity-bound record", async () => {
    const jobId = "11111111-1111-4111-8111-111111111111";
    await persistDeployJobRecord(record(WORKER_FP, jobId, 1));
    const loaded = await loadDeployJobRecord(WORKER_FP, jobId);
    expect(loaded.kind).toBe("record");
    if (loaded.kind === "record") expect(loaded.record.operation.revision).toBe(1);
  });

  test("chooses the highest persisted revision across job IDs", () => {
    const older = record(WORKER_FP, "11111111-1111-4111-8111-111111111111", 4);
    const newer = record(WORKER_FP, "22222222-2222-4222-8222-222222222222", 5);
    expect(latestDeployOperations([newer, older]).get(WORKER_FP)?.jobId)
      .toBe(newer.operation.jobId);
  });

  test("retains corrupt evidence under only its canonical worker directory", async () => {
    const jobId = "33333333-3333-4333-8333-333333333333";
    const path = deployJobRecordPath(WORKER_FP, jobId);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, JSON.stringify(record(OTHER_WORKER_FP, jobId, 1)));
    const loaded = await loadAllDeployJobRecords();
    expect(loaded.records).toHaveLength(0);
    expect(loaded.corruptWorkerFingerprints.get(WORKER_FP)).toEqual([path]);
  });

  test("normalizes and bounds persisted human output independently", () => {
    const line = "x".repeat(DEPLOY_JOB_MAX_LINE_LENGTH + 10);
    const normalized = normalizeDeployOutputLine(line);
    if (normalized === null) throw new Error("expected a normalized line");
    expect(appendDeployOutputLine([], normalized)[0]).toHaveLength(
      DEPLOY_JOB_MAX_LINE_LENGTH,
    );
  });
});
