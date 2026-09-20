// Reconnect respawn must survive keeper-update exclusivity rather than becoming
// a one-shot suppression. This test holds the write gate, releases it, and
// proves the current worker generation receives its missing-session command.

import { afterEach, expect, test } from "bun:test";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { respawnMissingForWorker } from "../src/connect/worker-respawn.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import { databaseWithOpenSession } from "./worker-respawn-harness.ts";

const WORKER_FP = "a".repeat(64);

afterEach(() => {
  __setConnectWorkerForTest(WORKER_FP, null);
});

test("defers reconnect respawn until keeper-update exclusivity releases", async () => {
  const gate = new CoordinatorWriteGate();
  const exclusive = await gate.acquireExclusive("keeper-update:test");
  const sentFrames: Parameters<WorkerHandle["send"]>[0][] = [];
  const send: WorkerHandle["send"] = frame => {
    sentFrames.push(frame);
    return 1;
  };
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: null,
    connectionGeneration: "respawn-test-connection",
    capabilities: new Set(),
    revoked: false,
    ready: true,
    send,
  };
  __setConnectWorkerForTest(WORKER_FP, worker);

  const respawn = respawnMissingForWorker(
    databaseWithOpenSession() as never,
    WORKER_FP,
    worker,
    gate,
  );
  await Promise.resolve();
  expect(sentFrames).toHaveLength(0);

  exclusive.release();
  await respawn;
  expect(sentFrames).toHaveLength(1);
  expect(sentFrames[0]?.frame.case).toBe("browserCommand");
});
