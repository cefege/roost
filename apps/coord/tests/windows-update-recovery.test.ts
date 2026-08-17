import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  __clearDeployJobsForTest,
  deployOutput,
  handleWorkerUpdateProgress,
  resumePersistedWindowsUpdateDeploysForWorker,
  startWindowsUpdateDeploy,
  windowsUpdateDeployRecordPath,
} from "../src/deploy-jobs.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { rejectPendingRpcsForWorker } from "../src/router/pending-rpcs.ts";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";

const workerFp = "a".repeat(64);
const previousCoordDataDir = process.env.ROOST_COORD_DATA_DIR;
let dataDir = "";
let actions: string[] = [];

async function collect(jobId: string) {
  const messages = [];
  for await (const message of deployOutput(jobId)) messages.push(message);
  return messages;
}

const updateOptions = {
  workerFp,
  manifestUrl: "https://releases.example/roost-windows-x64.manifest.json",
  signatureUrl: "https://releases.example/roost-windows-x64.manifest.json.p7s",
  manifestSha256: "b".repeat(64),
  publisherSha256: "c".repeat(64),
};

beforeEach(() => {
  dataDir = mkdtempSync(join(tmpdir(), "roost-windows-recovery-"));
  process.env.ROOST_COORD_DATA_DIR = dataDir;
  actions = [];
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    send(frame: CoordWorkerDown) {
      if (frame.frame.case === "updateBroker") actions.push(frame.frame.value.action);
      return 1;
    },
  });
});

afterEach(async () => {
  __setConnectWorkerForTest(workerFp, null);
  rejectPendingRpcsForWorker(workerFp, "test cleanup");
  await Promise.resolve();
  __clearDeployJobsForTest();
  if (previousCoordDataDir === undefined) delete process.env.ROOST_COORD_DATA_DIR;
  else process.env.ROOST_COORD_DATA_DIR = previousCoordDataDir;
  rmSync(dataDir, { recursive: true, force: true });
});

describe("Windows update coordinator durable recovery", () => {
  test("deduplicates repeated START admission before and after coordinator restart", async () => {
    const first = await startWindowsUpdateDeploy("build-pc", updateOptions);
    const repeated = await startWindowsUpdateDeploy("build-pc", updateOptions);
    expect(first).toMatchObject({ ok: true });
    expect(repeated).toEqual(first);
    expect(actions).toEqual(["START"]);

    expect(rejectPendingRpcsForWorker(workerFp, "coordinator restart before reply")).toBe(1);
    await Promise.resolve();
    __clearDeployJobsForTest();
    actions = [];

    const recovered = await startWindowsUpdateDeploy("build-pc", updateOptions);
    expect(recovered).toEqual(first);
    expect(actions).toEqual(["START"]);
    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: first.jobId!,
      sequence: 1,
      phase: "committed",
      message: "Windows update committed",
      terminal: true,
      success: true,
    });
  });

  test("rejects a conflicting release while a worker update is active", async () => {
    const first = await startWindowsUpdateDeploy("build-pc", updateOptions);
    const conflicting = await startWindowsUpdateDeploy("build-pc", {
      ...updateOptions,
      manifestSha256: "d".repeat(64),
    });
    expect(conflicting).toEqual({
      ok: false,
      error: `Windows worker already has active update job ${first.jobId}`,
    });
    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: first.jobId!,
      sequence: 1,
      phase: "committed",
      message: "Windows update committed",
      terminal: true,
      success: true,
    });
  });

  test("reloads a running job after restart and repeats unacknowledged START", async () => {
    const started = await startWindowsUpdateDeploy("build-pc", updateOptions);
    expect(started).toMatchObject({ ok: true });
    expect(actions).toEqual(["START"]);
    expect(rejectPendingRpcsForWorker(workerFp, "connection lost before ACK")).toBe(1);
    await Promise.resolve();
    __clearDeployJobsForTest();

    const output = deployOutput(started.jobId!);
    expect((await output.next()).value).toEqual({
      kind: "line",
      text: "starting signed Windows update on build-pc",
    });
    expect(actions).toEqual(["START", "START"]);
    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: started.jobId!,
      sequence: 1,
      phase: "committed",
      message: "Windows update committed",
      terminal: true,
      success: true,
    });
    await output.return(undefined);
  });

  test("worker reconnect resumes running jobs without a surviving output client", async () => {
    const started = await startWindowsUpdateDeploy("build-pc", updateOptions);
    expect(actions).toEqual(["START"]);
    rejectPendingRpcsForWorker(workerFp, "coordinator restart");
    await Promise.resolve();
    __clearDeployJobsForTest();
    actions = [];

    await resumePersistedWindowsUpdateDeploysForWorker(workerFp);
    await Promise.resolve();
    expect(actions).toEqual(["START"]);
    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: started.jobId!,
      sequence: 1,
      phase: "committed",
      message: "Windows update committed",
      terminal: true,
      success: true,
    });
  });

  test("replays a durably committed terminal result without redispatch", async () => {
    const started = await startWindowsUpdateDeploy("build-pc", updateOptions);
    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: started.jobId!,
      sequence: 3,
      phase: "rolled-back",
      message: "prior services restored",
      terminal: true,
      success: false,
      error: "health proof failed",
    });
    rejectPendingRpcsForWorker(workerFp, "terminal test cleanup");
    await Promise.resolve();
    __clearDeployJobsForTest();
    actions = [];

    expect(await collect(started.jobId!)).toEqual([
      { kind: "line", text: "starting signed Windows update on build-pc" },
      { kind: "line", text: "[rolled-back] prior services restored" },
      { kind: "done", exit: 1, error: "health proof failed" },
    ]);
    expect(actions).toEqual([]);
  });

  test("rejects path-like IDs and strictly invalid persisted records", async () => {
    expect(await collect("../../outside")).toEqual([
      { kind: "done", exit: null, error: "unknown jobId" },
    ]);

    const started = await startWindowsUpdateDeploy("build-pc", updateOptions);
    const path = windowsUpdateDeployRecordPath(started.jobId!);
    const valid = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
    rejectPendingRpcsForWorker(workerFp, "malformed record setup");
    await Promise.resolve();
    __clearDeployJobsForTest();
    actions = [];
    const malformed = [
      { ...valid, unexpected: true },
      { ...valid, workerFp: "../worker" },
      { ...valid, manifestUrl: "file:///tmp/manifest.json" },
      { ...valid, manifestSha256: "b".repeat(63) },
      { ...valid, publisherSha256: "C".repeat(64) },
      { ...valid, jobId: crypto.randomUUID() },
    ];
    for (const record of malformed) {
      writeFileSync(path, `${JSON.stringify(record)}\n`, { mode: 0o600 });
      expect(await collect(started.jobId!)).toEqual([
        { kind: "done", exit: null, error: "unknown jobId" },
      ]);
    }
    expect(actions).toEqual([]);
  });

  test("durably removes a terminal record after the reconnect TTL", async () => {
    const started = await startWindowsUpdateDeploy("build-pc", updateOptions);
    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: started.jobId!,
      sequence: 1,
      phase: "committed",
      message: "Windows update committed",
      terminal: true,
      success: true,
    });
    const path = windowsUpdateDeployRecordPath(started.jobId!);
    const record = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
    const completedAt = Date.now() - (20 * 60 * 1000) - 1;
    writeFileSync(path, `${JSON.stringify({
      ...record,
      startedAt: completedAt - 1_000,
      updatedAt: completedAt,
      completedAt,
    })}\n`, { mode: 0o600 });
    rejectPendingRpcsForWorker(workerFp, "expired record setup");
    await Promise.resolve();
    __clearDeployJobsForTest();

    expect(await collect(started.jobId!)).toEqual([
      { kind: "done", exit: null, error: "unknown jobId" },
    ]);
    expect(existsSync(path)).toBe(false);
  });
});
