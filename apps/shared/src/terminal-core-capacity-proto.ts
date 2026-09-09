// Converts terminal-core capacity reports between protobuf bigint fields and the
// shared operational contract. Worker heartbeat and coordinator persistence use
// this one boundary so invalid or lossy reports cannot alter admission visibility.

import { create } from "@bufbuild/protobuf";
import {
  TerminalCoreCapacityReportSchema as TerminalCoreCapacityReportProtoSchema,
  type TerminalCoreCapacityReport as TerminalCoreCapacityReportProto,
} from "./gen/roost/v1/wire_pb.ts";
import {
  TerminalCoreCapacityReportSchema,
  type TerminalCoreCapacityReport,
} from "./terminal-core-capacity.ts";

export function terminalCoreCapacityReportToProto(
  report: TerminalCoreCapacityReport,
): TerminalCoreCapacityReportProto {
  const checked = TerminalCoreCapacityReportSchema.parse(report);
  return create(TerminalCoreCapacityReportProtoSchema, {
    used: checked.used,
    pending: checked.pending,
    capacity: checked.capacity,
    estimatedReservedBytes: BigInt(checked.estimated_reserved_bytes),
    effectiveMemoryCeilingBytes: BigInt(checked.effective_memory_ceiling_bytes),
    bootRssBytes: BigInt(checked.boot_rss_bytes),
    overcommitCount: checked.overcommit_count,
    refusalCount: BigInt(checked.refusal_count),
  });
}

export function terminalCoreCapacityReportFromProto(
  report: TerminalCoreCapacityReportProto,
): TerminalCoreCapacityReport {
  return TerminalCoreCapacityReportSchema.parse({
    used: report.used,
    pending: report.pending,
    capacity: report.capacity,
    estimated_reserved_bytes: boundedNumber(
      report.estimatedReservedBytes,
      "estimated_reserved_bytes",
    ),
    effective_memory_ceiling_bytes: boundedNumber(
      report.effectiveMemoryCeilingBytes,
      "effective_memory_ceiling_bytes",
    ),
    boot_rss_bytes: boundedNumber(report.bootRssBytes, "boot_rss_bytes"),
    overcommit_count: report.overcommitCount,
    refusal_count: boundedNumber(report.refusalCount, "refusal_count"),
  });
}

function boundedNumber(value: bigint, field: string): number {
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`terminal core capacity ${field} exceeds a safe integer`);
  }
  return Number(value);
}
