// Terminal-core capacity report shared by worker heartbeat, coordinator projection, and CLI.
// The worker owns admission and snapshots this content-free operational shape.
// Consumers validate persisted reports before using them for fleet capacity decisions.

import { z } from "zod";

const SafeUint32 = z.number().int().nonnegative().max(0xffffffff);
const SafeUint64 = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);

export const TerminalCoreCapacityReportSchema = z.object({
  used: SafeUint32,
  pending: SafeUint32,
  capacity: SafeUint32,
  estimated_reserved_bytes: SafeUint64,
  effective_memory_ceiling_bytes: SafeUint64,
  boot_rss_bytes: SafeUint64,
  overcommit_count: SafeUint32,
  refusal_count: SafeUint64,
}).strict().readonly().superRefine((report, context) => {
  if (report.overcommit_count > 1) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["overcommit_count"],
      message: "terminal core replacement overcommit exceeds one reserved slot",
    });
  }
  if (report.used + report.pending > report.capacity + report.overcommit_count) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["used"],
      message: "terminal core use exceeds reported capacity without replacement reserve",
    });
  }
});

export type TerminalCoreCapacityReport = z.infer<
  typeof TerminalCoreCapacityReportSchema
>;
