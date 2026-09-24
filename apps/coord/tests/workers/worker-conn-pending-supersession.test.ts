// Verifies a new authenticated worker generation rejects work sent to its predecessor.
// The test drives only hello admission; database effects remain outside this boundary.
// Pending RPC identity is owned by router/pending-rpcs and worker-conn owns supersession.

import { afterEach, expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { CoordWorkerUpSchema, WHelloSchema } from "@roost/protocol/proto/worker_transport_pb";
import { TERMINAL_METADATA_CAPABILITY } from "@roost/protocol/terminal-metadata";
import { makeWorkerConn, type WorkerServiceDeps } from "../../src/workers/worker-conn.ts";
import {
  __setConnectWorkerForTest,
  connectWorkers,
} from "../../src/workers/worker-registry.ts";
import { createPendingRpc, rejectPendingRpcsForWorker } from "../../src/router/pending-rpcs.ts";

const WORKER_FP = "a".repeat(64);

function helloFrame(processEpoch = "", capabilities: string[] = []) {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, {
        workerFp: WORKER_FP,
        version: "test",
        processEpoch,
        capabilities,
      }),
    },
  });
}

afterEach(() => {
  rejectPendingRpcsForWorker(WORKER_FP, "test cleanup");
  connectWorkers.delete(WORKER_FP);
});

test("a replacement worker generation rejects predecessor RPCs", async () => {
  const deps = {} as WorkerServiceDeps;
  const oldConnection = makeWorkerConn(deps, { fingerprint: WORKER_FP }, () => 1, () => undefined);
  const replacement = makeWorkerConn(deps, { fingerprint: WORKER_FP }, () => 1, () => undefined);
  try {
    await oldConnection.handleUpstream(helloFrame());
    const pending = createPendingRpc(1_000, WORKER_FP);

    await replacement.handleUpstream(helloFrame());

    await expect(pending.promise).rejects.toMatchObject({ code: Code.Unavailable });
  } finally {
    oldConnection.close();
    replacement.close();
  }
});

test("tracks worker boot and socket generations without retaining superseded capabilities", async () => {
  const deps = {} as WorkerServiceDeps;
  const oldConnection = makeWorkerConn(deps, { fingerprint: WORKER_FP }, () => 1, () => undefined);
  const replacement = makeWorkerConn(deps, { fingerprint: WORKER_FP }, () => 1, () => undefined);
  try {
    await oldConnection.handleUpstream(helloFrame("worker-boot-1", [
      TERMINAL_METADATA_CAPABILITY,
      "unacknowledged-capability",
    ]));
    const oldHandle = connectWorkers.get(WORKER_FP);
    expect(oldHandle?.processEpoch).toBe("worker-boot-1");
    expect([...oldHandle?.capabilities ?? []]).toEqual([TERMINAL_METADATA_CAPABILITY]);

    await replacement.handleUpstream(helloFrame());
    const currentHandle = connectWorkers.get(WORKER_FP);
    expect(currentHandle?.processEpoch).toBeNull();
    expect([...currentHandle?.capabilities ?? []]).toEqual([]);
    expect(currentHandle?.connectionGeneration).not.toBe(oldHandle?.connectionGeneration);
  } finally {
    oldConnection.close();
    replacement.close();
  }
});

test("test worker handles default to legacy-safe connection fields", () => {
  __setConnectWorkerForTest(WORKER_FP, {
    workerFp: WORKER_FP,
    send: () => 1,
  });

  const handle = connectWorkers.get(WORKER_FP);
  expect(handle?.processEpoch).toBeNull();
  expect(handle?.capabilities.size).toBe(0);
  expect(handle?.connectionGeneration).toEqual(expect.any(String));
});
