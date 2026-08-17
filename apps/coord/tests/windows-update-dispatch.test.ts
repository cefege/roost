import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  __clearDeployJobsForTest,
  handleWorkerUpdateProgress,
  resumeWindowsUpdateDeploysForWorker,
  startWindowsUpdateDeploy,
} from "../src/deploy-jobs.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { rejectPendingRpcsForWorker } from "../src/router/pending-rpcs.ts";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";

const workerFp = "a".repeat(64);
const previousCoordDataDir = process.env.ROOST_COORD_DATA_DIR;
let dataDir = "";

beforeEach(() => {
  dataDir = mkdtempSync(join(tmpdir(), "roost-windows-dispatch-"));
  process.env.ROOST_COORD_DATA_DIR = dataDir;
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

describe("Windows update dispatch recovery", () => {
  test("retries START when transport admission is not followed by a worker acknowledgement", async () => {
    const actions: string[] = [];
    __setConnectWorkerForTest(workerFp, {
      workerFp,
      send(frame: CoordWorkerDown) {
        if (frame.frame.case === "updateBroker") actions.push(frame.frame.value.action);
        return 1;
      },
    });

    const started = await startWindowsUpdateDeploy("build-pc", {
      workerFp,
      manifestUrl: "https://releases.example/roost-windows-x64.manifest.json",
      signatureUrl: "https://releases.example/roost-windows-x64.manifest.json.p7s",
      manifestSha256: "b".repeat(64),
      publisherSha256: "c".repeat(64),
    });
    expect(started.ok).toBe(true);
    expect(started.jobId).toBeString();
    expect(actions).toEqual(["START"]);

    expect(rejectPendingRpcsForWorker(workerFp, "connection closed before acknowledgement")).toBe(1);
    await Promise.resolve();
    resumeWindowsUpdateDeploysForWorker(workerFp);
    expect(actions).toEqual(["START", "START"]);

    await handleWorkerUpdateProgress(workerFp, {
      request_id: "terminal",
      job_id: started.jobId!,
      sequence: 1,
      phase: "complete",
      message: "updated",
      terminal: true,
      success: true,
    });
  });
});
