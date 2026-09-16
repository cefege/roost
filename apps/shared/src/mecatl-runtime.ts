// Worker-reported state of the Mecatl daemon supervised on one machine.
// Emitted by the worker heartbeat, persisted by the coordinator against that
// worker row, and read by `roost status`. It is operator-facing diagnosis
// only: the browser learns availability from the relay response instead.

import { z } from "zod";

export const MecatlUnavailableReasonSchema = z.enum([
  "disabled",
  "not_ready",
  "binary_missing",
  "spawn_failed",
  "readiness_timeout",
  "incompatible_api",
  "daemon_exit",
  "restart_exhausted",
  "stopped",
]);

/** The single definition of why a machine offers no agent surface. The worker
 *  supervisor, the heartbeat projection, and the browser pane all read this
 *  one list, so a new reason cannot reach an operator through one path and
 *  fall through as a generic failure on another. */
export type MecatlUnavailableReason = z.infer<typeof MecatlUnavailableReasonSchema>;

export const MecatlRuntimeReportSchema = z.object({
  state: z.enum(["disabled", "starting", "ready", "unavailable"]),
  /** Present exactly when `state` is `unavailable`, so a status line can say
   *  WHY a machine offers no agent surface instead of only that it does not. */
  reason: MecatlUnavailableReasonSchema.optional(),
});

export type MecatlRuntimeReport = z.infer<typeof MecatlRuntimeReportSchema>;
