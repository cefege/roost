// Verifies a new authenticated worker generation rejects work sent to its predecessor.
// The test drives only hello admission; database effects remain outside this boundary.
// Pending RPC identity is owned by router/pending-rpcs and worker-conn owns supersession.

import { afterEach, expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { CoordWorkerUpSchema, WHelloSchema } from "@roost/shared/proto/worker_transport_pb";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import { createPendingRpc, rejectPendingRpcsForWorker } from "../src/router/pending-rpcs.ts";

const WORKER_FP = "a".repeat(64);

function helloFrame() {
  return create(CoordWorkerUpSchema, {
    frame: { case: "hello", value: create(WHelloSchema, { workerFp: WORKER_FP, version: "test" }) },
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
