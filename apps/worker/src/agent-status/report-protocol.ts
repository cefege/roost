// Validates the one-request local agent integration protocol.
// Status and durable conversation-reference methods share capability/session
// authentication, but only the latter carries a bounded opaque value.
// Reported agent state is deliberately three-valued (working|blocked|idle):
// unlike herdr's pane report API there is no `unknown` value, because
// `active: false` already withdraws a status — see REPORTED_STATE_UNKNOWN_REASON.

import { z } from "zod";
import { AgentConversationReferenceV1Schema } from "@roost/shared/agent-conversation-reference";
import {
  AGENT_STATUS_MESSAGE_MAX_LENGTH,
  AgentRuntimeState,
  SessionId,
} from "@roost/shared/wire";

export const AGENT_REPORT_MAX_LINE_BYTES = 32 * 1024;

const CapabilitySchema = z.string().regex(/^[a-f0-9]{64}$/);

/** herdr's pane report API accepts a fourth reported value, `unknown`. Roost
 * has no wire state for "no longer known": `done` is derived per viewer in the
 * browser from an acknowledged completion revision, so a fifth state would
 * roll up into an attention band nobody could ever clear. Naming the
 * withdrawal verb keeps a herdr-shaped integration from guessing. */
export const REPORTED_STATE_UNKNOWN_REASON =
  'state "unknown" is not reported; send active: false to withdraw the status';

const ReportedAgentRuntimeState = z.string()
  .superRefine((value, context) => {
    if (AgentRuntimeState.safeParse(value).success) return;
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: value === "unknown"
        ? REPORTED_STATE_UNKNOWN_REASON
        : `state must be one of ${AgentRuntimeState.options.join(", ")}`,
    });
  })
  .pipe(AgentRuntimeState);

export const AgentStatusReportRequestSchema = z.object({
  version: z.literal(1),
  method: z.literal("agent.report"),
  capability: CapabilitySchema,
  params: z.object({
    session_id: SessionId,
    state: ReportedAgentRuntimeState,
    message: z.string().max(AGENT_STATUS_MESSAGE_MAX_LENGTH).optional(),
    active: z.boolean(),
  }).strict(),
}).strict();
export type AgentStatusReportRequest = z.infer<
  typeof AgentStatusReportRequestSchema
>;

const AgentReferenceReportValueSchema = z.object({
  kind: z.enum(["id", "path"]),
  value: z.string(),
}).strict().transform((value, context) => {
  const parsed = AgentConversationReferenceV1Schema.safeParse({
    schema_version: 1,
    agent_id: "omp",
    kind: value.kind,
    value: value.value,
  });
  if (!parsed.success) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      message: "invalid agent conversation reference",
    });
    return z.NEVER;
  }
  return parsed.data;
});

export const AgentReferenceReportRequestSchema = z.object({
  version: z.literal(1),
  method: z.literal("agent.reference"),
  capability: CapabilitySchema,
  params: z.object({
    session_id: SessionId,
    reference: AgentReferenceReportValueSchema.nullable(),
  }).strict(),
}).strict();
export type AgentReferenceReportRequest = z.infer<
  typeof AgentReferenceReportRequestSchema
>;

export const AgentIntegrationRequestSchema = z.discriminatedUnion("method", [
  AgentStatusReportRequestSchema,
  AgentReferenceReportRequestSchema,
]);
export type AgentIntegrationRequest = z.infer<
  typeof AgentIntegrationRequestSchema
>;
