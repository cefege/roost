// Sync proto adapters must contain malformed persisted worker capacity data.
// A rejected capacity report is a dropped field, not a Sync transport failure.
// This protects live terminal continuity from one bad coordinator row.

import { expect, mock, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { TerminalCoreCapacityReportSchema } from "@roost/shared/proto/wire_pb";

const signal = mock((_kind: string, _facts: Record<string, unknown>) => {});

mock.module("@roost/shared/diag", () => ({ signal }));

// Install the diagnostic mock before the adapter evaluates its static import.
const { terminalCoreCapacityProtoToWire } = await import(
  "../src/store/sync-proto-adapters.ts"
);

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
