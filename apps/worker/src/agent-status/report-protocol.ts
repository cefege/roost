// Validates the one-request local agent integration protocol.
// Status and durable conversation-reference methods share capability/session
// authentication, but only the latter carries a bounded opaque value.

import { z } from "zod";
import { AgentConversationReferenceV1Schema } from "@roost/shared/agent-conversation-reference";
import {
  AGENT_STATUS_MESSAGE_MAX_LENGTH,
  AgentRuntimeState,
  SessionId,
} from "@roost/shared/wire";

export const AGENT_REPORT_MAX_LINE_BYTES = 32 * 1024;

const CapabilitySchema = z.string().regex(/^[a-f0-9]{64}$/);

export const AgentStatusReportRequestSchema = z.object({
  version: z.literal(1),
  method: z.literal("agent.report"),
  capability: CapabilitySchema,
  params: z.object({
    session_id: SessionId,
    state: AgentRuntimeState,
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
