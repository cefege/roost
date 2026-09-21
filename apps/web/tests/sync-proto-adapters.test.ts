// Sync proto adapters must contain malformed persisted worker capacity data.
// A rejected capacity report is a dropped field, not a Sync transport failure.
// This protects live terminal continuity from one bad coordinator row.

import { expect, mock, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { TerminalCoreCapacityReportSchema } from "@roost/shared/proto/wire_pb";
import type { WorkerUpdateOperation } from "@roost/shared/worker-update-operation";
import { workerUpdateOperationToProto } from "@roost/shared/worker-update-operation-proto";

const signal = mock((_kind: string, _facts: Record<string, unknown>) => {});

mock.module("@roost/shared/diag", () => ({ signal }));

// Install the diagnostic mock before the adapter evaluates its static import.
const {
  mergeWorkerUpdateOperation,
  terminalCoreCapacityProtoToWire,
  workerUpdateOperationProtoToWire,
} = await import("../src/store/sync-proto-adapters.ts");

test("capacity projection drops an unsafe protobuf counter without throwing", () => {
  const unsafe = create(TerminalCoreCapacityReportSchema, {
    used: 1,
    pending: 0,
    capacity: 1,
    estimatedReservedBytes: 40n * 1024n * 1024n,
    effectiveMemoryCeilingBytes: 2n * 1024n * 1024n * 1024n,
    bootRssBytes: 256n * 1024n * 1024n,
    overcommitCount: 0,
    refusalCount: BigInt(Number.MAX_SAFE_INTEGER) + 1n,
  });

  expect(terminalCoreCapacityProtoToWire(unsafe, "workers_list_hydration"))
    .toBeNull();
  expect(signal).toHaveBeenCalledWith("diag.corruption_signal", {
    kind: "terminal_core_capacity_invalid",
    frame: "workers_list_hydration",
    msg: expect.stringContaining("refusal_count exceeds a safe integer"),
    cooldownKey: "sync",
  });
});

function makeOperation(revision: number): WorkerUpdateOperation {
  return {
    jobId: "00000000-0000-4000-8000-000000000001",
    workerFp: "a".repeat(64) as WorkerUpdateOperation["workerFp"],
    host: "worker.example",
    revision,
    targetGitSha: "b".repeat(40),
    source: "manual",
    status: "running",
    phase: "activation",
    reasonCode: null,
    message: null,
    createdAtMs: 1,
    updatedAtMs: revision,
    startedAtMs: 1,
    completedAtMs: null,
    nextAttemptAtMs: null,
    exitCode: null,
  };
}

test("worker update revisions reject stale snapshots and legacy nulls", () => {
  const stale = makeOperation(4);
  const current = makeOperation(5);
  const tied = { ...current, jobId: "00000000-0000-4000-8000-000000000002" };

  expect(workerUpdateOperationProtoToWire(
    workerUpdateOperationToProto(current),
    "workers_list_hydration",
  )).toEqual(current);
  expect(mergeWorkerUpdateOperation(current, stale)).toBe(current);
  expect(mergeWorkerUpdateOperation(current, null)).toBe(current);
  expect(mergeWorkerUpdateOperation(current, tied)).toBe(current);
  expect(mergeWorkerUpdateOperation(stale, current)).toBe(current);
});
