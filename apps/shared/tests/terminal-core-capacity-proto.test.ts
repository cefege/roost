// Validates the one protobuf boundary for worker terminal-core capacity reports.

import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { TerminalCoreCapacityReportSchema } from "../src/gen/roost/v1/wire_pb.ts";
import {
  terminalCoreCapacityReportFromProto,
  terminalCoreCapacityReportToProto,
} from "../src/terminal-core-capacity-proto.ts";

const report = {
  used: 12,
  pending: 0,
  capacity: 12,
  estimated_reserved_bytes: 480 * 1024 * 1024,
  effective_memory_ceiling_bytes: 2 * 1024 * 1024 * 1024,
  boot_rss_bytes: 256 * 1024 * 1024,
  overcommit_count: 0,
  refusal_count: 3,
} as const;

describe("terminal-core-capacity-proto", () => {
  test("round-trips a bounded capacity report", () => {
    expect(terminalCoreCapacityReportFromProto(
      terminalCoreCapacityReportToProto(report),
    )).toEqual(report);
  });

  test("rejects a count beyond the protobuf uint32 range", () => {
    const outOfRange = create(TerminalCoreCapacityReportSchema, {
      ...terminalCoreCapacityReportToProto(report),
      capacity: 0x1_0000_0000,
    });
    expect(() => terminalCoreCapacityReportFromProto(outOfRange)).toThrow();
  });

  test("rejects an unsafe protobuf counter", () => {
    const unsafe = create(TerminalCoreCapacityReportSchema, {
      ...terminalCoreCapacityReportToProto(report),
      refusalCount: BigInt(Number.MAX_SAFE_INTEGER) + 1n,
    });
    expect(() => terminalCoreCapacityReportFromProto(unsafe)).toThrow(
      "terminal core capacity refusal_count exceeds a safe integer",
    );
  });
});
