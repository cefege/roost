// Pinned deploy provenance tests bind the internal publication-check bypass to
// one private coordinator job record. Caller-invented IDs and tuple drift fail
// before any target probe or mutation.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { validatePinnedDeployJobOrDie } from "../src/deploy-job-provenance.ts";

const WORKER_FP = "a".repeat(64);
const JOB_ID = "11111111-1111-4111-8111-111111111111";
const TARGET_SHA = "b".repeat(40);
const HOST = "worker.example.test";
let directory: string;
let sourceRoot: string;
let priorDataDir: string | undefined;

beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), "roost-job-provenance-"));
  sourceRoot = realpathSync(directory);
  priorDataDir = process.env.ROOST_COORD_DATA_DIR;
  process.env.ROOST_COORD_DATA_DIR = join(directory, "coord-data");
  const recordDirectory = join(
    process.env.ROOST_COORD_DATA_DIR,
    "deploy-jobs",
    WORKER_FP,
  );
  mkdirSync(recordDirectory, { recursive: true, mode: 0o700 });
  const recordPath = join(recordDirectory, `${JOB_ID}.json`);
  writeFileSync(recordPath, JSON.stringify({
    schemaVersion: 1,
    operation: {
      jobId: JOB_ID,
      workerFp: WORKER_FP,
      host: HOST,
      revision: 1,
      targetGitSha: TARGET_SHA,
      source: "push",
      status: "queued",
      phase: "preflight",
      reasonCode: null,
      message: null,
      createdAtMs: 1,
      updatedAtMs: 1,
      startedAtMs: null,
      completedAtMs: null,
      nextAttemptAtMs: null,
      exitCode: null,
    },
    sourceRoot,
    sourceMode: "coordinator-pinned",
  }));
  chmodSync(recordPath, 0o600);
});

afterEach(() => {
  if (priorDataDir === undefined) delete process.env.ROOST_COORD_DATA_DIR;
  else process.env.ROOST_COORD_DATA_DIR = priorDataDir;
  rmSync(directory, { recursive: true, force: true });
});

test("accepts only the exact durable worker, host, source, target, and job", () => {
  expect(() => validatePinnedDeployJobOrDie({
    jobId: JOB_ID,
    workerFp: WORKER_FP,
    host: HOST,
    sourceRoot,
    targetSha: TARGET_SHA,
  })).not.toThrow();
  expect(() => validatePinnedDeployJobOrDie({
    jobId: JOB_ID,
    workerFp: WORKER_FP,
    host: "other.example.test",
    sourceRoot,
    targetSha: TARGET_SHA,
  })).toThrow("does not match");
});
